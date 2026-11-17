// SPDX-License-Identifier: MIT OR Apache-2.0
//! The status of a transfer is kept by the stock triggers (`move_counter_sql.rs`), not written
//! through the ORM, so its changes are not captured by the flush. The status operations (confirm,
//! check availability, validate, cancel) read the states before and after and report the changes
//! with `Transaction::track_external`; a tracking addon (the stock mail bridge) turns them into the
//! transfer's history. Without such an addon the field is not tracked and nothing is read.

use erp_sdk::prelude::*;

const STATES: &str =
    "SELECT id::int8, state::text FROM stock_picking WHERE id = ANY($1::int8[]) ORDER BY id";

fn decode(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("A transfer status could not be read for its history.").with_source(e)
}

/// The states of `records` before a status operation, or `None` when no installed addon tracks
/// them (no statement then).
pub(crate) async fn before(
    tx: &mut Transaction,
    records: &Records,
) -> Result<Option<Vec<(i64, Option<String>)>>> {
    if records.is_empty() || !tx.tracks(records, "state") {
        return Ok(None);
    }
    tx.flush().await?;
    states(tx, records).await.map(Some)
}

/// Report the state changes of `records` since [`before`].
pub(crate) async fn report(
    tx: &mut Transaction,
    records: &Records,
    before: Option<Vec<(i64, Option<String>)>>,
) -> Result<()> {
    let Some(before) = before else {
        return Ok(());
    };
    tx.flush().await?;
    let after = states(tx, records).await?;
    let cell = |state: Option<String>| state.map_or(Cell::Null, |s| Cell::Str(s.into()));
    let before: std::collections::BTreeMap<i64, Option<String>> = before.into_iter().collect();
    let changes: Vec<(i64, Cell, Cell)> = after
        .into_iter()
        .filter_map(|(id, new)| {
            let old = before.get(&id)?.clone();
            (old != new).then(|| (id, cell(old), cell(new)))
        })
        .collect();
    tx.track_external(records, "state", changes)
}

async fn states(tx: &mut Transaction, records: &Records) -> Result<Vec<(i64, Option<String>)>> {
    tx.query(STATES, &[&records.ids()])
        .await?
        .iter()
        .map(|row| {
            Ok((
                row.try_get(0).map_err(decode)?,
                row.try_get(1).map_err(decode)?,
            ))
        })
        .collect()
}
