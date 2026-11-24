// SPDX-License-Identifier: MIT OR Apache-2.0
//! The count buttons that join a purchase order and its receipts: "Receipt" on the order,
//! "Purchase Order" on the receipt. Both show only documents the caller may read; backorders of
//! the linked receipt count with it.

use erp_addon_purchase::order::PurchaseOrder;
use erp_addon_purchase::{counts, flat, readable_links, window};
use erp_addon_stock::StockPicking;
use erp_sdk::Operation;
use erp_sdk::prelude::*;

/// Order id, receipt id: the linked receipt and its backorders.
const ORDER_PICKINGS: &str = "SELECT l.order_id::int8, l.picking_id::int8 FROM purchase_stock_receipt_link l \
    WHERE l.order_id = ANY($1) \
    UNION \
    SELECT l.order_id::int8, m.picking_id::int8 FROM stock_move m \
    JOIN stock_move root ON root.id = m.backorder_root_move_id AND root.company_id = m.company_id \
    JOIN purchase_stock_receipt_link l ON l.picking_id = root.picking_id AND l.company_id = m.company_id \
    WHERE l.order_id = ANY($1) AND m.picking_id IS NOT NULL";
/// Transfer id, order id.
const PICKING_ORDERS: &str = "SELECT l.picking_id::int8, l.order_id::int8 FROM purchase_stock_receipt_link l \
    WHERE l.picking_id = ANY($1) \
    UNION \
    SELECT m.picking_id::int8, l.order_id::int8 FROM stock_move m \
    JOIN stock_move root ON root.id = m.backorder_root_move_id AND root.company_id = m.company_id \
    JOIN purchase_stock_receipt_link l ON l.picking_id = root.picking_id AND l.company_id = m.company_id \
    WHERE m.picking_id = ANY($1)";

/// `purchase.order` with the count of its receipts.
#[extend(PurchaseOrder, addon = "purchase_stock")]
pub struct PurchaseOrderReceipts {
    /// How many readable receipts the order has, backorders included.
    #[field(integer, compute = "_compute_receipt_count", string = "Receipt Count")]
    pub receipt_count: i32,
}

#[methods(PurchaseOrder, addon = "purchase_stock")]
impl PurchaseOrderReceipts {
    #[compute(depends = ["state", "company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_receipt_count(
        this: &RecordSet<PurchaseOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, PurchaseOrder>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, ORDER_PICKINGS, "stock.picking").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, PurchaseOrderReceipts::RECEIPT_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the order's receipts in the receipt list and form.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_receipt(
        this: &RecordSet<PurchaseOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, ORDER_PICKINGS, "stock.picking").await?;
        Ok(window("Receipts", "stock.picking", Vec::new(), flat(found)))
    }
}

/// `stock.picking` with the count of the purchase orders it receives.
#[extend(StockPicking, addon = "purchase_stock")]
pub struct PurchasePickingOrders {
    /// 1 when the receipt, or the receipt it is a backorder of, receives a readable order.
    #[field(integer, compute = "_compute_purchase_order_count", string = "Purchase Order Count")]
    pub purchase_order_count: i32,
}

#[methods(StockPicking, addon = "purchase_stock")]
impl PurchasePickingOrders {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_purchase_order_count(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockPicking>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, PICKING_ORDERS, "purchase.order").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, PurchasePickingOrders::PURCHASE_ORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the purchase order this receipt receives.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_purchase_order(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PICKING_ORDERS, "purchase.order").await?;
        Ok(window("Purchase Order", "purchase.order", Vec::new(), flat(found)))
    }
}
