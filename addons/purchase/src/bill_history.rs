// SPDX-License-Identifier: MIT OR Apache-2.0
//! The caller-visible, locked accounting quantities already attributed to an order.

use super::direct_bill::{SOURCE_LIMIT, authorize_fields, decode};
use erp_sdk::prelude::*;
use std::collections::BTreeSet;

/// The linked documents of an order, read and locked. The quantities themselves are the lines'
/// `qty_to_invoice` ([`crate::billing`]).
#[derive(Default)]
pub(super) struct History {
    pub latest_active: Option<i64>,
    pub link_count: usize,
}

pub(super) async fn read(
    tx: &mut Transaction,
    env: &Env,
    company: i64,
    order: i64,
    limit: i64,
) -> Result<History> {
    let probe = limit
        .checked_add(1)
        .ok_or_else(|| Error::invariant("Invalid direct bill source limit."))?;
    let rows = tx
        .query(
            "SELECT id, bill_id, bill_line_id, purchase_line_id FROM purchase_direct_bill_line
        WHERE company_id=$1 AND order_id=$2 ORDER BY id LIMIT $3",
            &[&company, &order, &probe],
        )
        .await?;
    if rows.is_empty() {
        return Ok(History::default());
    }
    let mut links = Vec::<i64>::with_capacity(rows.len());
    let mut bills = BTreeSet::<i64>::new();
    let mut sources = BTreeSet::<i64>::new();
    let mut items = BTreeSet::<i64>::new();
    for row in &rows {
        links.push(row.try_get(0).map_err(decode)?);
        bills.insert(row.try_get(1).map_err(decode)?);
        sources.insert(row.try_get(3).map_err(decode)?);
        items.extend(row.try_get::<_, Option<i64>>(2).map_err(decode)?);
    }
    let bills = env.records("account.move")?.browse(bills)?;
    let items = env.records("account.move.line")?.browse(items)?;
    authorize_fields(
        tx,
        &env.records("purchase.order.line")?.browse(sources)?,
        &["qty_to_invoice"],
    )
    .await?;
    authorize_fields(tx, &bills, &["state"]).await?;
    authorize_fields(tx, &items, &["quantity"]).await?;
    if i64::try_from(links.len()).map_err(decode)? > limit {
        return Err(Error::validation(format!(
            "The direct bill history exceeds {SOURCE_LIMIT} ({limit}) source links."
        )));
    }
    // A PO lock serializes generators. Locks on the existing documents and items also
    // serialize the calculation with ordinary Accounting edits, posting and cancellation.
    bills.lock_for_update(tx, true).await?;
    items.lock_for_update(tx, true).await?;
    let rows = tx
        .query(
            "SELECT s.purchase_line_id, m.id, m.state, s.bill_line_id, item.quantity
        FROM purchase_direct_bill_line s JOIN account_move m ON m.id=s.bill_id
        LEFT JOIN account_move_line item ON item.id=s.bill_line_id
        WHERE s.id=ANY($1)",
            &[&links],
        )
        .await?;
    let mut history = History {
        link_count: links.len(),
        ..History::default()
    };
    for row in rows {
        let state: &str = row.try_get(2).map_err(decode)?;
        match state {
            "cancel" => continue,
            "draft" | "posted" => (),
            _ => {
                return Err(Error::invariant(
                    "A linked vendor bill has an unknown accounting state.",
                ));
            }
        }
        let bill: i64 = row.try_get(1).map_err(decode)?;
        history.latest_active = history.latest_active.max(Some(bill));
        if row.try_get::<_, Option<i64>>(3).map_err(decode)?.is_none() {
            continue; // A draft item removed in Accounting keeps only its document association.
        }
        let qty: Decimal = row.try_get(4).map_err(decode)?;
        if qty < Decimal::ZERO {
            return Err(Error::user(
                "A negative linked quantity requires a vendor credit review before billing more goods.",
            ));
        }
    }
    Ok(history)
}
