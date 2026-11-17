// SPDX-License-Identifier: MIT OR Apache-2.0
//! Engine-owned columns of moves and move lines (lead ruling L6'): the done time,
//! the open/done membership, the correction counter and the move's open-line version (
//! "Writers") are written only by the stock engine's SQL statements, never through the ORM.
//! `readonly` binds the client only, so an ORM or RPC `create` or `write` naming one of them, or a
//! `create` whose context carries a `default_<field>` key for one, is refused here, after the model
//! access check, in the pattern of the quant balance guard (`quant_guards`). Every create row then
//! gets the columns' initial values, so no stored `ir.default` can supply them either. The engine
//! never reaches these overrides.
//!
//! A move's status and quantity follow from its operation lines, its confirmation and its
//! reservation ([`crate::move_lines`], [`crate::confirm`], [`crate::reservation`]): they are
//! refused the same way for every caller, superuser included, and a new move starts `draft` with
//! no quantity.
//!
//! The same overrides keep a transfer's move counters (D3, lead conditions M4, M5):
//! the counter vector and state of `stock.picking` are refused by name on `create` and `write` for
//! every caller, and a move `create`, a `write` naming a counted column, and an `unlink` lock their
//! transfers before their moves ([`crate::move_counters`]), flush, and notify the ORM of the
//! counters the triggers changed.
//!
//! They also keep done work fixed ([`crate::done`]): no create, write or default sets a move to
//! done, done moves and transfers change only within their allow-lists, a move is not added to a
//! done transfer, and done moves and transfers are not deleted.
//!
//! They also keep the move references and chains of [`crate::move_relations`] (topology ADR D3):
//! a move create, a write naming the transfer or the references, a move unlink and a reference
//! write naming its moves rebuild the reference projection of the transfers they touch, and a move
//! linked to a transfer takes the transfer's references when it has none; both sides of the
//! projection are refused by name; a deadline write shifts the linked open moves.

use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::confirm::fill_from_pickings;
use crate::done::{check_move_create, check_move_write, check_picking_write, check_unlink};
use crate::error::StockError;
use crate::move_counters::{
    COUNTED_MOVE_FIELDS, DERIVED_FIELDS, STATE_INPUT_FIELDS, cell_id, lock_move_roots,
    lock_pickings, notify_pickings, pickings_of,
};
use crate::move_lines::{create_lines, unlink_lines, write_lines};
use crate::move_relations::{
    apply_deadlines, deadline_plan, inherit_references, member_pickings, rebuild_references,
};
use crate::move_units::{SNAPSHOT_FIELDS, UNIT_INPUTS, fill_create, write_groups};
use crate::picking::StockPicking;
use crate::picking_flow::{delete_moves_of, fill_from_types};
use crate::reference::StockReference;
use crate::stock_move::StockMove;
use crate::stock_move_line::StockMoveLine;

const MOVE_FIELDS: [&str; 2] = ["done_at", "open_line_version"];
/// Move columns the line, confirmation and reservation statements write. The product-unit
/// quantity follows the quantity through the unit snapshot check ([`crate::move_units`]).
const LINE_DERIVED: [&str; 2] = ["state", "quantity"];
const LINE_FIELDS: [&str; 3] = ["done_at", "membership", "correction_seq"];

fn label(tx: &Transaction, model: &str, field: &str) -> Result<String> {
    tx.registry()
        .model(model)
        .and_then(|spec| spec.field(field))
        .map(|spec| spec.string.clone())
        .ok_or_else(|| Error::internal(format!("{model} has no field '{field}'")))
}

/// Who writes the guarded columns: the stock engine (`what` names the record), the transfer's
/// moves through the counter triggers, or the reference projection rebuild.
#[derive(Clone, Copy)]
enum Writer {
    Engine(&'static str),
    Moves,
    Lines,
    References,
}

impl Writer {
    fn refusal(self, field: String, default: bool) -> Error {
        match (self, default) {
            (Self::Engine(what), false) => StockError::EngineOnlyField { what, field },
            (Self::Engine(what), true) => StockError::EngineOnlyDefault { what, field },
            (Self::Moves, default) => StockError::DerivedFromMoves { field, default },
            (Self::Lines, default) => StockError::DerivedFromLines { field, default },
            (Self::References, default) => StockError::ReferenceProjection { field, default },
        }
        .into()
    }
}

/// True when `vals` names `field`, as a plain value or as an x2many command list (the form RPC,
/// data files and `copy` deliver x2many fields as commands).
fn names(vals: &RawVals, field: &str) -> bool {
    vals.contains(field) || vals.iter_commands().any(|(name, _)| name == field)
}

/// The first guarded field `vals` names, as a refusal with its label.
fn refuse(
    tx: &Transaction,
    model: &str,
    writer: Writer,
    fields: &[&str],
    vals: &RawVals,
) -> Result<()> {
    let Some(field) = fields.iter().find(|f| names(vals, f)) else {
        return Ok(());
    };
    let field = label(tx, model, field)?;
    Err(writer.refusal(field, false))
}

/// The value a new row starts with: the declared defaults (`open`, 0, a transfer's `draft`) and
/// an empty done time.
fn initial(field: &str) -> Result<Cell> {
    if DERIVED_FIELDS.contains(&field) {
        return Ok(if field == "state" {
            Cell::Str("draft".into())
        } else {
            Cell::Int(0)
        });
    }
    match field {
        "membership" => Ok(Cell::Str("open".into())),
        "correction_seq" | "open_line_version" => Ok(Cell::Int(0)),
        "quantity" => Ok(Cell::Decimal(Decimal::ZERO)),
        "done_at" => Ok(Cell::Null),
        other => Err(Error::internal(format!(
            "no initial value for the engine-owned field '{other}'"
        ))),
    }
}

async fn check_create(
    records: &Records,
    tx: &mut Transaction,
    model: &str,
    writer: Writer,
    fields: &[&'static str],
    vals_list: &mut [RawVals],
) -> Result<()> {
    let context = records.env().context();
    let defaulted = fields
        .iter()
        .copied()
        .find(|f| context.contains(&format!("default_{f}")));
    if defaulted.is_some()
        || vals_list
            .iter()
            .any(|vals| fields.iter().any(|f| names(vals, f)))
    {
        records.check_access(tx, Operation::Create).await?;
        if let Some(field) = defaulted {
            let field = label(tx, model, field)?;
            return Err(writer.refusal(field, true));
        }
        for vals in vals_list.iter() {
            refuse(tx, model, writer, fields, vals)?;
        }
    }
    // a projection starts empty: the rebuild fills it, no initial value is written
    if matches!(writer, Writer::References) {
        return Ok(());
    }
    for vals in vals_list {
        for field in fields {
            vals.insert((*field).into(), initial(field)?);
        }
    }
    Ok(())
}

async fn check_write(
    records: &Records,
    tx: &mut Transaction,
    model: &str,
    writer: Writer,
    fields: &[&str],
    vals: &RawVals,
) -> Result<()> {
    if fields.iter().any(|f| names(vals, f)) {
        records.check_access(tx, Operation::Write).await?;
        refuse(tx, model, writer, fields, vals)?;
    }
    Ok(())
}

#[methods(StockMove, addon = "stock")]
impl StockMove {
    /// The done time, the open-line version, the status and the quantity come from the engine;
    /// refused by name or context default after the create access check, then set to their
    /// initial values. The transfers the new moves name (in the values or as the context default)
    /// are locked before the insert; the counters the insert changed are notified once it is
    /// flushed.
    #[method(override)]
    async fn create(
        this: &RecordSet<StockMove>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let records = this.as_records();
        check_create(
            records,
            tx,
            "stock.move",
            Writer::Engine("stock move"),
            &MOVE_FIELDS,
            &mut vals_list,
        )
        .await?;
        check_create(
            records,
            tx,
            "stock.move",
            Writer::Lines,
            &LINE_DERIVED,
            &mut vals_list,
        )
        .await?;
        check_move_create(records, tx, &vals_list).await?;
        fill_from_pickings(records, tx, &mut vals_list).await?;
        fill_create(records, tx, &mut vals_list).await?;
        let mut roots: Vec<i64> = vals_list
            .iter()
            .filter_map(|vals| vals.get("picking_id").and_then(cell_id))
            .collect();
        if let Some(Value::Int(id)) = records.env().context().get("default_picking_id") {
            roots.push(id);
        }
        if !roots.is_empty() {
            records.check_access(tx, Operation::Create).await?;
            lock_pickings(tx, &roots).await?;
        }
        let created = sup.call(tx, records, (vals_list,)).await?;
        tx.flush().await?;
        let pickings = pickings_of(tx, &created.ids()).await?;
        notify_pickings(&created, tx, &pickings).await?;
        if !pickings.is_empty() {
            rebuild_references(tx, &pickings).await?;
            inherit_references(tx, &created.ids()).await?;
        }
        Ok(created)
    }

    /// The done time, the open-line version, the status and the quantity come from the engine;
    /// refused by name after the write access check. A write naming a counted column (demand,
    /// scrap flag, supply method, transfer) locks the old and new transfers, then the moves, writes, flushes and
    /// notifies the counters of those transfers. A deadline write first locks the transfers, then
    /// the moves, of every move its shift may reach.
    #[method(override)]
    async fn write(
        this: &RecordSet<StockMove>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        check_write(
            records,
            tx,
            "stock.move",
            Writer::Engine("stock move"),
            &MOVE_FIELDS,
            &vals,
        )
        .await?;
        check_write(
            records,
            tx,
            "stock.move",
            Writer::Lines,
            &LINE_DERIVED,
            &vals,
        )
        .await?;
        check_move_write(records, tx, &vals).await?;
        let mut vals = vals;
        if !records.is_empty()
            && (UNIT_INPUTS.iter().any(|f| vals.contains(f))
                || SNAPSHOT_FIELDS.iter().any(|f| vals.contains(f)))
        {
            let mut groups = write_groups(records, tx, &vals).await?;
            if groups.len() > 1 {
                return write_each(records, tx, groups).await;
            }
            if let Some((_, group)) = groups.pop() {
                vals = group;
            }
        }
        let counted = COUNTED_MOVE_FIELDS.iter().any(|f| vals.contains(f));
        let relinked = vals.contains("picking_id");
        let referenced = relinked || names(&vals, "reference_ids");
        let deadline = vals.contains("date_deadline");
        if records.is_empty() || !(counted || referenced || deadline) {
            return sup.call(tx, records, (vals,)).await;
        }
        records.check_access(tx, Operation::Write).await?;
        // discovery reads the stored transfer, chain and deadline of each move
        tx.flush().await?;
        let ids = records.ids();
        let plan = if deadline {
            Some(deadline_plan(tx, &ids).await?)
        } else {
            None
        };
        let new_root: Vec<i64> = vals
            .get("picking_id")
            .and_then(cell_id)
            .into_iter()
            .collect();
        // the moves a deadline shift may write, and their transfers, are locked in the
        // root order before any of them is written; the lock below then holds them already
        if let Some(plan) = &plan {
            lock_move_roots(tx, &plan.moves(), &new_root).await?;
        }
        let roots = if counted || referenced {
            lock_move_roots(tx, &ids, &new_root).await?
        } else {
            Vec::new()
        };
        sup.call(tx, records, (vals,)).await?;
        tx.flush().await?;
        if counted {
            notify_pickings(records, tx, &roots).await?;
        }
        if referenced {
            rebuild_references(tx, &roots).await?;
        }
        if relinked {
            inherit_references(tx, &ids).await?;
        }
        match plan {
            Some(plan) => apply_deadlines(records, tx, &plan).await,
            None => Ok(()),
        }
    }

    /// Deleting moves locks their transfers, then the moves, and notifies the counters the delete
    /// changed.
    #[method(override)]
    async fn unlink(this: &RecordSet<StockMove>, tx: &mut Transaction, sup: Super) -> Result<()> {
        let records = this.as_records();
        if records.is_empty() {
            return sup.call(tx, records, ()).await;
        }
        records.check_access(tx, Operation::Unlink).await?;
        check_unlink(records, tx, "stock.move").await?;
        tx.flush().await?;
        let roots = lock_move_roots(tx, &records.ids(), &[]).await?;
        sup.call(tx, records, ()).await?;
        tx.flush().await?;
        notify_pickings(records, tx, &roots).await?;
        rebuild_references(tx, &roots).await
    }
}

/// Write the groups of [`write_groups`] one ORM write each, after locking the transfers of all of
/// them in order (each nested write re-locks only what it holds already).
async fn write_each(
    records: &Records,
    tx: &mut Transaction,
    groups: Vec<(Vec<i64>, RawVals)>,
) -> Result<()> {
    records.check_access(tx, Operation::Write).await?;
    tx.flush().await?;
    let new_root: Vec<i64> = groups
        .iter()
        .filter_map(|(_, vals)| vals.get("picking_id").and_then(cell_id))
        .collect();
    lock_move_roots(tx, &records.ids(), &new_root).await?;
    for (ids, vals) in groups {
        records.browse(ids)?.write(tx, vals).await?;
    }
    Ok(())
}

#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    /// The counter vector and the state follow from the moves: refused by name or context default
    /// after the create access check, then set to their initial values (no move, `draft`). The
    /// operation type supplies the reference, locations and company (`crate::picking_flow`).
    #[method(override)]
    async fn create(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let records = this.as_records();
        check_create(
            records,
            tx,
            "stock.picking",
            Writer::Moves,
            &DERIVED_FIELDS,
            &mut vals_list,
        )
        .await?;
        check_create(
            records,
            tx,
            "stock.picking",
            Writer::References,
            &["reference_ids"],
            &mut vals_list,
        )
        .await?;
        fill_from_types(records, tx, &mut vals_list).await?;
        sup.call(tx, records, (vals_list,)).await
    }

    /// The counter vector and the state are refused by name after the write access check. A
    /// shipping-policy or source-location change re-evaluates the state in its own statement (the
    /// state trigger); it is flushed and the state notified.
    #[method(override)]
    async fn write(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        check_write(
            records,
            tx,
            "stock.picking",
            Writer::Moves,
            &DERIVED_FIELDS,
            &vals,
        )
        .await?;
        check_write(
            records,
            tx,
            "stock.picking",
            Writer::References,
            &["reference_ids"],
            &vals,
        )
        .await?;
        check_picking_write(records, tx, &vals).await?;
        let reevaluate = !records.is_empty() && STATE_INPUT_FIELDS.iter().any(|f| vals.contains(f));
        sup.call(tx, records, (vals,)).await?;
        if reevaluate {
            tx.flush().await?;
            notify_pickings(records, tx, &records.ids()).await?;
        }
        Ok(())
    }

    /// A done transfer is not deleted (`crate::done`); the moves of any other are cancelled and
    /// deleted first (`crate::picking_flow`).
    #[method(override)]
    async fn unlink(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        if records.is_empty() {
            return sup.call(tx, records, ()).await;
        }
        records.check_access(tx, Operation::Unlink).await?;
        check_unlink(records, tx, "stock.picking").await?;
        delete_moves_of(records, tx).await?;
        sup.call(tx, records, ()).await
    }
}

#[methods(StockReference, addon = "stock")]
impl StockReference {
    /// The transfers of a reference follow from its moves: refused by name or context default
    /// after the create access check. References created with moves rebuild the projection of
    /// those moves' transfers.
    #[method(override)]
    async fn create(
        this: &RecordSet<StockReference>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let records = this.as_records();
        check_create(
            records,
            tx,
            "stock.reference",
            Writer::References,
            &["picking_ids"],
            &mut vals_list,
        )
        .await?;
        let linked = vals_list.iter().any(|vals| names(vals, "move_ids"));
        let created = sup.call(tx, records, (vals_list,)).await?;
        if linked {
            tx.flush().await?;
            let pickings = member_pickings(tx, &created.ids()).await?;
            rebuild_references(tx, &pickings).await?;
        }
        Ok(created)
    }

    /// The transfers of a reference are refused by name after the write access check. A write
    /// naming the moves locks the transfers of the current moves, writes, and rebuilds the
    /// projection of the old and new moves' transfers. The transfers of newly linked moves are
    /// known only after the link and are locked then, by the rebuild; a writer crossing that
    /// order ends in a detected deadlock (`40P01`), retried as a whole unit.
    #[method(override)]
    async fn write(
        this: &RecordSet<StockReference>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        check_write(
            records,
            tx,
            "stock.reference",
            Writer::References,
            &["picking_ids"],
            &vals,
        )
        .await?;
        if records.is_empty() || !names(&vals, "move_ids") {
            return sup.call(tx, records, (vals,)).await;
        }
        records.check_access(tx, Operation::Write).await?;
        tx.flush().await?;
        let mut pickings = member_pickings(tx, &records.ids()).await?;
        lock_pickings(tx, &pickings).await?;
        sup.call(tx, records, (vals,)).await?;
        tx.flush().await?;
        pickings.extend(member_pickings(tx, &records.ids()).await?);
        rebuild_references(tx, &pickings).await
    }
}

#[methods(StockMoveLine, addon = "stock")]
impl StockMoveLine {
    /// Done time, membership and correction counter come from the engine; refused by name or context
    /// default after the create access check, then set to their initial values. The line takes its
    /// move's defaults and snapshot, and its reservation and move follow (`move_lines`).
    #[method(override)]
    async fn create(
        this: &RecordSet<StockMoveLine>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let records = this.as_records();
        check_create(
            records,
            tx,
            "stock.move.line",
            Writer::Engine("stock move line"),
            &LINE_FIELDS,
            &mut vals_list,
        )
        .await?;
        create_lines(records, tx, vals_list, sup).await
    }

    /// Done time, membership and correction counter come from the engine; refused by name after the
    /// write access check. The move link is fixed, done lines are refused, and a quantity, unit or
    /// key change moves the reservation (`move_lines`).
    #[method(override)]
    async fn write(
        this: &RecordSet<StockMoveLine>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        check_write(
            records,
            tx,
            "stock.move.line",
            Writer::Engine("stock move line"),
            &LINE_FIELDS,
            &vals,
        )
        .await?;
        write_lines(records, tx, vals, sup).await
    }

    /// Deleting open lines gives their reservations back and resettles their moves; done lines are
    /// refused (`move_lines`).
    #[method(override)]
    async fn unlink(
        this: &RecordSet<StockMoveLine>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        unlink_lines(this.as_records(), tx, sup).await
    }
}
