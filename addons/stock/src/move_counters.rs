// SPDX-License-Identifier: MIT OR Apache-2.0
//! The transfer's move counter vector (3).
//!
//! The vector and the stored `state` of `stock.picking` are kept by the statement triggers of
//! [`crate::move_counter_sql`]: every statement that inserts, deletes or changes a counted column
//! of a move (state, demand, scrap flag, supply method, transfer) adds its net contribution per
//! transfer in the same statement, and the transfer's state is evaluated from the result by the
//! one stock-owned SQL expression `picking_state_sql!`. Nothing here counts.
//!
//! This module owns the two obligations the triggers cannot meet on their own:
//! - **root locks** (1 class 2): a typed move mutation locks the
//!   transfers it touches (old and new) in ascending id order, then the moves in ascending id
//!   order, before it writes. Every trusted engine writer of moves calls [`lock_move_roots`] (or
//!   [`lock_pickings`] before inserting moves) the same way;
//! - **cache notification**: after the mutation is flushed, the transfers' cached
//!   counters and state are dropped and their dependents (the operation-type overview counts) are
//!   notified with [`notify_pickings`].

use std::collections::BTreeSet;

use erp_sdk::ErrorKind;
use erp_sdk::prelude::*;

/// The counter vector columns of `stock.picking`.
pub const COUNTER_FIELDS: [&str; 14] = [
    "move_count",
    "draft_count",
    "waiting_count",
    "confirmed_count",
    "partially_available_count",
    "assigned_count",
    "done_count",
    "cancel_count",
    "done_scrap_count",
    "cancel_scrap_count",
    "assigned_zero_demand_count",
    "open_positive_demand_count",
    "confirmed_positive_demand_count",
    "mto_nonscrap_count",
];

/// The columns of `stock.picking` derived from its moves: the vector and the state evaluated from
/// it. No `create` or `write` may name them.
pub const DERIVED_FIELDS: [&str; 15] = [
    "move_count",
    "draft_count",
    "waiting_count",
    "confirmed_count",
    "partially_available_count",
    "assigned_count",
    "done_count",
    "cancel_count",
    "done_scrap_count",
    "cancel_scrap_count",
    "assigned_zero_demand_count",
    "open_positive_demand_count",
    "confirmed_positive_demand_count",
    "mto_nonscrap_count",
    "state",
];

/// The move columns the vector counts. A write naming none of them changes no counter.
pub const COUNTED_MOVE_FIELDS: [&str; 5] = [
    "state",
    "product_uom_qty",
    "is_scrap",
    "procure_method",
    "picking_id",
];

/// The transfer columns its state is evaluated from besides the vector: the shipping policy and
/// the source location (whose usage decides the reservation bypass).
pub const STATE_INPUT_FIELDS: [&str; 2] = ["move_type", "location_id"];

const LOCK_PICKINGS: &str =
    "SELECT FROM stock_picking WHERE id = ANY($1) ORDER BY id FOR NO KEY UPDATE";
const MOVE_ROOTS: &str = "SELECT DISTINCT picking_id FROM stock_move \
     WHERE id = ANY($1) AND picking_id IS NOT NULL";
const LOCK_MOVES: &str =
    "SELECT picking_id FROM stock_move WHERE id = ANY($1) ORDER BY id FOR NO KEY UPDATE";

/// The id a many2one cell names: a typed caller's `Id`, an RPC caller's `Int`; an empty value
/// (`Null`, `false`) names none.
pub fn cell_id(cell: &Cell) -> Option<i64> {
    match cell {
        Cell::Id(id) => Some(*id),
        Cell::Int(id) => Some(i64::from(*id)),
        _ => None,
    }
}

/// Lock the transfers `ids` in ascending id order (the class-2 root lock), waiting for a
/// concurrent holder. Under the transaction's `REPEATABLE READ` snapshot a transfer another
/// transaction changed and committed meanwhile fails with a serialization error (`40001`), which
/// the unit of work retries as a whole.
///
/// # Errors
///
/// The database error of the lock.
pub async fn lock_pickings(tx: &mut Transaction, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    tx.query(LOCK_PICKINGS, &[&ids]).await.map(drop)
}

/// Discover the transfers of `moves` (without locking), add `extra` transfers (a new parent),
/// lock all of them ascending, then lock the moves ascending, and check that no locked move now
/// belongs to a transfer outside the locked set. A move without a transfer is its own root: its
/// row lock is the root lock. Returns the locked transfers, ascending.
///
/// Pending ORM writes must be flushed first: discovery reads the stored transfer of each move.
///
/// # Errors
///
/// A `Concurrency` error with SQLSTATE `40001` (retryable as a whole unit) when a move changed
/// transfer between discovery and its lock; the database error of a lock.
pub async fn lock_move_roots(
    tx: &mut Transaction,
    moves: &[i64],
    extra: &[i64],
) -> Result<Vec<i64>> {
    let mut roots: BTreeSet<i64> = extra.iter().copied().collect();
    if !moves.is_empty() {
        for row in tx.query(MOVE_ROOTS, &[&moves]).await? {
            let id: i64 = row.try_get(0).map_err(|e| {
                Error::db("A stock move's transfer could not be read.").with_source(e)
            })?;
            roots.insert(id);
        }
    }
    let roots: Vec<i64> = roots.into_iter().collect();
    lock_pickings(tx, &roots).await?;
    if moves.is_empty() {
        return Ok(roots);
    }
    for row in tx.query(LOCK_MOVES, &[&moves]).await? {
        let picking: Option<i64> = row
            .try_get(0)
            .map_err(|e| Error::db("A stock move's transfer could not be read.").with_source(e))?;
        if picking.is_some_and(|id| roots.binary_search(&id).is_err()) {
            return Err(Error::new(
                ErrorKind::Concurrency,
                "could not serialize access: a stock move changed transfer while its transfers \
                 were being locked",
            )
            .with_sqlstate(Some("40001".to_owned())));
        }
    }
    Ok(roots)
}

/// The distinct transfers the stored moves `moves` belong to, ascending.
///
/// # Errors
///
/// The database error of the read.
pub async fn pickings_of(tx: &mut Transaction, moves: &[i64]) -> Result<Vec<i64>> {
    if moves.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for row in tx.query(MOVE_ROOTS, &[&moves]).await? {
        out.push(
            row.try_get(0).map_err(|e| {
                Error::db("A stock move's transfer could not be read.").with_source(e)
            })?,
        );
    }
    out.sort_unstable();
    Ok(out)
}

/// Tell the ORM that the triggers changed the derived columns of the transfers `ids` in this
/// transaction: their cached counters and state are dropped and the dependents (the
/// operation-type overview counts and any addon compute on the state) are notified as by a write.
/// Call it after the mutation is flushed. No access is checked or granted here: the mutation that
/// changed the moves was authorized by its caller.
///
/// # Errors
///
/// The notification's errors (a pending write of a derived column is a `Programming` error).
pub async fn notify_pickings(records: &Records, tx: &mut Transaction, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let pickings = records
        .env()
        .records("stock.picking")?
        .browse(ids.iter().copied())?;
    pickings.modified_external(tx, &DERIVED_FIELDS).await
}
