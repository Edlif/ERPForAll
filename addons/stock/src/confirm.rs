// SPDX-License-Identifier: MIT OR Apache-2.0
//! Make-to-stock confirmation of moves and transfers, with the early reservation it triggers, and
//! the defaults a new move takes from its transfer.
//!
//! Observed in the reference (320-328, 1773-1866`,
//! ): a move added to a transfer takes the
//! transfer's source, destination and operation type; confirming a draft move makes it `waiting`
//! when it has origin moves and `confirmed` otherwise, stamps today's reservation date when its
//! operation type reserves at confirmation, and reserves at once the moves that bypass reservation,
//! whose type reserves at confirmation, whose reservation date has come, or that return earlier
//! moves. Check availability confirms draft transfers first. Adopted: those rules. Modified: one
//! statement confirms every draft move of the call; the early reservation is one reservation call
//! for all of them; a transfer without a draft move to confirm is refused by name (the reference
//! does nothing).
//!
//! Not here yet, each refused by name or listed: make-to-order and make-to-stock-else-order moves
//! need the procurement engine (refused); merging moves of the same key and
//! negative demands turned into returns; assigning transfers to moves without one; the zero-demand
//! confirmation dialog; the replenishment trigger after confirmation.

use std::collections::{BTreeSet, HashMap};

use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::error::StockError;
use crate::move_counters::{cell_id, lock_move_roots, notify_pickings};
use crate::picking::StockPicking;
use crate::reservation::{assign_moves, open_moves};
use crate::stock_move::StockMove;

/// What a new move takes from its transfer when it does not name it.
const PICKING_DEFAULTS: &str = "SELECT id, company_id, location_id, location_dest_id, \
       picking_type_id, partner_id FROM stock_picking WHERE id = ANY($1)";

/// Draft moves of `$1` that need a procurement to confirm: make-to-order without origins, or
/// created by a rule that falls back to an order.
const NEEDS_PROCUREMENT: &str = "SELECT count(*) FROM stock_move m \
     LEFT JOIN stock_rule r ON r.id = m.rule_id \
     WHERE m.id = ANY($1) AND m.state = 'draft' \
       AND NOT EXISTS (SELECT 1 FROM stock_move_move_rel o WHERE o.move_dest_id = m.id) \
       AND (m.procure_method = 'make_to_order' OR r.procure_method = 'mts_else_mto')";

/// Confirm the draft moves of `$1`: `waiting` behind origin moves, else `confirmed`; today's date
/// `$2` as reservation date where the operation type reserves at confirmation.
const CONFIRM: &str = "UPDATE stock_move m SET \
       state = CASE WHEN EXISTS (SELECT 1 FROM stock_move_move_rel o WHERE o.move_dest_id = m.id) \
                    THEN 'waiting' ELSE 'confirmed' END, \
       reservation_date = CASE WHEN t.reservation_method = 'at_confirm' \
                               THEN $2::text::date ELSE m.reservation_date END \
     FROM stock_move s \
     LEFT JOIN stock_picking p ON p.id = s.picking_id \
     LEFT JOIN stock_picking_type t ON t.id = COALESCE(s.picking_type_id, p.picking_type_id) \
     WHERE s.id = m.id AND m.id = ANY($1) AND m.state = 'draft' \
     RETURNING m.id";

/// The confirmed moves of `$1` that reserve at confirmation: returns,
/// moves that bypass reservation, moves whose type reserves at confirmation and moves whose
/// reservation date `$2` has come.
const ASSIGN_AT_CONFIRM: &str = "SELECT m.id FROM stock_move m \
     JOIN stock_location l ON l.id = m.location_id \
     JOIN product_product pp ON pp.id = m.product_id \
     JOIN product_template pt ON pt.id = pp.product_tmpl_id \
     LEFT JOIN stock_picking p ON p.id = m.picking_id \
     LEFT JOIN stock_picking_type t ON t.id = COALESCE(m.picking_type_id, p.picking_type_id) \
     WHERE m.id = ANY($1) \
       AND (m.state IN ('confirmed', 'partially_available') OR m.origin_returned_move_id IS NOT NULL) \
       AND (m.origin_returned_move_id IS NOT NULL \
         OR l.usage IN ('supplier', 'customer', 'inventory', 'production') OR NOT pt.is_storable \
         OR t.reservation_method = 'at_confirm' \
         OR m.reservation_date <= $2::text::date) \
     ORDER BY m.id";

fn decode(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("A stock move or transfer could not be read for confirmation.").with_source(e)
}

/// Fill the transfer's company, source, destination, operation type and contact into the values
/// of each new move that names a transfer (or has one as context default) and not the field.
/// One read for all transfers of the call.
///
/// The staged ORM values are flushed first, so a transfer whose header is changed in the same
/// save gives its new values.
///
/// # Errors
///
/// The database error of the flush or the read.
pub(crate) async fn fill_from_pickings(
    records: &Records,
    tx: &mut Transaction,
    vals_list: &mut [RawVals],
) -> Result<()> {
    let default_picking = match records.env().context().get("default_picking_id") {
        Some(Value::Int(id)) => Some(id),
        _ => None,
    };
    let picking_of = |vals: &RawVals| match vals.get("picking_id") {
        Some(cell) => cell_id(cell),
        None => default_picking,
    };
    let pickings: Vec<i64> = vals_list
        .iter()
        .filter_map(picking_of)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if pickings.is_empty() {
        return Ok(());
    }
    // a header change staged in the same save (values before the x2many commands) is read here
    tx.flush().await?;
    let mut rows: HashMap<i64, [Option<i64>; 5]> = HashMap::new();
    for row in tx.query(PICKING_DEFAULTS, &[&pickings]).await? {
        rows.insert(
            row.try_get(0).map_err(decode)?,
            [
                row.try_get(1).map_err(decode)?,
                row.try_get(2).map_err(decode)?,
                row.try_get(3).map_err(decode)?,
                row.try_get(4).map_err(decode)?,
                row.try_get(5).map_err(decode)?,
            ],
        );
    }
    const FIELDS: [&str; 5] = [
        "company_id",
        "location_id",
        "location_dest_id",
        "picking_type_id",
        "partner_id",
    ];
    for vals in vals_list.iter_mut() {
        // a missing transfer is left to the foreign key
        let Some(values) = picking_of(vals).and_then(|id| rows.get(&id)) else {
            continue;
        };
        for (field, value) in FIELDS.into_iter().zip(values) {
            if let Some(id) = value
                && !vals.contains(field)
            {
                vals.insert(field.into(), Cell::Id(*id));
            }
        }
    }
    Ok(())
}

/// Moves that are not draft are left as
/// they are. Validation confirms without the reservation: it makes the one engine call of its unit.
///
/// # Errors
///
/// [`StockError::NeedsProcurement`] for a make-to-order move; the reservation's errors; read and
/// database errors.
pub(crate) async fn confirm_moves(
    records: &Records,
    tx: &mut Transaction,
    moves: &[i64],
    assign: bool,
) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    tx.flush().await?;
    let roots = lock_move_roots(tx, moves, &[]).await?;
    let blocked: i64 = tx
        .query(NEEDS_PROCUREMENT, &[&moves])
        .await?
        .first()
        .ok_or_else(|| Error::internal("A count returned no row."))?
        .try_get(0)
        .map_err(decode)?;
    if blocked > 0 {
        return Err(StockError::NeedsProcurement.into());
    }
    let today = records.env().context_today()?.to_string();
    let mut confirmed = Vec::new();
    for row in tx.query(CONFIRM, &[&moves, &today]).await? {
        confirmed.push(row.try_get::<_, i64>(0).map_err(decode)?);
    }
    if confirmed.is_empty() {
        return Ok(());
    }
    records
        .env()
        .records("stock.move")?
        .browse(confirmed.iter().copied())?
        .modified_external(tx, &["state", "reservation_date"])
        .await?;
    notify_pickings(records, tx, &roots).await?;
    if !assign {
        return Ok(());
    }
    let mut eligible = Vec::new();
    for row in tx.query(ASSIGN_AT_CONFIRM, &[&confirmed, &today]).await? {
        eligible.push(row.try_get::<_, i64>(0).map_err(decode)?);
    }
    assign_moves(records, tx, &eligible).await
}

/// The draft moves of the transfers `pickings`, ascending.
pub(crate) async fn draft_moves(tx: &mut Transaction, pickings: &[i64]) -> Result<Vec<i64>> {
    let mut out = Vec::new();
    for row in tx
        .query(
            "SELECT id FROM stock_move WHERE picking_id = ANY($1) \
               AND state = 'draft' ORDER BY id",
            &[&pickings],
        )
        .await?
    {
        out.push(row.try_get(0).map_err(decode)?);
    }
    Ok(out)
}

#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    /// Transfers with
    /// no draft move are refused by name.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_confirm(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        records.check_access(tx, Operation::Write).await?;
        tx.flush().await?;
        let moves = draft_moves(tx, &records.ids()).await?;
        if moves.is_empty() {
            return Err(StockError::NothingToConfirm.into());
        }
        confirm_moves(records, tx, &moves, true).await
    }

    /// A transfer with no open move is refused by name.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_assign(this: &RecordSet<StockPicking>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        records.check_access(tx, Operation::Write).await?;
        tx.flush().await?;
        let drafts = draft_moves(tx, &records.ids()).await?;
        confirm_moves(records, tx, &drafts, true).await?;
        let moves = open_moves(records, tx).await?;
        if moves.is_empty() {
            return Err(StockError::NothingToAssign.into());
        }
        assign_moves(records, tx, &moves).await
    }
}

#[methods(StockMove, addon = "stock")]
impl StockMove {
    /// A product picked on a new move proposes its unit (on the form).
    #[onchange("product_id")]
    async fn _onchange_product_unit(
        this: &RecordSet<StockMove>,
        tx: &mut Transaction,
    ) -> Result<OnchangeResult> {
        for mv in this.as_records().iter() {
            let product = mv.related(tx, "product_id").await?;
            if product.is_empty() {
                continue;
            }
            let unit = product.cell(tx, "uom_id").await?;
            mv.write_base(tx, RawVals::new().set_cell("uom_id", unit))
                .await?;
        }
        Ok(OnchangeResult::none())
    }
}
