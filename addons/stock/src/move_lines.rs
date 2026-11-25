// SPDX-License-Identifier: MIT OR Apache-2.0
//! Open move lines (`stock.move.line`): create, write and unlink of the lines of a move, with the
//! reservation each open line holds on its quant key and the move columns that follow from the
//! lines (1, D2.2 I3, D2.5;.6, D3.1).
//!
//! Observed in the reference (471-497,
//! 562-583`;, 2534-2556`): a new line takes its
//! company and picked flag from its move, a line with a quantity on a non-done move raises the
//! reserved quantity of its quant key, a key or quantity edit releases the old reservation and
//! takes the new one, deleting an open line gives its reservation back, the move's quantity is the
//! sum of its lines converted to the move's unit, and the move's state is recomputed from quantity
//! against demand. Adopted: those rules. Modified: every call works on the whole batch with a fixed
//! number of statements: one read of the moves, the ORM insert or write, one netted quant engine
//! call per unit of work, one statement that sets the moves' quantities and one that recomputes
//! their states; the line also takes the move's transfer, product, locations and unit (the
//! reference does so in its views), and its product-unit snapshot from the unit service as moves do
//! ([`crate::move_units`]).
//!
//! Lock order (1): the transfers of the lines' moves, then the moves, in
//! ascending id order ([`lock_move_roots`]), then the quant keys in key order (the engine), so a
//! line writer never waits on a move writer in the opposite order. The move columns change in raw
//! statements after the engine call, under locks already held; the transfer counters follow from
//! the move counter triggers in those statements.
//!
//! Not here yet, each refused by name where it would matter: lots and serial rows,
//! a line without a move (the transfer creating one), lines of a done or cancelled move and
//! corrections of done lines. The done transition ([`crate::done`]) reuses
//! [`reservation_changes`] and [`sync_moves`].

use std::collections::{BTreeSet, HashMap};

use erp_addon_uom::stock::product_quantity_sql;
use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::error::StockError;
use crate::move_counters::{cell_id, lock_move_roots, notify_pickings};
use crate::move_units::{
    LINE_SNAPSHOT_FIELDS, LINE_UNIT_INPUTS, Target, fill_create_for, write_groups_for,
};
use crate::quant_engine::{QuantChange, QuantKey, apply_quant_changes_in_chunks};

/// Line columns whose change moves the line's reservation (its quantity or its quant key).
const RESERVATION_FIELDS: [&str; 6] = [
    "quantity",
    "uom_id",
    "location_id",
    "lot_id",
    "package_id",
    "owner_id",
];

/// Line columns that follow the line's move and are fixed once the line exists.
const RELINK_FIELDS: [&str; 4] = ["move_id", "picking_id", "company_id", "product_id"];

/// The move columns the line statements write.
pub(crate) const LINE_DERIVED_MOVE_FIELDS: [&str; 4] = [
    "quantity",
    "quantity_product_uom",
    "open_line_version",
    "state",
];

/// The moves of `stock.move` the line statements read, one row each.
const READ_MOVES: &str = "SELECT id, picking_id, company_id, product_id, location_id, \
     location_dest_id, uom_id, picked, state FROM stock_move WHERE id = ANY($1)";

/// Distinct moves of the lines `$1`.
const LINE_MOVES: &str = "SELECT DISTINCT move_id FROM stock_move_line WHERE id = ANY($1)";

/// Lines of `$1` whose transfer is not their move's.
const OTHER_TRANSFER: &str = "SELECT count(*) FROM stock_move_line l \
     JOIN stock_move m ON m.id = l.move_id \
     WHERE l.id = ANY($1) AND l.picking_id IS DISTINCT FROM m.picking_id";

/// Lines of `$1` that are in the ledger.
const DONE_LINES: &str =
    "SELECT count(*) FROM stock_move_line WHERE id = ANY($1) AND membership = 'done'";

/// The reservation of each open, active line of `$1` with a quantity, for a storable product: its
/// quant key and its quantity in the product unit (5).
const LINE_RESERVATIONS: &str = "SELECT l.company_id, l.product_id, l.location_id, l.lot_id, \
     l.package_id, l.owner_id, l.quantity_product_uom \
     FROM stock_move_line l \
     JOIN product_product pp ON pp.id = l.product_id \
     JOIN product_template pt ON pt.id = pp.product_tmpl_id \
     WHERE l.id = ANY($1) AND l.membership = 'open' AND l.active AND pt.is_storable \
       AND l.quantity_product_uom <> 0";

/// Recompute the state of the moves `$1` from their quantity and demand, as the reference does
/// after a line change: done and cancelled moves and drafts without a
/// quantity keep theirs; a covered demand is `assigned`, a partly covered one
/// `partially_available`; an uncovered make-to-order move without origins, or one with an open
/// origin that has a demand, is `waiting`; otherwise `confirmed`. Only changed rows are written,
/// so the counter triggers see only real transitions.
const RECOMPUTE_STATES: &str = "UPDATE stock_move m SET state = s.state FROM ( \
     SELECT m.id, CASE \
       WHEN m.quantity >= m.product_uom_qty THEN 'assigned' \
       WHEN m.quantity > 0 THEN 'partially_available' \
       WHEN (m.procure_method = 'make_to_order' AND NOT EXISTS ( \
               SELECT 1 FROM stock_move_move_rel r WHERE r.move_dest_id = m.id)) \
         OR EXISTS ( \
               SELECT 1 FROM stock_move_move_rel r JOIN stock_move o ON o.id = r.move_orig_id \
               WHERE r.move_dest_id = m.id AND o.product_uom_qty > 0 \
                 AND o.state NOT IN ('done', 'cancel')) THEN 'waiting' \
       ELSE 'confirmed' END AS state \
     FROM stock_move m \
     WHERE m.id = ANY($1) AND m.state NOT IN ('done', 'cancel') \
       AND NOT (m.state = 'draft' AND m.quantity = 0)) s \
     WHERE m.id = s.id AND m.state <> s.state";

/// One move as the line statements read it.
struct MoveRow {
    picking_id: Option<i64>,
    company_id: i64,
    product_id: i64,
    location_id: i64,
    location_dest_id: i64,
    uom_id: i64,
    picked: bool,
    state: String,
}

fn decode(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("A stock move or move line could not be read.").with_source(e)
}

async fn read_moves(tx: &mut Transaction, ids: &[i64]) -> Result<HashMap<i64, MoveRow>> {
    let mut out = HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    for row in tx.query(READ_MOVES, &[&ids]).await? {
        out.insert(
            row.try_get(0).map_err(decode)?,
            MoveRow {
                picking_id: row.try_get(1).map_err(decode)?,
                company_id: row.try_get(2).map_err(decode)?,
                product_id: row.try_get(3).map_err(decode)?,
                location_id: row.try_get(4).map_err(decode)?,
                location_dest_id: row.try_get(5).map_err(decode)?,
                uom_id: row.try_get(6).map_err(decode)?,
                picked: row.try_get(7).map_err(decode)?,
                state: row.try_get(8).map_err(decode)?,
            },
        );
    }
    Ok(out)
}

async fn line_moves(tx: &mut Transaction, lines: &[i64]) -> Result<Vec<i64>> {
    let mut out = Vec::new();
    for row in tx.query(LINE_MOVES, &[&lines]).await? {
        out.push(row.try_get(0).map_err(decode)?);
    }
    out.sort_unstable();
    Ok(out)
}

fn label(tx: &Transaction, field: &str) -> Result<String> {
    tx.registry()
        .model("stock.move.line")
        .and_then(|spec| spec.field(field))
        .map(|spec| spec.string.clone())
        .ok_or_else(|| Error::internal(format!("stock.move.line has no field '{field}'")))
}

/// Serial child-row mode remains reserved for a later slice; plain lines support `lot_id` and `lot_name`.
fn validate_lot_vals(vals: &RawVals) -> Result<()> {
    if matches!(vals.get("child_mode"), Some(Cell::Bool(true))) {
        return Err(StockError::LotsNotAvailable.into());
    }
    let has_lot_id = matches!(vals.get("lot_id"), Some(Cell::Id(_)) | Some(Cell::Int(_)));
    let has_lot_name = matches!(vals.get("lot_name"), Some(Cell::Str(s)) if !s.trim().is_empty());
    if has_lot_id && has_lot_name {
        return Err(Error::validation(
            "Specify either an existing Lot/Serial Number or a new Lot/Serial Number Name, not both.",
        ));
    }
    Ok(())
}

/// Refuse `default_child_mode=true` in context; `default_lot_id` and `default_lot_name` are allowed.
fn validate_lot_defaults(env: &Env) -> Result<()> {
    if matches!(
        env.context().get("default_child_mode"),
        Some(Value::Bool(true))
    ) {
        return Err(StockError::LotsNotAvailable.into());
    }
    Ok(())
}

async fn refuse_done(tx: &mut Transaction, lines: &[i64]) -> Result<()> {
    let rows = tx.query(DONE_LINES, &[&lines]).await?;
    let done: i64 = rows
        .first()
        .ok_or_else(|| Error::internal("A count returned no row."))?
        .try_get(0)
        .map_err(decode)?;
    if done > 0 {
        return Err(StockError::DoneLineChange.into());
    }
    Ok(())
}

/// The reservations of the lines `lines` as signed quant changes (`sign` 1 to take them, -1 to
/// give them back), netted per key with zero nets dropped. Keys at reservation-bypass locations are
/// dropped by the engine (1); non-storable products hold none.
///
/// # Errors
///
/// The database error of the read.
pub(crate) async fn reservation_changes(
    tx: &mut Transaction,
    lines: &[i64],
    sign: Decimal,
    into: &mut HashMap<QuantKey, Decimal>,
) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    for row in tx.query(LINE_RESERVATIONS, &[&lines]).await? {
        let key = QuantKey::new(
            row.try_get(0).map_err(decode)?,
            row.try_get(1).map_err(decode)?,
            row.try_get(2).map_err(decode)?,
        )
        .lot_id(row.try_get(3).map_err(decode)?)
        .package_id(row.try_get(4).map_err(decode)?)
        .owner_id(row.try_get(5).map_err(decode)?);
        let quantity: Decimal = row.try_get(6).map_err(decode)?;
        *into.entry(key).or_default() += sign * quantity;
    }
    Ok(())
}

/// Apply netted reserved deltas (nothing when every net is zero), in one engine call or, above its
/// bound, in ascending chunks; `probed` marks them as an allocation chosen without a lock (
/// D2), floored at the on-hand.
pub(crate) async fn apply_reserved(
    tx: &mut Transaction,
    nets: HashMap<QuantKey, Decimal>,
    probed: bool,
) -> Result<()> {
    let changes: Vec<QuantChange> = nets
        .into_iter()
        .filter(|(_, delta)| !delta.is_zero())
        .map(|(key, delta)| {
            let change = QuantChange::new(key, Decimal::ZERO, delta);
            if probed { change.as_probed() } else { change }
        })
        .collect();
    apply_quant_changes_in_chunks(tx, &changes).await.map(drop)
}

/// While a reservation unit inserts its lines through the ORM, a validation or cancellation deletes
/// them, or a line write of several snapshot groups writes each group, the line create, write and
/// unlink leave the reservation and the move settlement to that unit, which makes one engine call
/// for all of them.
#[derive(Default)]
pub(crate) struct DeferSettle {
    pub(crate) active: bool,
}

/// The statement that sets each move of `$1` to the sum of its active lines converted to the move's
/// unit (unrounded per line, then at the move's Product Unit digits), its
/// product-unit quantity by the unit service's SQL form (so the table check holds), and bumps its
/// open-line version.
fn sync_sql() -> Result<String> {
    let product_quantity = product_quantity_sql("q", "uf", "puf", "d").map_err(|e| {
        Error::internal("The product-unit conversion of a move could not be built.").with_source(e)
    })?;
    Ok(format!(
        "WITH s AS ( \
           SELECT m.id, \
             round(COALESCE(sum(l.quantity * l.uom_factor / m.uom_factor), 0), \
                   m.product_unit_digits) AS q, \
             m.uom_factor AS uf, m.product_uom_factor AS puf, m.product_unit_digits AS d \
           FROM stock_move m LEFT JOIN stock_move_line l ON l.move_id = m.id AND l.active \
           WHERE m.id = ANY($1) GROUP BY m.id) \
         UPDATE stock_move m SET quantity = s.q, quantity_product_uom = {product_quantity}, \
           open_line_version = m.open_line_version + 1 \
         FROM s WHERE m.id = s.id"
    ))
}

/// Set the quantities of `moves` from their lines (see [`sync_sql`]). The moves must be locked.
///
/// # Errors
///
/// The database error of the statement.
pub(crate) async fn sync_moves(tx: &mut Transaction, moves: &[i64]) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    tx.execute(&sync_sql()?, &[&moves]).await.map(drop)
}

/// Recompute the states of `moves` (see [`RECOMPUTE_STATES`]). The moves and their transfers must be
/// locked; the counter triggers keep the transfers.
///
/// # Errors
///
/// The database error of the statement.
pub(crate) async fn recompute_states(tx: &mut Transaction, moves: &[i64]) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    tx.execute(RECOMPUTE_STATES, &[&moves]).await.map(drop)
}

/// After the lines of `moves` changed (flushed) and the reservation nets are known: one engine call,
/// the move quantities and states, and the ORM notification of the moves and their transfers.
pub(crate) async fn settle(
    records: &Records,
    tx: &mut Transaction,
    moves: &[i64],
    roots: &[i64],
    nets: HashMap<QuantKey, Decimal>,
    probed: bool,
) -> Result<()> {
    apply_reserved(tx, nets, probed).await?;
    sync_moves(tx, moves).await?;
    recompute_states(tx, moves).await?;
    let moves_rs = records
        .env()
        .records("stock.move")?
        .browse(moves.iter().copied())?;
    moves_rs
        .modified_external(tx, &LINE_DERIVED_MOVE_FIELDS)
        .await?;
    notify_pickings(records, tx, roots).await
}

/// Create lines: the move's transfer, company, product, locations, unit and picked flag are taken
/// when not given, the product-unit snapshot is stamped, the moves are locked before the insert,
/// and the reservations, move quantities and states follow.
pub(crate) async fn create_lines(
    records: &Records,
    tx: &mut Transaction,
    mut vals_list: Vec<RawVals>,
    sup: Super<(Vec<RawVals>,), Records>,
) -> Result<Records> {
    let default_move = match records.env().context().get("default_move_id") {
        Some(Value::Int(id)) => Some(id),
        _ => None,
    };
    if vals_list.is_empty() {
        return sup.call(tx, records, (vals_list,)).await;
    }
    // a caller without the right gets the access error before any refusal of the values
    records.check_access(tx, Operation::Create).await?;
    validate_lot_defaults(records.env())?;
    let mut moves = BTreeSet::new();
    for vals in &vals_list {
        validate_lot_vals(vals)?;
        let move_id = match vals.get("move_id") {
            Some(cell) => cell_id(cell),
            None => default_move,
        };
        moves.insert(move_id.ok_or(StockError::LineNeedsMove)?);
    }
    tx.flush().await?;
    let moves: Vec<i64> = moves.into_iter().collect();
    // the caller-named moves pass the record check before the statements below lock and read them
    records
        .env()
        .records("stock.move")?
        .browse(moves.iter().copied())?
        .check_access(tx, Operation::Read)
        .await?;
    let roots = lock_move_roots(tx, &moves, &[]).await?;
    let rows = read_moves(tx, &moves).await?;
    for vals in &mut vals_list {
        let move_id = match vals.get("move_id") {
            Some(cell) => cell_id(cell),
            None => default_move,
        };
        // a missing move is left to the foreign key
        let Some(row) = move_id.and_then(|id| rows.get(&id)) else {
            continue;
        };
        if matches!(row.state.as_str(), "done" | "cancel") {
            return Err(StockError::LineMoveClosed.into());
        }
        let resolved_dest = if !vals.contains("location_dest_id") {
            let qty_hint = match vals.get("quantity") {
                Some(Cell::Decimal(d)) => *d,
                _ => Decimal::ZERO,
            };
            crate::putaway::resolve_putaway_location(
                tx,
                row.company_id,
                row.product_id,
                row.location_dest_id,
                qty_hint,
            )
            .await?
        } else {
            row.location_dest_id
        };
        let defaults = [
            ("move_id", move_id.map(Cell::Id)),
            ("picking_id", row.picking_id.map(Cell::Id)),
            ("company_id", Some(Cell::Id(row.company_id))),
            ("product_id", Some(Cell::Id(row.product_id))),
            ("location_id", Some(Cell::Id(row.location_id))),
            ("location_dest_id", Some(Cell::Id(resolved_dest))),
            ("uom_id", Some(Cell::Id(row.uom_id))),
            ("picked", Some(Cell::Bool(row.picked))),
        ];
        for (field, cell) in defaults {
            if let Some(cell) = cell
                && !vals.contains(field)
            {
                vals.insert(field.into(), cell);
            }
        }
    }
    fill_create_for(Target::Line, records, tx, &mut vals_list).await?;
    let created = sup.call(tx, records, (vals_list,)).await?;
    tx.flush().await?;
    // checked after the insert, so the company checks of a given transfer speak first
    let rows = tx.query(OTHER_TRANSFER, &[&created.ids()]).await?;
    let other: i64 = rows
        .first()
        .ok_or_else(|| Error::internal("A count returned no row."))?
        .try_get(0)
        .map_err(decode)?;
    if other > 0 {
        return Err(StockError::LineRelink {
            field: label(tx, "picking_id")?,
        }
        .into());
    }
    if tx.extension::<DeferSettle>().active {
        return Ok(created);
    }
    let mut nets = HashMap::new();
    reservation_changes(tx, &created.ids(), Decimal::ONE, &mut nets).await?;
    settle(&created, tx, &moves, &roots, nets, false).await?;
    Ok(created)
}

/// Write lines: the move, transfer, company and product are fixed; a done line is not changed here;
/// a write naming the quantity, the unit or a key part moves the reservation and resettles the
/// moves; a write naming the unit or the quantity restamps the product-unit snapshot.
pub(crate) async fn write_lines(
    records: &Records,
    tx: &mut Transaction,
    vals: RawVals,
    sup: Super<(RawVals,), ()>,
) -> Result<()> {
    if records.is_empty() {
        return sup.call(tx, records, (vals,)).await;
    }
    // a caller without the right gets the access error before any refusal of the values
    records.check_access(tx, Operation::Write).await?;
    validate_lot_vals(&vals)?;
    if let Some(field) = RELINK_FIELDS.iter().find(|f| vals.contains(f)) {
        return Err(StockError::LineRelink {
            field: label(tx, field)?,
        }
        .into());
    }
    tx.flush().await?;
    let ids = records.ids();
    refuse_done(tx, &ids).await?;
    let unit = LINE_UNIT_INPUTS.iter().any(|f| vals.contains(f))
        || LINE_SNAPSHOT_FIELDS.iter().any(|f| vals.contains(f));
    if !unit && !RESERVATION_FIELDS.iter().any(|f| vals.contains(f)) {
        return sup.call(tx, records, (vals,)).await;
    }
    // a group write of a multi-group write below: the outer write holds the locks, gave the
    // reservations back and takes them again in its one engine call
    let deferred = tx.extension::<DeferSettle>().active;
    let moves = line_moves(tx, &ids).await?;
    let mut nets = HashMap::new();
    let roots = if deferred {
        Vec::new()
    } else {
        let roots = lock_move_roots(tx, &moves, &[]).await?;
        reservation_changes(tx, &ids, Decimal::NEGATIVE_ONE, &mut nets).await?;
        roots
    };
    let vals = if unit {
        let mut groups = write_groups_for(Target::Line, records, tx, &vals).await?;
        if groups.len() > 1 {
            // lines needing different snapshots are written one group at a time (each its own
            // statement), with the reservation and the settlement left to this write: one engine
            // call for all groups
            tx.extension::<DeferSettle>().active = true;
            let mut result = Ok(());
            for (group, group_vals) in groups {
                result = match records.browse(group) {
                    Ok(group) => group.write(tx, group_vals).await,
                    Err(error) => Err(error),
                };
                if result.is_err() {
                    break;
                }
            }
            tx.extension::<DeferSettle>().active = deferred;
            result?;
            tx.flush().await?;
            if deferred {
                return Ok(());
            }
            reservation_changes(tx, &ids, Decimal::ONE, &mut nets).await?;
            return settle(records, tx, &moves, &roots, nets, false).await;
        }
        match groups.pop() {
            Some((_, group_vals)) => group_vals,
            None => vals,
        }
    } else {
        vals
    };
    sup.call(tx, records, (vals,)).await?;
    tx.flush().await?;
    if deferred {
        return Ok(());
    }
    reservation_changes(tx, &ids, Decimal::ONE, &mut nets).await?;
    settle(records, tx, &moves, &roots, nets, false).await
}

/// Delete open lines: their reservations are given back and their moves resettled. Done lines are
/// in the ledger and are not deleted.
pub(crate) async fn unlink_lines(
    records: &Records,
    tx: &mut Transaction,
    sup: Super<(), ()>,
) -> Result<()> {
    if records.is_empty() {
        return sup.call(tx, records, ()).await;
    }
    records.check_access(tx, Operation::Unlink).await?;
    tx.flush().await?;
    let ids = records.ids();
    refuse_done(tx, &ids).await?;
    // the deferring unit (validation, cancellation) holds the locks and gives the reservations
    // back in its own single engine call
    if tx.extension::<DeferSettle>().active {
        sup.call(tx, records, ()).await?;
        return tx.flush().await;
    }
    let moves = line_moves(tx, &ids).await?;
    let roots = lock_move_roots(tx, &moves, &[]).await?;
    let mut nets = HashMap::new();
    reservation_changes(tx, &ids, Decimal::NEGATIVE_ONE, &mut nets).await?;
    sup.call(tx, records, ()).await?;
    tx.flush().await?;
    settle(records, tx, &moves, &roots, nets, false).await
}
