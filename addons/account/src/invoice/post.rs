// SPDX-License-Identifier: MIT OR Apache-2.0
//! The posting service (`action_post`) calls [`check_postable`] and
//! [`before_post`]; both do nothing for entries that are not invoices.

use std::collections::BTreeSet;

use erp_sdk::prelude::*;

use super::{is_invoice, is_sale};
use crate::moves::id_cell;

fn text(cell: &Cell) -> Option<Arc<str>> {
    match cell {
        Cell::Str(text) => Some(text.clone()),
        _ => None,
    }
}

fn decimal(cell: &Cell) -> Result<Decimal> {
    match cell {
        Cell::Null | Cell::Bool(false) => Ok(Decimal::ZERO),
        Cell::Decimal(value) => Ok(*value),
        other => Err(Error::invariant(format!(
            "an invoice total holds {other:?} instead of an amount"
        ))),
    }
}

/// Refuses the invoices among `moves` that cannot be confirmed, with one message per problem in
/// one error: no customer or vendor, no product line (partial:
/// invoices only), a negative total and a vendor document without its bill date.
/// Entries that are not invoices pass.
///
/// # Errors
///
/// A user error listing every refused invoice and why.
pub async fn check_postable(tx: &mut Transaction, env: &Env, moves: &[i64]) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    let records = env.records("account.move")?.browse(moves.iter().copied())?;
    let types = records.cells(tx, "move_type").await?;
    let partners = records.cells(tx, "partner_id").await?;
    let totals = records.cells(tx, "amount_total").await?;
    let dates = records.cells(tx, "invoice_date").await?;
    let ids = records.ids();
    let invoices: Vec<i64> = ids
        .iter()
        .zip(&types)
        .filter(|(_, t)| text(t).is_some_and(|t| is_invoice(&t)))
        .map(|(id, _)| *id)
        .collect();
    if invoices.is_empty() {
        return Ok(());
    }
    let with_lines: BTreeSet<i64> = {
        let lines = env
            .records("account.move.line")?
            .search(
                tx,
                &Domain::and([
                    Domain::condition("move_id", Operator::In, invoices.clone())?,
                    Domain::condition("display_type", Operator::Eq, "product")?,
                ]),
            )
            .await?;
        let mut owners = BTreeSet::new();
        for cell in lines.cells(tx, "move_id").await? {
            if let Some(id) = id_cell(Some(&cell), "move_id")? {
                owners.insert(id);
            }
        }
        owners
    };
    let mut problems = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let Some(kind) = types.get(i).and_then(text).filter(|t| is_invoice(t)) else {
            continue;
        };
        let sale = is_sale(&kind);
        let what = if sale { "Invoice" } else { "Bill" };
        if id_cell(partners.get(i), "partner_id")?.is_none() {
            let who = if sale { "customer" } else { "vendor" };
            problems.push(format!("{what} {id}: choose the {who} before confirming."));
        }
        if !with_lines.contains(id) {
            problems.push(format!(
                "{what} {id}: add at least one product line before confirming."
            ));
        }
        if totals.get(i).map(decimal).transpose()?.unwrap_or_default() < Decimal::ZERO {
            problems.push(format!(
                "{what} {id}: the total is negative; record a credit note instead."
            ));
        }
        if !sale && !matches!(dates.get(i), Some(Cell::Date(_))) {
            problems.push(format!(
                "{what} {id}: enter the bill date before confirming."
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::user(problems.join("\n")))
    }
}

/// Gives the customer documents among `moves` that have no invoice date today's date in the
/// user's timezone (partial: the rate refresh comes with the currency slice);
/// the write runs the invoice line sync, so the accounting and due dates follow. Vendor documents
/// keep the date their vendor printed: [`check_postable`] asks for it.
///
/// # Errors
///
/// The errors of the write.
pub async fn before_post(tx: &mut Transaction, env: &Env, moves: &[i64]) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    let records = env.records("account.move")?.browse(moves.iter().copied())?;
    let types = records.cells(tx, "move_type").await?;
    let dates = records.cells(tx, "invoice_date").await?;
    let undated: Vec<i64> = records
        .ids()
        .into_iter()
        .zip(types.iter().zip(&dates))
        .filter(|(_, (kind, date))| {
            text(kind).is_some_and(|k| is_sale(&k)) && !matches!(date, Cell::Date(_))
        })
        .map(|(id, _)| id)
        .collect();
    if undated.is_empty() {
        return Ok(());
    }
    let today = env.context_today()?;
    records
        .browse(undated)?
        .write(
            tx,
            RawVals::new().set_cell("invoice_date", Cell::Date(today)),
        )
        .await
}
