// SPDX-License-Identifier: MIT OR Apache-2.0
//! Move references and move chains (topology ADR D3, and 137..140).
//!
//! **References.** `stock_reference_move_rel` is the truth. `stock_reference_picking_rel` is a
//! stored projection: the distinct union of the references of the moves of each transfer. The move
//! and reference overrides of [`crate::move_guards`] call [`rebuild_references`] after every
//! change of that union's inputs (a move's references, from either side of the relation; a move's
//! transfer; a move's deletion). The rebuild locks the affected transfers ascending and updates
//! their rows before it reads membership, so a `REPEATABLE READ` transaction that rebuilt the same
//! transfer from an older snapshot fails with `40001` and retries (D3). Direct writes of either
//! side of the projection are refused by the overrides. A move linked to a transfer without
//! references of its own takes the transfer's references ([`inherit_references`]).
//!
//! **Deadlines.** A deadline write shifts the deadlines of the open moves linked to it, through
//! both chain directions, by the same difference, each move at most once (
//! 139). The reachable chain is read in one bounded query before the write ([`deadline_plan`]),
//! the visit is computed in memory in the reference's order ([`propagate`]), and the result is
//! written in one statement after the seeds' write ([`apply_deadlines`]). The bound is the system
//! parameter `stock.move_chain_limit`.

use std::collections::{HashMap, HashSet};

use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::error::StockError;
use crate::move_counters::lock_pickings;
use crate::stock_move::StockMove;

/// System parameter: the most moves one deadline change may reach through the chain links.
pub const CHAIN_LIMIT_KEY: &str = "stock.move_chain_limit";

// One statement: the version update of the transfers (D3: a lock on an unchanged row would not
// make an older snapshot retry), the removal of pairs no move supports and the missing pairs.
const REBUILD: &str = "WITH touched AS ( \
       UPDATE stock_picking SET move_count = move_count WHERE id = ANY($1) RETURNING id), \
     want AS ( \
       SELECT DISTINCT r.reference_id, m.picking_id FROM stock_reference_move_rel r \
       JOIN stock_move m ON m.id = r.move_id WHERE m.picking_id = ANY($1)), \
     gone AS ( \
       DELETE FROM stock_reference_picking_rel p WHERE p.picking_id = ANY($1) \
       AND NOT EXISTS (SELECT FROM want w \
                       WHERE w.reference_id = p.reference_id AND w.picking_id = p.picking_id) \
       RETURNING p.picking_id) \
     INSERT INTO stock_reference_picking_rel (reference_id, picking_id) \
     SELECT reference_id, picking_id FROM want ON CONFLICT DO NOTHING";
const INHERIT: &str = "INSERT INTO stock_reference_move_rel (reference_id, move_id) \
     SELECT p.reference_id, m.id FROM stock_move m \
     JOIN stock_reference_picking_rel p ON p.picking_id = m.picking_id \
     WHERE m.id = ANY($1) \
     AND NOT EXISTS (SELECT FROM stock_reference_move_rel r WHERE r.move_id = m.id) \
     ON CONFLICT DO NOTHING";
const MEMBER_PICKINGS: &str = "SELECT DISTINCT m.picking_id FROM stock_reference_move_rel r \
     JOIN stock_move m ON m.id = r.move_id \
     WHERE r.reference_id = ANY($1) AND m.picking_id IS NOT NULL";
// The open moves linked to the seeds through both directions, seeds included whatever their
// state; `UNION` ends cycles, the outer `LIMIT` ends the walk early (PostgreSQL evaluates a
// recursive query only as far as its reader fetches).
const REACH: &str = "WITH RECURSIVE reach(id) AS ( \
       SELECT unnest($1::int8[]) \
       UNION \
       SELECT n.id FROM reach r \
       CROSS JOIN LATERAL ( \
         SELECT e.move_dest_id AS id FROM stock_move_move_rel e WHERE e.move_orig_id = r.id \
         UNION ALL \
         SELECT e.move_orig_id FROM stock_move_move_rel e WHERE e.move_dest_id = r.id) l \
       JOIN stock_move n ON n.id = l.id \
       WHERE n.state NOT IN ('done', 'cancel')), \
     limited AS (SELECT id FROM reach LIMIT $2) \
     SELECT m.id, m.sequence, m.date_deadline FROM limited JOIN stock_move m USING (id)";
const EDGES: &str = "SELECT move_orig_id, move_dest_id FROM stock_move_move_rel \
     WHERE move_orig_id = ANY($1) AND move_dest_id = ANY($1)";
const WRITTEN: &str = "SELECT date_deadline FROM stock_move WHERE id = $1";
const SHIFT: &str = "UPDATE stock_move m SET date_deadline = v.deadline \
     FROM unnest($1::int8[], $2::timestamp[]) AS v(id, deadline) WHERE m.id = v.id";

fn decode(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("A stock move chain row could not be read.").with_source(e)
}

/// Rebuild the reference projection of the transfers `pickings` from their moves' references.
/// Locks the transfers ascending first (a no-op for those the caller holds).
///
/// # Errors
///
/// The database errors of the lock and the statement (`40001` for an older snapshot, retryable as
/// a whole unit); cache invalidation errors.
pub async fn rebuild_references(tx: &mut Transaction, pickings: &[i64]) -> Result<()> {
    if pickings.is_empty() {
        return Ok(());
    }
    let mut ids = pickings.to_vec();
    ids.sort_unstable();
    ids.dedup();
    lock_pickings(tx, &ids).await?;
    tx.execute(REBUILD, &[&ids]).await?;
    tx.invalidate_model("stock.picking", Some(&["reference_ids"]))
        .await?;
    tx.invalidate_model("stock.reference", Some(&["picking_ids"]))
        .await
}

/// Give each move of `moves` that has a transfer and no reference the references of its transfer.
/// Call it after [`rebuild_references`], so the transfer's set includes the
/// moves written in the same call; the inherited pairs leave the union unchanged.
///
/// # Errors
///
/// The database error of the insert; cache invalidation errors.
pub async fn inherit_references(tx: &mut Transaction, moves: &[i64]) -> Result<()> {
    if moves.is_empty() || tx.execute(INHERIT, &[&moves]).await? == 0 {
        return Ok(());
    }
    tx.invalidate_model("stock.move", Some(&["reference_ids"]))
        .await?;
    tx.invalidate_model("stock.reference", Some(&["move_ids"]))
        .await
}

/// The transfers of the moves the references `references` link, ascending.
///
/// # Errors
///
/// The database error of the read.
pub async fn member_pickings(tx: &mut Transaction, references: &[i64]) -> Result<Vec<i64>> {
    if references.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for row in tx.query(MEMBER_PICKINGS, &[&references]).await? {
        out.push(row.try_get(0).map_err(decode)?);
    }
    out.sort_unstable();
    Ok(out)
}

/// The chain reachable from the written moves, read before their write.
pub struct DeadlinePlan {
    /// The written moves in call order, with their deadline before the write.
    seeds: Vec<(i64, Option<DateTime>)>,
    /// The deadline before the write of every reached open move (and of the seeds).
    nodes: HashMap<i64, Option<DateTime>>,
    /// Per move, its destinations then its origins, each in move order (sequence, id).
    neighbours: HashMap<i64, Vec<i64>>,
}

impl DeadlinePlan {
    /// Every move the plan may write: the seeds and the reached open moves.
    pub fn moves(&self) -> Vec<i64> {
        self.seeds
            .iter()
            .map(|(id, _)| *id)
            .chain(self.nodes.keys().copied())
            .collect()
    }
}

/// Read the chain linked to `seeds` (flushed, in call order) before their deadline is written.
///
/// # Errors
///
/// [`StockError::ChainLimit`] when more moves than `stock.move_chain_limit` are reachable; the
/// parameter read and database errors.
pub async fn deadline_plan(tx: &mut Transaction, seeds: &[i64]) -> Result<DeadlinePlan> {
    let limit = erp_sdk::params::get_int(tx, &crate::ADDON, CHAIN_LIMIT_KEY).await?;
    let fetch = limit.saturating_add(1);
    let mut nodes = HashMap::new();
    let mut order = HashMap::new();
    for row in tx.query(REACH, &[&seeds, &fetch]).await? {
        let id: i64 = row.try_get(0).map_err(decode)?;
        let sequence: Option<i32> = row.try_get(1).map_err(decode)?;
        nodes.insert(id, row.try_get::<_, Option<DateTime>>(2).map_err(decode)?);
        order.insert(id, (sequence.unwrap_or(0), id));
    }
    if i64::try_from(nodes.len()).unwrap_or(i64::MAX) > limit {
        return Err(StockError::ChainLimit { limit }.into());
    }
    let ids: Vec<i64> = nodes.keys().copied().collect();
    let mut dests: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut origs: HashMap<i64, Vec<i64>> = HashMap::new();
    for row in tx.query(EDGES, &[&ids]).await? {
        let (orig, dest): (i64, i64) = (
            row.try_get(0).map_err(decode)?,
            row.try_get(1).map_err(decode)?,
        );
        dests.entry(orig).or_default().push(dest);
        origs.entry(dest).or_default().push(orig);
    }
    let mut neighbours = HashMap::new();
    for id in ids {
        let mut list = dests.remove(&id).unwrap_or_default();
        list.sort_by_key(|n| order.get(n).copied());
        let mut before = origs.remove(&id).unwrap_or_default();
        before.sort_by_key(|n| order.get(n).copied());
        before.retain(|n| !list.contains(n));
        list.extend(before);
        neighbours.insert(id, list);
    }
    let seeds = seeds
        .iter()
        .map(|id| (*id, nodes.get(id).copied().flatten()))
        .collect();
    Ok(DeadlinePlan {
        seeds,
        nodes,
        neighbours,
    })
}

fn out_of_range() -> Error {
    Error::validation("The shifted deadline is outside the supported date range.")
}

/// The deadline changes a write of `new` on the plan's seeds makes, in visit order (each reached
/// move at most once; a move whose value would not change is left out).
///
/// The reference's visit, made explicit: from a move written from `old` to `new`, each unvisited
/// open neighbour (destinations first, then origins) with a deadline moves by `old - new` when
/// both are set and differ; otherwise it takes `new` unless it already has that value (an empty
/// deadline always takes `new`, even an empty one). A neighbour taken this way is visited and
/// continues the walk from its own old and new value. The seeds are visited from the start.
///
/// # Errors
///
/// `Validation` when a shifted deadline leaves the supported range.
pub fn propagate(
    plan: &DeadlinePlan,
    new: Option<DateTime>,
) -> Result<Vec<(i64, Option<DateTime>)>> {
    let mut visited: HashSet<i64> = plan.seeds.iter().map(|s| s.0).collect();
    let mut changes = Vec::new();
    for &(seed, old) in &plan.seeds {
        // (move, its shift, its new value, next neighbour index)
        let mut stack = vec![(seed, shift(old, new)?, new, 0usize)];
        while let Some(frame) = stack.last_mut() {
            let (node, delta, value, next) = *frame;
            let list = plan.neighbours.get(&node).map_or(&[][..], Vec::as_slice);
            let Some(&u) = list.get(next) else {
                stack.pop();
                continue;
            };
            frame.3 += 1;
            if visited.contains(&u) {
                continue;
            }
            let Some(&u_old) = plan.nodes.get(&u) else {
                continue;
            };
            let u_new = match (u_old, delta) {
                (Some(d), Some(delta)) => Some(DateTime::from_micros(
                    d.micros().checked_sub(delta).ok_or_else(out_of_range)?,
                )),
                _ if u_old.is_none() || u_old != value => value,
                _ => continue,
            };
            visited.insert(u);
            if u_new != u_old {
                changes.push((u, u_new));
            }
            stack.push((u, shift(u_old, u_new)?, u_new, 0));
        }
    }
    Ok(changes)
}

/// `old - new` in microseconds when both are set and differ.
fn shift(old: Option<DateTime>, new: Option<DateTime>) -> Result<Option<i64>> {
    match (old, new) {
        (Some(old), Some(new)) => {
            let delta = old
                .micros()
                .checked_sub(new.micros())
                .ok_or_else(out_of_range)?;
            Ok((delta != 0).then_some(delta))
        }
        _ => Ok(None),
    }
}

/// Write the deadline changes of `plan` after the seeds' write is flushed (their new value is read
/// back as stored): the changed moves must be writable by the caller (record rules included), then
/// one statement writes them and the ORM is notified.
///
/// # Errors
///
/// The access error of a move the caller cannot write; a range error; database and notification
/// errors.
pub async fn apply_deadlines(
    records: &Records,
    tx: &mut Transaction,
    plan: &DeadlinePlan,
) -> Result<()> {
    let Some(&(seed, _)) = plan.seeds.first() else {
        return Ok(());
    };
    let rows = tx.query(WRITTEN, &[&seed]).await?;
    let new: Option<DateTime> = match rows.first() {
        Some(row) => row.try_get(0).map_err(decode)?,
        None => {
            return Err(Error::internal(
                "A stock move whose deadline was written is missing.",
            ));
        }
    };
    let changes = propagate(plan, new)?;
    if changes.is_empty() {
        return Ok(());
    }
    let (ids, deadlines): (Vec<i64>, Vec<Option<DateTime>>) = changes.into_iter().unzip();
    let moves = records
        .env()
        .records("stock.move")?
        .browse(ids.iter().copied())?;
    moves.check_access(tx, Operation::Write).await?;
    tx.execute(SHIFT, &[&ids, &deadlines]).await?;
    // shortcut: write_date/write_uid of the shifted moves are not stamped (relational
    // notification is not available yet)
    moves.modified_external(tx, &["date_deadline"]).await
}

const TERMINAL: [&str; 2] = ["done", "cancel"];

#[methods(StockMove, addon = "stock")]
impl StockMove {
    /// The latest date of the unfinished origins when it is later than the move's own date; empty
    /// for a done or cancelled move and when no unfinished origin is later.
    #[compute(depends = ["move_orig_ids.date", "move_orig_ids.state", "state", "date"])]
    async fn _compute_delay_alert_date(
        this: &RecordSet<StockMove>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockMove>,
    ) -> Result<()> {
        let states = this.mapped(tx, StockMove::STATE).await?;
        let dates = this.mapped(tx, StockMove::DATE).await?;
        let origins = this.mapped(tx, StockMove::MOVE_ORIG_IDS).await?;
        let ids: Vec<i64> = origins
            .iter()
            .flatten()
            .filter_map(|id| id.as_real().map(|real| real.get()))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let before = this.as_records().browse(ids.iter().copied())?;
        let before_states = before.cells(tx, "state").await?;
        let before_dates = before.cells(tx, "date").await?;
        // the unfinished origins' dates; a terminal or undated origin counts as none
        let mut open: HashMap<i64, DateTime> = HashMap::new();
        for ((id, state), date) in before
            .ids()
            .into_iter()
            .zip(before_states)
            .zip(before_dates)
        {
            match (state, date) {
                (Cell::Str(state), Cell::DateTime(date)) if !TERMINAL.contains(&state.as_ref()) => {
                    open.insert(id, date);
                }
                _ => {}
            }
        }
        for (index, ((state, date), origins)) in
            states.into_iter().zip(dates).zip(origins).enumerate()
        {
            let open_move = state.is_some_and(|s| !TERMINAL.contains(&s.as_ref()));
            let latest = origins
                .iter()
                .filter_map(|id| id.as_real().and_then(|real| open.get(&real.get())))
                .max()
                .copied();
            let alert = match (latest, date) {
                (Some(latest), Some(date)) if open_move && latest > date => Some(latest),
                _ => None,
            };
            out.set(index, StockMove::DELAY_ALERT_DATE, alert)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: i64) -> Option<DateTime> {
        Some(DateTime::from_micros(day * 86_400_000_000))
    }

    fn plan(
        seeds: &[(i64, Option<DateTime>)],
        nodes: &[(i64, Option<DateTime>)],
        edges: &[(i64, &[i64])],
    ) -> DeadlinePlan {
        DeadlinePlan {
            seeds: seeds.to_vec(),
            nodes: nodes.iter().copied().collect(),
            neighbours: edges.iter().map(|(a, b)| (*a, b.to_vec())).collect(),
        }
    }

    #[test]
    fn a_shift_moves_each_linked_deadline_once() {
        // M1 -> M2 -> M3 -> M1 (a cycle): M1 moves from day 10 to day 11
        let p = plan(
            &[(1, at(10))],
            &[(1, at(10)), (2, at(12)), (3, at(20))],
            &[(1, &[2, 3]), (2, &[3, 1]), (3, &[1, 2])],
        );
        assert_eq!(
            propagate(&p, at(11)).unwrap(),
            vec![(2, at(13)), (3, at(21))]
        );
    }

    #[test]
    fn a_terminal_neighbour_is_neither_changed_nor_crossed() {
        // 2 is done, so it is not in the plan's open moves: 3 behind it is not reached
        let p = plan(
            &[(1, at(10))],
            &[(1, at(10)), (3, at(5))],
            &[(1, &[2]), (2, &[3])],
        );
        assert_eq!(propagate(&p, at(11)).unwrap(), vec![]);
    }

    #[test]
    fn without_a_shift_neighbours_take_the_value() {
        // the seed had no deadline: 2 takes day 7; from 2 (12 -> 7) the walk shifts 3 by 5 days;
        // 4 has no deadline and takes 3's new value
        let p = plan(
            &[(1, None)],
            &[(1, None), (2, at(12)), (3, at(30)), (4, None)],
            &[(1, &[2]), (2, &[3]), (3, &[4])],
        );
        assert_eq!(
            propagate(&p, at(7)).unwrap(),
            vec![(2, at(7)), (3, at(25)), (4, at(25))]
        );
        // clearing walks on through moves without a deadline
        let p = plan(
            &[(1, at(3))],
            &[(1, at(3)), (2, None), (3, at(9))],
            &[(1, &[2]), (2, &[3])],
        );
        assert_eq!(propagate(&p, None).unwrap(), vec![(3, None)]);
    }

    #[test]
    fn two_seeds_reach_a_shared_neighbour_once() {
        let p = plan(
            &[(1, at(10)), (2, at(20))],
            &[(1, at(10)), (2, at(20)), (3, at(40))],
            &[(1, &[3]), (2, &[3])],
        );
        // the first seed (10 -> 15) shifts 3 by 5 days; the second does not reach it again
        assert_eq!(propagate(&p, at(15)).unwrap(), vec![(3, at(45))]);
    }
}
