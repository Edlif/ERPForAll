// SPDX-License-Identifier: MIT OR Apache-2.0
//! The count buttons that join a sales order and its transfers: "Delivery" on the order, "Sales
//! Order" on the transfer. Both show only documents the caller may read; backorders of the
//! linked transfer count with it.

use erp_addon_sale::{SaleOrder, counts, flat, readable_links, window};
use erp_addon_stock::StockPicking;
use erp_sdk::Operation;
use erp_sdk::prelude::*;

/// Order id, transfer id: the linked transfer and its backorders.
const ORDER_PICKINGS: &str = "SELECT l.order_id::int8, l.picking_id::int8 FROM sale_stock_delivery_link l \
    WHERE l.order_id = ANY($1) \
    UNION \
    SELECT l.order_id::int8, m.picking_id::int8 FROM stock_move m \
    JOIN stock_move root ON root.id = m.backorder_root_move_id AND root.company_id = m.company_id \
    JOIN sale_stock_delivery_link l ON l.picking_id = root.picking_id AND l.company_id = m.company_id \
    WHERE l.order_id = ANY($1) AND m.picking_id IS NOT NULL";
/// Transfer id, order id.
const PICKING_ORDERS: &str = "SELECT l.picking_id::int8, l.order_id::int8 FROM sale_stock_delivery_link l \
    WHERE l.picking_id = ANY($1) \
    UNION \
    SELECT m.picking_id::int8, l.order_id::int8 FROM stock_move m \
    JOIN stock_move root ON root.id = m.backorder_root_move_id AND root.company_id = m.company_id \
    JOIN sale_stock_delivery_link l ON l.picking_id = root.picking_id AND l.company_id = m.company_id \
    WHERE m.picking_id = ANY($1)";

/// `sale.order` with the count of its transfers.
#[extend(SaleOrder, addon = "sale_stock")]
pub struct SalesOrderDeliveries {
    /// How many readable transfers the order has, backorders included.
    #[field(
        integer,
        compute = "_compute_delivery_count",
        string = "Delivery Count"
    )]
    pub delivery_count: i32,
}

#[methods(SaleOrder, addon = "sale_stock")]
impl SalesOrderDeliveries {
    /// A completed transfer changes the delivered quantities, and a backorder is only made then;
    /// the link row itself is written by SQL, which drops this value ([`crate::delivery`]).
    #[compute(depends = ["state", "company_id", "order_line.qty_delivered"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_delivery_count(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, SaleOrder>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, ORDER_PICKINGS, "stock.picking").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, SalesOrderDeliveries::DELIVERY_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the order's transfers in the transfer list and form.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_delivery(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, ORDER_PICKINGS, "stock.picking").await?;
        Ok(window("Delivery", "stock.picking", Vec::new(), flat(found)))
    }
}

/// `stock.picking` with the count of the sales orders it delivers.
#[extend(StockPicking, addon = "sale_stock")]
pub struct SalesPickingOrders {
    /// 1 when the transfer, or the transfer it is a backorder of, delivers a readable order.
    #[field(
        integer,
        compute = "_compute_sale_order_count",
        string = "Sales Order Count"
    )]
    pub sale_order_count: i32,
}

#[methods(StockPicking, addon = "sale_stock")]
impl SalesPickingOrders {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_sale_order_count(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockPicking>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, PICKING_ORDERS, "sale.order").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, SalesPickingOrders::SALE_ORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the sales order this transfer delivers.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_sale_order(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PICKING_ORDERS, "sale.order").await?;
        Ok(window("Sales Order", "sale.order", Vec::new(), flat(found)))
    }
}
