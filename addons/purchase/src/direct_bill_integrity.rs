// SPDX-License-Identifier: MIT OR Apache-2.0
//! Keep existing draft-edit privileges while preserving the source of a direct bill.

use erp_addon_account::moves::{AccountMove, AccountMoveLine};
use erp_sdk::Operation;
use erp_sdk::prelude::*;

#[extend(AccountMove, addon = "purchase")]
struct DirectBill {}

#[methods(AccountMove, addon = "purchase")]
impl DirectBill {
    #[method(override)]
    async fn write(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        preserve_source(tx, this.as_records(), vals, sup,
            &["company_id","currency_id","partner_id","move_type","journal_id"],
            "SELECT 1 FROM purchase_direct_bill_line s
             JOIN account_move m ON m.id=s.bill_id JOIN purchase_order p ON p.id=s.order_id
             WHERE m.id=ANY($1) AND (m.move_type IS DISTINCT FROM 'in_invoice'
             OR m.company_id IS DISTINCT FROM s.company_id OR m.currency_id IS DISTINCT FROM p.currency_id
             OR m.partner_id IS DISTINCT FROM p.partner_id) LIMIT 1").await
    }
}

#[extend(AccountMoveLine, addon = "purchase")]
struct DirectBillItem {}

#[methods(AccountMoveLine, addon = "purchase")]
impl DirectBillItem {
    #[method(override)]
    async fn write(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        preserve_source(tx, this.as_records(), vals, sup,
            &["move_id","product_id","product_uom_id","display_type"],
            "SELECT 1 FROM purchase_direct_bill_line s
             JOIN account_move_line l ON l.id=s.bill_line_id JOIN purchase_order_line p ON p.id=s.purchase_line_id
             WHERE l.id=ANY($1) AND (l.move_id IS DISTINCT FROM s.bill_id OR l.display_type IS DISTINCT FROM 'product'
             OR l.product_id IS DISTINCT FROM p.product_id OR l.product_uom_id IS DISTINCT FROM p.uom_id) LIMIT 1").await
    }
}

/// Check the final purchase values after product defaults have also set the unit.
pub(crate) async fn check_purchase_items(tx: &mut Transaction, lines: &Records) -> Result<()> {
    tx.flush().await?;
    let changed = tx
        .query(
            "SELECT 1 FROM purchase_direct_bill_line s
         JOIN account_move_line b ON b.id=s.bill_line_id
         JOIN purchase_order_line p ON p.id=s.purchase_line_id
         WHERE p.id=ANY($1) AND (p.product_id IS DISTINCT FROM b.product_id
             OR p.uom_id IS DISTINCT FROM b.product_uom_id) LIMIT 1",
            &[&lines.ids()],
        )
        .await?;
    if !changed.is_empty() {
        return Err(Error::validation(
            "A purchase order item linked to a vendor bill must keep its billed product and unit.",
        ));
    }
    Ok(())
}

async fn preserve_source(
    tx: &mut Transaction,
    records: &Records,
    vals: RawVals,
    sup: Super<(RawVals,), ()>,
    fields: &[&str],
    mismatch: &str,
) -> Result<()> {
    if !fields.iter().any(|field| vals.contains(field)) || records.is_empty() {
        return sup.call(tx, records, (vals,)).await;
    }
    records.check_access(tx, Operation::Write).await?;
    tx.savepoint(async move |tx| {
        let ()=sup.call(tx,records,(vals,)).await?;
        tx.flush().await?;
        if !tx.query(mismatch,&[&records.ids()]).await?.is_empty() {
            return Err(Error::validation("A vendor bill linked to a purchase order must keep its supplier, company, currency, and source item identity."));
        }
        Ok(())
    }).await
}
