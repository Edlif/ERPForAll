// SPDX-License-Identifier: MIT OR Apache-2.0
//! The invoice and delivery status of an order, and closing its invoicing by hand (B3;
//! ).

use std::collections::BTreeMap;

use erp_sdk::prelude::*;

use crate::line::{LineFacts, SaleOrderLine, facts, product_facts};
use crate::order::{SaleOrder, headers};

/// Each order's counted lines with their invoice status.
type LinesByOrder = BTreeMap<RecId, Vec<(LineFacts, Option<Arc<str>>)>>;

/// The lines of each order of `this` that count for its statuses: no section, note or down
/// payment.
async fn order_lines(this: &RecordSet<SaleOrder>, tx: &mut Transaction) -> Result<LinesByOrder> {
    let lines = this.mapped(tx, SaleOrder::ORDER_LINE).await?;
    let set = RecordSet::<SaleOrderLine>::try_from(
        this.env()
            .records("sale.order.line")?
            .browse_ids(lines.iter().flatten().copied()),
    )?;
    let statuses = set.mapped(tx, SaleOrderLine::INVOICE_STATUS).await?;
    let mut by_order: LinesByOrder = BTreeMap::new();
    for (line, status) in facts(&set, tx).await?.into_iter().zip(statuses) {
        if line.display_type.is_some() || line.downpayment {
            continue;
        }
        if let Some(order) = line.order {
            by_order.entry(order).or_default().push((line, status));
        }
    }
    Ok(by_order)
}

/// Whether a counted line has the invoice status `status`.
fn is(status: &'static str) -> impl Fn(&(LineFacts, Option<Arc<str>>)) -> bool {
    move |(_, own)| own.as_deref() == Some(status)
}

#[methods(SaleOrder, addon = "sale")]
impl SaleOrder {
    /// `no` outside a confirmed order; `invoiced` when its invoicing is closed; otherwise from
    /// the lines: a line to invoice makes the order `to invoice`, unless lines
    /// with nothing to invoice remain and every line to invoice refuses to be invoiced alone;
    /// all lines invoiced, `invoiced`; invoiced and upselling only, `upselling`; else `no`.
    #[compute(depends = ["state", "invoicing_closed", "order_line.invoice_status"])]
    async fn _compute_invoice_status(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, SaleOrder>,
    ) -> Result<()> {
        let states = this.mapped(tx, SaleOrder::STATE).await?;
        let closed = this.mapped(tx, SaleOrder::INVOICING_CLOSED).await?;
        let lines = order_lines(this, tx).await?;
        let line_model = this.env().records("sale.order.line")?;
        let ids = this.as_records().rec_ids().to_vec();
        for (index, order) in ids.iter().enumerate() {
            let own = lines.get(order).map_or(&[][..], Vec::as_slice);
            let status = if states[index].as_deref() != Some("sale") {
                "no"
            } else if closed[index] {
                "invoiced"
            } else if own.iter().any(is("to invoice")) {
                let invoiceable: Vec<RecId> = own
                    .iter()
                    .filter(|line| is("to invoice")(line))
                    .map(|(line, _)| line.id)
                    .collect();
                let alone: Records = line_model
                    .browse_ids(invoiceable)
                    .call(tx, "_can_be_invoiced_alone", ())
                    .await?;
                if own.iter().any(is("no")) && alone.is_empty() {
                    "no"
                } else {
                    "to invoice"
                }
            } else if !own.is_empty() && own.iter().all(is("invoiced")) {
                "invoiced"
            } else if !own.is_empty()
                && own
                    .iter()
                    .all(|line| is("invoiced")(line) || is("upselling")(line))
            {
                "upselling"
            } else {
                "no"
            };
            out.set(
                index,
                SaleOrder::INVOICE_STATUS,
                Some(Arc::<str>::from(status)),
            )?;
        }
        Ok(())
    }

    /// Over the goods lines of a confirmed order: `full` when every line has
    /// its ordered quantity delivered (a negative quantity: returned), `partial` when something
    /// was delivered, `pending` otherwise; empty outside a confirmed order or without goods lines.
    /// `sale_stock` replaces the value.
    #[compute(depends = ["state", "order_line.qty_delivered", "order_line.product_uom_qty", "order_line.product_id"])]
    async fn _compute_delivery_status(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, SaleOrder>,
    ) -> Result<()> {
        let states = this.mapped(tx, SaleOrder::STATE).await?;
        let lines = order_lines(this, tx).await?;
        let products = lines
            .values()
            .flatten()
            .filter_map(|(line, _)| line.product)
            .collect::<Vec<_>>();
        let products = product_facts(tx, this.env(), products).await?;
        let ids = this.as_records().rec_ids().to_vec();
        for (index, order) in ids.iter().enumerate() {
            let goods: Vec<&LineFacts> = lines
                .get(order)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(|(line, _)| line)
                .filter(|line| {
                    line.product
                        .and_then(|product| products.get(&product))
                        .is_some_and(|facts| facts.kind.as_deref() == Some("consu"))
                })
                .collect();
            let status = if states[index].as_deref() != Some("sale") || goods.is_empty() {
                None
            } else if goods.iter().all(|line| {
                if line.ordered > Decimal::ZERO {
                    line.delivered >= line.ordered
                } else {
                    line.delivered <= line.ordered
                }
            }) {
                Some("full")
            } else if goods.iter().any(|line| !line.delivered.is_zero()) {
                Some("partial")
            } else {
                Some("pending")
            };
            out.set(
                index,
                SaleOrder::DELIVERY_STATUS,
                status.map(Arc::<str>::from),
            )?;
        }
        Ok(())
    }

    /// Closes the invoicing of confirmed orders by hand; any other order refuses the whole call.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_close_invoicing(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        records.lock_for_update(tx, false).await?;
        let orders = headers(tx, records).await?;
        let refused: Vec<String> = orders
            .iter()
            .filter(|order| order.state != "sale")
            .map(|order| order.describe())
            .collect();
        if !refused.is_empty() {
            return Err(Error::user(format!(
                "Only a confirmed sales order can have its invoicing closed: {}.",
                refused.join(", ")
            )));
        }
        records
            .write_base(
                tx,
                RawVals::new().set_cell("invoicing_closed", Cell::Bool(true)),
            )
            .await
    }

    /// Reopens the invoicing of orders closed by hand.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_reopen_order(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        records.lock_for_update(tx, false).await?;
        let refused: Vec<String> = headers(tx, records)
            .await?
            .iter()
            .filter(|order| order.state != "sale")
            .map(|order| order.describe())
            .collect();
        if !refused.is_empty() {
            return Err(Error::user(format!(
                "Only a confirmed sales order can have its invoicing reopened: {}.",
                refused.join(", ")
            )));
        }
        records
            .write_base(
                tx,
                RawVals::new().set_cell("invoicing_closed", Cell::Bool(false)),
            )
            .await
    }
}
