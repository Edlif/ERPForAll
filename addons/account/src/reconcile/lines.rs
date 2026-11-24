// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a journal item reads of its reconciliation (residuals, settled flag, matching number), the
//! item actions that match and unmatch, and the deletes of matches and settlements, which go
//! through the reconciliation service.

use std::collections::BTreeMap;

use erp_sdk::prelude::*;

use super::{AccountFullReconcile, AccountPartialReconcile, apply};
use crate::moves::AccountMoveLine;

#[methods(AccountMoveLine, addon = "account")]
impl AccountMoveLine {
    /// The residuals, the settled flag and the matching number of each item, in one read for the
    /// batch (with A7): from the open-item row when the item is a member of an open
    /// component, zero when it is settled, else its own amounts; the number is the settlement id,
    /// else `P` and the component's smallest match id, else empty (
    /// -026).
    #[compute(depends = ["balance", "amount_currency", "full_reconcile_id"])]
    async fn _compute_amount_residual(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountMoveLine>,
    ) -> Result<()> {
        let ids: Vec<i64> = this
            .as_records()
            .rec_ids()
            .iter()
            .filter_map(|id| id.as_real().map(|real| real.get()))
            .collect();
        let rows = tx
            .query(
                "SELECT l.id::int8, l.full_reconcile_id::int8, o.amount_residual, \
                 o.amount_residual_currency, o.component_id::int8 \
                 FROM account_move_line l LEFT JOIN account_open_item o ON o.line_id = l.id \
                 WHERE l.id = ANY($1::int8[])",
                &[&ids],
            )
            .await?;
        let read = |e: &dyn std::fmt::Display| {
            Error::db(format!("A journal item's residual could not be read: {e}"))
        };
        // `None`: an unmatched item, which reads its own amounts as the ORM holds them (an amount
        // written in this transaction may not be flushed yet)
        let mut found: BTreeMap<i64, Option<(Decimal, Decimal, Option<String>)>> = BTreeMap::new();
        for row in &rows {
            let id: i64 = row.try_get(0).map_err(|e| read(&e))?;
            let full: Option<i64> = row.try_get(1).map_err(|e| read(&e))?;
            let residual: Option<Decimal> = row.try_get(2).map_err(|e| read(&e))?;
            let residual_currency: Option<Decimal> = row.try_get(3).map_err(|e| read(&e))?;
            let component: Option<i64> = row.try_get(4).map_err(|e| read(&e))?;
            let value = match (full, residual, residual_currency, component) {
                (Some(full), _, _, _) => {
                    Some((Decimal::ZERO, Decimal::ZERO, Some(full.to_string())))
                }
                (None, Some(r), Some(c), Some(label)) => Some((r, c, Some(format!("P{label}")))),
                _ => None,
            };
            found.insert(id, value);
        }
        let balances = this.mapped(tx, AccountMoveLine::BALANCE).await?;
        let amounts = this.mapped(tx, AccountMoveLine::AMOUNT_CURRENCY).await?;
        for (index, id) in this.as_records().rec_ids().iter().enumerate() {
            // an unsaved or unmatched item reads its own amounts
            let own = || {
                let balance = balances.get(index).copied().unwrap_or_default();
                let amount = amounts.get(index).copied().unwrap_or_default();
                (balance, amount, None)
            };
            let (residual, residual_currency, number) = match id.as_real() {
                Some(real) => found
                    .get(&real.get())
                    .cloned()
                    .ok_or_else(|| {
                        Error::missing(format!("Journal item {} no longer exists.", real.get()))
                    })?
                    .unwrap_or_else(own),
                None => own(),
            };
            out.set(index, AccountMoveLine::AMOUNT_RESIDUAL, residual)?;
            out.set(
                index,
                AccountMoveLine::AMOUNT_RESIDUAL_CURRENCY,
                residual_currency,
            )?;
            out.set(
                index,
                AccountMoveLine::RECONCILED,
                residual.is_zero() && residual_currency.is_zero(),
            )?;
            out.set(
                index,
                AccountMoveLine::MATCHING_NUMBER,
                number.map(Arc::<str>::from),
            )?;
        }
        Ok(())
    }

    /// Matches the items with each other as far as they offset; see
    /// [`apply::reconcile_lines`].
    #[method]
    #[rpc(effect = write)]
    pub async fn reconcile(this: &RecordSet<AccountMoveLine>, tx: &mut Transaction) -> Result<()> {
        apply::reconcile_lines(tx, this.as_records()).await
    }

    /// Removes every match of the items (reference `remove_move_reconcile`); see
    /// [`apply::unreconcile_partials`].
    #[method]
    #[rpc(effect = write)]
    pub async fn remove_move_reconcile(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let ids = this.ids();
        if ids.is_empty() {
            return Ok(());
        }
        this.as_records()
            .check_access(tx, erp_sdk::Operation::Write)
            .await?;
        tx.flush().await?;
        let rows = tx
            .query(
                "SELECT id::int8 FROM account_partial_reconcile \
                 WHERE debit_move_id = ANY($1::int8[]) OR credit_move_id = ANY($1::int8[]) \
                 ORDER BY id",
                &[&ids],
            )
            .await?;
        let partials: Vec<i64> = rows
            .iter()
            .map(|row| {
                row.try_get::<_, i64>(0)
                    .map_err(|e| Error::db(format!("A match could not be read: {e}")))
            })
            .collect::<Result<_>>()?;
        let records = this
            .env()
            .records("account.partial.reconcile")?
            .browse(partials)?;
        apply::unreconcile_partials(tx, &records).await
    }
}

#[methods(AccountPartialReconcile, addon = "account")]
impl AccountPartialReconcile {
    /// Deleting a match is unmatching it: residuals, components, settlements and invoice statuses
    /// follow ([`apply::unreconcile_partials`]). Sealed: no later override goes around it.
    #[method(override, sealed)]
    async fn unlink(
        this: &RecordSet<AccountPartialReconcile>,
        tx: &mut Transaction,
        _sup: Super,
    ) -> Result<()> {
        apply::unreconcile_partials(tx, this.as_records()).await
    }
}

#[methods(AccountFullReconcile, addon = "account")]
impl AccountFullReconcile {
    /// A settlement goes with its matches: deleting it alone is refused.
    #[method(override, sealed)]
    async fn unlink(
        _this: &RecordSet<AccountFullReconcile>,
        _tx: &mut Transaction,
        _sup: Super,
    ) -> Result<()> {
        Err(Error::user(
            "A settlement cannot be deleted on its own; unmatch its journal items instead.",
        ))
    }
}
