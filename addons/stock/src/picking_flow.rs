// SPDX-License-Identifier: MIT OR Apache-2.0
//! The transfer's own rules: what a new transfer takes from its operation type (reference from the
//! type's sequence, source, destination, company), cancellation and deletion.
//!
//! Observed in the reference (722-725,
//! 800-808`): a new transfer without a reference
//! draws the next number of its operation type's sequence; its locations follow the type's defaults;
//! Cancel cancels the open moves (their reservations are given back), locks the transfer and marks a
//! transfer without moves cancelled; deleting a transfer cancels and deletes its moves first, and a
//! done move cannot be deleted. Adopted: those rules. Modified: one sequence read for all types of a
//! create, the type defaults also on `create` for callers that skip the form; cancellation gives
//! every reservation of the call back in one engine call after the transfers and moves are locked;
//! a done transfer is refused by name on cancel (the reference refuses it through its moves); a
//! transfer without moves cannot be cancelled (its state follows from its moves, so an
//! empty transfer is a draft) and is refused by name: delete it instead.
//!
//! Not here yet: cancel propagation along move chains (refused by name), the resequence on an
//! operation type change, batches, the zero-demand and immediate-transfer dialogs.

use std::collections::{BTreeSet, HashMap};

use erp_sdk::prelude::*;
use erp_sdk::sequence::draw_by_id;
use erp_sdk::{Load, Operation};

use crate::error::StockError;
use crate::move_counters::{cell_id, lock_move_roots, lock_pickings, notify_pickings};
use crate::move_lines::{DeferSettle, LINE_DERIVED_MOVE_FIELDS, reservation_changes, sync_moves};
use crate::picking::StockPicking;
use crate::quant_engine::{QuantChange, apply_quant_changes_in_chunks};

/// What a new transfer takes from its operation type, read as the caller.
const TYPE_DEFAULTS: [&str; 4] = [
    "sequence_id",
    "default_location_src_id",
    "default_location_dest_id",
    "company_id",
];

/// The transfers of `$1` with their state and move count.
const READ_PICKINGS: &str =
    "SELECT id, state, move_count FROM stock_picking WHERE id = ANY($1) ORDER BY id";

/// The moves of the transfers `$1` (scrap included), and whether they are open.
const PICKING_MOVES: &str = "SELECT id, state NOT IN ('done', 'cancel') FROM stock_move \
     WHERE picking_id = ANY($1) ORDER BY id";

/// Moves of `$1` that an open move takes from.
const FEEDS_OPEN: &str = "SELECT count(*) FROM stock_move_move_rel r \
     JOIN stock_move d ON d.id = r.move_dest_id \
     WHERE r.move_orig_id = ANY($1) AND d.state NOT IN ('done', 'cancel')";

/// The open, active lines of the moves `$1`.
const OPEN_LINES: &str = "SELECT id FROM stock_move_line \
     WHERE move_id = ANY($1) AND membership = 'open' AND active ORDER BY id";

/// A cancelled move is no longer picked and takes from stock.
const MOVES_CANCEL: &str = "UPDATE stock_move SET state = 'cancel', picked = false, \
       procure_method = 'make_to_stock', write_uid = $2, write_date = (now() AT TIME ZONE 'UTC') \
     WHERE id = ANY($1)";
const PICKINGS_LOCK: &str = "UPDATE stock_picking SET is_locked = true, write_uid = $2, \
       write_date = (now() AT TIME ZONE 'UTC') WHERE id = ANY($1)";
/// The audit columns the two statements above stamp.
const AUDIT: [&str; 2] = ["write_uid", "write_date"];

fn decode(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("A transfer or its operation type could not be read.").with_source(e)
}

fn unnamed(vals: &RawVals) -> bool {
    match vals.get("name") {
        None | Some(Cell::Null) | Some(Cell::Bool(false)) => true,
        Some(Cell::Str(name)) => name.is_empty() || &**name == "/",
        Some(_) => false,
    }
}

/// Fill each new transfer from its operation type (in the values or as the context default): the
/// source, destination and company when not given, and the reference from the type's sequence when
/// the transfer has none. The types are read as the caller, after the caller's
/// create right on transfers was checked, so no number is drawn for a refused create or a type the
/// caller cannot see. One read for all types of the call, one draw per unnamed transfer.
///
/// # Errors
///
/// The read's access errors; a refusal naming the cause for an unnamed transfer whose type has no
/// numbering (the second one would only meet the per-company reference key); the sequence draw's
/// errors (read access on sequences).
pub(crate) async fn fill_from_types(
    records: &Records,
    tx: &mut Transaction,
    vals_list: &mut [RawVals],
) -> Result<()> {
    let context = records.env().context();
    let default_type = match context.get("default_picking_type_id") {
        Some(Value::Int(id)) => Some(id),
        _ => None,
    };
    // a reference given as the context default wins, as in the reference
    let default_named = matches!(
        context.get("default_name"),
        Some(Value::Str(name)) if !name.is_empty() && &*name != "/"
    );
    let type_of = |vals: &RawVals| match vals.get("picking_type_id") {
        Some(cell) => cell_id(cell),
        None => default_type,
    };
    let types: Vec<i64> = vals_list
        .iter()
        .filter_map(type_of)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if types.is_empty() {
        return Ok(());
    }
    let id_of = |value: Option<&Value>| match value {
        Some(Value::Int(id)) => Some(*id),
        _ => None,
    };
    let mut rows: HashMap<i64, [Option<i64>; 4]> = HashMap::new();
    for row in records
        .env()
        .records("stock.picking.type")?
        .browse(types)?
        .read_with(tx, &TYPE_DEFAULTS, Load::Raw)
        .await?
    {
        let id = id_of(row.get("id"))
            .ok_or_else(|| Error::internal("An operation type was read without its id."))?;
        rows.insert(id, TYPE_DEFAULTS.map(|field| id_of(row.get(field))));
    }
    for vals in vals_list.iter_mut() {
        // a missing type is left to the foreign key
        let Some([sequence, source, dest, company]) = type_of(vals).and_then(|id| rows.get(&id))
        else {
            continue;
        };
        let (sequence, defaults) = (*sequence, [*source, *dest, *company]);
        for (field, value) in ["location_id", "location_dest_id", "company_id"]
            .into_iter()
            .zip(defaults)
        {
            if let Some(id) = value
                && !vals.contains(field)
                && !context.contains(&format!("default_{field}"))
            {
                vals.insert(field.into(), Cell::Id(id));
            }
        }
        if unnamed(vals) && !default_named {
            let Some(sequence) = sequence else {
                return Err(Error::user(
                    "This operation type has no numbering for its transfers: set its Sequence \
                     Prefix, or enter the transfer's reference.",
                ));
            };
            let name = draw_by_id(tx, records.env(), sequence, None).await?;
            vals.insert("name".into(), Cell::Str(name.into()));
        }
    }
    Ok(())
}

/// The moves' transfers are locked first, then the moves.
///
/// # Errors
///
/// [`StockError::CancelChained`] for a move an open move takes from; the engine's and the line
/// deletion's errors.
pub(crate) async fn cancel_moves(
    records: &Records,
    tx: &mut Transaction,
    moves: &[i64],
) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    tx.flush().await?;
    let roots = lock_move_roots(tx, moves, &[]).await?;
    let feeds: i64 = tx
        .query(FEEDS_OPEN, &[&moves])
        .await?
        .first()
        .ok_or_else(|| Error::internal("A count returned no row."))?
        .try_get(0)
        .map_err(decode)?;
    if feeds > 0 {
        return Err(StockError::CancelChained.into());
    }
    let mut lines = Vec::new();
    for row in tx.query(OPEN_LINES, &[&moves]).await? {
        lines.push(row.try_get::<_, i64>(0).map_err(decode)?);
    }
    let mut released = HashMap::new();
    reservation_changes(tx, &lines, Decimal::NEGATIVE_ONE, &mut released).await?;
    let changes: Vec<QuantChange> = released
        .into_iter()
        .filter(|(_, reserved)| !reserved.is_zero())
        .map(|(key, reserved)| QuantChange::new(key, Decimal::ZERO, reserved))
        .collect();
    apply_quant_changes_in_chunks(tx, &changes).await?;
    let env = records.env();
    if !lines.is_empty() {
        let lines = env.records("stock.move.line")?.browse(lines)?;
        tx.extension::<DeferSettle>().active = true;
        let result = lines.unlink(tx).await;
        tx.extension::<DeferSettle>().active = false;
        result?;
    }
    sync_moves(tx, moves).await?;
    tx.execute(MOVES_CANCEL, &[&moves, &env.uid()]).await?;
    tx.invalidate_model("stock.move", Some(&AUDIT)).await?;
    env.records("stock.move")?
        .browse(moves.iter().copied())?
        .modified_external(
            tx,
            &[&LINE_DERIVED_MOVE_FIELDS[..], &["picked", "procure_method"]].concat(),
        )
        .await?;
    notify_pickings(records, tx, &roots).await
}

/// The moves of the transfers `pickings`: `(all, open)`, ascending.
async fn picking_moves(tx: &mut Transaction, pickings: &[i64]) -> Result<(Vec<i64>, Vec<i64>)> {
    let (mut all, mut open) = (Vec::new(), Vec::new());
    for row in tx.query(PICKING_MOVES, &[&pickings]).await? {
        let id: i64 = row.try_get(0).map_err(decode)?;
        if row.try_get::<_, bool>(1).map_err(decode)? {
            open.push(id);
        }
        all.push(id);
    }
    Ok((all, open))
}

/// Before a transfer is deleted: the transfers are locked first (
/// as validation and cancel do), then their open moves are cancelled and all their moves are
/// deleted through the move model as the caller (which refuses done ones). The caller checked
/// the unlink access and refused done transfers.
///
/// # Errors
///
/// The cancellation's and the move deletion's errors.
pub(crate) async fn delete_moves_of(records: &Records, tx: &mut Transaction) -> Result<()> {
    tx.flush().await?;
    let ids = records.ids();
    lock_pickings(tx, &ids).await?;
    let (all, open) = picking_moves(tx, &ids).await?;
    cancel_moves(records, tx, &open).await?;
    if all.is_empty() {
        return Ok(());
    }
    records
        .env()
        .records("stock.move")?
        .browse(all)?
        .unlink(tx)
        .await
}

#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    /// Done transfers and transfers without moves are
    /// refused by name; cancelled ones are left as they are.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_cancel(this: &RecordSet<StockPicking>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        records.check_access(tx, Operation::Write).await?;
        tx.flush().await?;
        let ids = records.ids();
        if ids.is_empty() {
            return Ok(());
        }
        lock_pickings(tx, &ids).await?;
        for row in tx.query(READ_PICKINGS, &[&ids]).await? {
            let state: String = row.try_get(1).map_err(decode)?;
            let moves: i32 = row.try_get(2).map_err(decode)?;
            if state == "done" {
                return Err(StockError::CancelDone.into());
            }
            if moves == 0 {
                return Err(StockError::CancelEmpty.into());
            }
        }
        let before = crate::state_history::before(tx, records).await?;
        let (_, open) = picking_moves(tx, &ids).await?;
        cancel_moves(records, tx, &open).await?;
        tx.execute(PICKINGS_LOCK, &[&ids, &records.env().uid()])
            .await?;
        tx.invalidate_model("stock.picking", Some(&AUDIT)).await?;
        records.modified_external(tx, &["is_locked"]).await?;
        crate::state_history::report(tx, records, before).await
    }

    /// The operation type proposes the transfer's source, destination and company (the reference's
    /// location computes on the type).
    #[onchange("picking_type_id")]
    async fn _onchange_picking_type_locations(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<OnchangeResult> {
        for picking in this.as_records().iter() {
            let kind = picking.related(tx, "picking_type_id").await?;
            if kind.is_empty() {
                continue;
            }
            let mut vals = RawVals::new();
            for (field, source) in [
                ("location_id", "default_location_src_id"),
                ("location_dest_id", "default_location_dest_id"),
                ("company_id", "company_id"),
            ] {
                let cell = kind.cell(tx, source).await?;
                if !matches!(cell, Cell::Null | Cell::Bool(false)) {
                    vals = vals.set_cell(field, cell);
                }
            }
            picking.write_base(tx, vals).await?;
        }
        Ok(OnchangeResult::none())
    }
}
