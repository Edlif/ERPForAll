// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only purchase terms for an accounting item created from a receipt-backed order.

use std::collections::{BTreeMap, BTreeSet};

use crate::line::PurchaseOrderLine;
use erp_addon_account::moves::AccountMoveLine;
use erp_sdk::prelude::*;
use erp_sdk::{FieldOp, Operation};

pub(crate) const FIELDS: &[&str] = &[
    "purchase_source_line_id",
    "purchase_source_order_id",
    "purchase_ordered_qty",
    "purchase_received_qty",
    "purchase_agreed_price",
    "purchase_agreed_discount",
];

#[derive(Clone, Copy, Default)]
struct PurchaseTerms {
    ordered: Decimal,
    received: Decimal,
    price: Decimal,
    discount: Decimal,
}

#[extend(AccountMoveLine, addon = "purchase")]
struct BillPurchaseSource {
    #[field(
        many2one,
        comodel = "purchase.order.line",
        compute = "_compute_purchase_terms",
        compute_sudo = false,
        readonly = true,
        copy = false,
        groups = "purchase.group_purchase_user",
        string = "Purchase Order Line"
    )]
    purchase_source_line_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "purchase.order",
        compute = "_compute_purchase_terms",
        compute_sudo = false,
        readonly = true,
        groups = "purchase.group_purchase_user",
        string = "Purchase Order"
    )]
    purchase_source_order_id: Option<RecId>,
    #[field(
        float,
        digits = "Product Unit",
        compute = "_compute_purchase_terms",
        compute_sudo = false,
        readonly = true,
        groups = "purchase.group_purchase_user",
        string = "Ordered Quantity"
    )]
    purchase_ordered_qty: Decimal,
    #[field(
        float,
        digits = "Product Unit",
        compute = "_compute_purchase_terms",
        compute_sudo = false,
        readonly = true,
        groups = "purchase.group_purchase_user",
        string = "Received Quantity"
    )]
    purchase_received_qty: Decimal,
    #[field(
        float,
        digits = "Product Price",
        compute = "_compute_purchase_terms",
        compute_sudo = false,
        readonly = true,
        groups = "purchase.group_purchase_user",
        string = "PO Unit Price"
    )]
    purchase_agreed_price: Decimal,
    #[field(
        float,
        digits = "Discount",
        compute = "_compute_purchase_terms",
        compute_sudo = false,
        readonly = true,
        groups = "purchase.group_purchase_user",
        string = "PO Discount (%)"
    )]
    purchase_agreed_discount: Decimal,
}

#[methods(AccountMoveLine, addon = "purchase")]
impl BillPurchaseSource {
    #[compute(depends = ["move_id", "company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_purchase_terms(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountMoveLine>,
    ) -> Result<()> {
        let items = this.as_records();
        let originals = items.origin();
        originals.check_access(tx, Operation::Read).await?;
        let mut links = BTreeMap::<i64, (i64, i64)>::new();
        if !originals.is_empty() {
            // The private association supplies identities. Business data is read as the caller.
            let rows = tx.query(
                "SELECT s.bill_line_id, s.purchase_line_id, s.order_id
                 FROM purchase_direct_bill_line s
                 JOIN account_move_line i ON i.id=s.bill_line_id AND i.move_id=s.bill_id AND i.company_id=s.company_id
                 JOIN purchase_order_line l ON l.id=s.purchase_line_id AND l.order_id=s.order_id
                 JOIN purchase_order p ON p.id=l.order_id AND p.company_id=s.company_id
                 WHERE s.bill_line_id=ANY($1)",
                &[&originals.ids()],
            ).await?;
            for row in rows {
                let error =
                    |e| Error::db("Reading the vendor bill purchase source failed.").with_source(e);
                links.insert(
                    row.try_get(0).map_err(error)?,
                    (
                        row.try_get(1).map_err(error)?,
                        row.try_get(2).map_err(error)?,
                    ),
                );
            }
        }
        let mut terms = BTreeMap::new();
        if !links.is_empty() {
            let env = items.env();
            let allowed_orders: BTreeSet<_> = env
                .records("purchase.order")?
                .browse(links.values().map(|&(_, order)| order))?
                .filtered_access(tx, Operation::Read)
                .await?
                .ids()
                .into_iter()
                .collect();
            links.retain(|_, (_, order)| allowed_orders.contains(order));
            let line_ids: BTreeSet<_> = links.values().map(|&(line, _)| line).collect();
            let readable = env
                .records("purchase.order.line")?
                .browse(line_ids)?
                .filtered_access(tx, Operation::Read)
                .await?;
            if !readable.is_empty() {
                for field in [
                    "order_id",
                    "product_qty",
                    "qty_received",
                    "price_unit",
                    "discount",
                ] {
                    if !tx
                        .has_field_access(env, "purchase.order.line", field, FieldOp::Read)
                        .await?
                    {
                        return Err(Error::access(
                            "You need access to the purchase terms to compare this bill item.",
                        ));
                    }
                }
                let lines = RecordSet::<PurchaseOrderLine>::try_from(readable)?;
                let ordered = lines.mapped(tx, PurchaseOrderLine::PRODUCT_QTY).await?;
                let received = lines.mapped(tx, PurchaseOrderLine::QTY_RECEIVED).await?;
                let prices = lines.mapped(tx, PurchaseOrderLine::PRICE_UNIT).await?;
                let discounts = lines.mapped(tx, PurchaseOrderLine::DISCOUNT).await?;
                for (index, id) in lines.ids().into_iter().enumerate() {
                    terms.insert(
                        id,
                        PurchaseTerms {
                            ordered: ordered[index],
                            received: received[index],
                            price: prices[index],
                            discount: discounts[index],
                        },
                    );
                }
            }
        }
        for (index, record) in items.rec_ids().iter().enumerate() {
            let visible = record
                .as_real()
                .or(record.origin())
                .and_then(|id| links.get(&id.get()))
                .and_then(|&(line, order)| {
                    Some((
                        RecId::from_real(line)?,
                        RecId::from_real(order)?,
                        terms.get(&line)?,
                    ))
                });
            let (line, order, values) = match visible {
                Some((line, order, values)) => (Some(line), Some(order), *values),
                None => (None, None, PurchaseTerms::default()),
            };
            out.set(index, BillPurchaseSource::PURCHASE_SOURCE_LINE_ID, line)?;
            out.set(index, BillPurchaseSource::PURCHASE_SOURCE_ORDER_ID, order)?;
            out.set(
                index,
                BillPurchaseSource::PURCHASE_ORDERED_QTY,
                values.ordered,
            )?;
            out.set(
                index,
                BillPurchaseSource::PURCHASE_RECEIVED_QTY,
                values.received,
            )?;
            out.set(
                index,
                BillPurchaseSource::PURCHASE_AGREED_PRICE,
                values.price,
            )?;
            out.set(
                index,
                BillPurchaseSource::PURCHASE_AGREED_DISCOUNT,
                values.discount,
            )?;
        }
        Ok(())
    }
}
