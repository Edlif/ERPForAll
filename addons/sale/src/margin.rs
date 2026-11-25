// SPDX-License-Identifier: MIT OR Apache-2.0
//! Profit margins on sales order lines and orders (`purchase_price`, `margin`, `margin_percent`).

use erp_sdk::prelude::*;

use crate::line::SaleOrderLine;
use crate::order::SaleOrder;

/// Pure exact-decimal margin calculation: returns `(margin, margin_percent)`.
pub fn compute_line_margin(
    subtotal: Decimal,
    quantity: Decimal,
    purchase_price: Decimal,
) -> (Decimal, Decimal) {
    let cost_total = purchase_price * quantity;
    let margin = (subtotal - cost_total).round_dp(2);
    let pct = if subtotal > Decimal::ZERO {
        ((margin * Decimal::from(100)) / subtotal).round_dp(2)
    } else {
        Decimal::ZERO
    };
    (margin, pct)
}

/// Pure exact-decimal order margin percentage helper.
pub fn compute_order_margin_percent(margin: Decimal, amount_untaxed: Decimal) -> Decimal {
    if amount_untaxed > Decimal::ZERO {
        ((margin * Decimal::from(100)) / amount_untaxed).round_dp(2)
    } else {
        Decimal::ZERO
    }
}

#[methods(SaleOrderLine, addon = "sale")]
impl SaleOrderLine {
    /// Default unit cost from the product's standard cost, converted by the order's currency rate.
    #[compute(depends = ["product_id", "order_id.currency_rate"])]
    async fn _compute_purchase_price(
        this: &RecordSet<SaleOrderLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, SaleOrderLine>,
    ) -> Result<()> {
        let products = this.mapped(tx, SaleOrderLine::PRODUCT_ID).await?;
        let orders = this.mapped(tx, SaleOrderLine::ORDER_ID).await?;
        let env = this.env().clone();
        for (index, (prod, order)) in products.iter().zip(&orders).enumerate() {
            let Some(pid) = prod else {
                out.set(index, SaleOrderLine::PURCHASE_PRICE, Decimal::ZERO)?;
                continue;
            };
            let std_price = match env
                .records("product.product")?
                .browse_ids([*pid])
                .cell(tx, "standard_price")
                .await
            {
                Ok(Cell::Decimal(d)) => d,
                _ => Decimal::ZERO,
            };
            let rate = match order {
                Some(oid) => match env
                    .records("sale.order")?
                    .browse_ids([*oid])
                    .cell(tx, "currency_rate")
                    .await
                {
                    Ok(Cell::Decimal(r)) if r > Decimal::ZERO => r,
                    _ => Decimal::ONE,
                },
                None => Decimal::ONE,
            };
            let cost = (std_price * rate).round_dp(2);
            out.set(index, SaleOrderLine::PURCHASE_PRICE, cost)?;
        }
        Ok(())
    }

    /// Exact-decimal line margin and margin percentage.
    #[compute(depends = ["price_subtotal", "product_uom_qty", "purchase_price", "display_type", "is_downpayment"])]
    async fn _compute_margin(
        this: &RecordSet<SaleOrderLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, SaleOrderLine>,
    ) -> Result<()> {
        let subtotals = this.mapped(tx, SaleOrderLine::PRICE_SUBTOTAL).await?;
        let qtys = this.mapped(tx, SaleOrderLine::PRODUCT_UOM_QTY).await?;
        let costs = this.mapped(tx, SaleOrderLine::PURCHASE_PRICE).await?;
        let kinds = this.mapped(tx, SaleOrderLine::DISPLAY_TYPE).await?;
        let dps = this.mapped(tx, SaleOrderLine::IS_DOWNPAYMENT).await?;
        for (index, ((((sub, qty), cost), kind), is_dp)) in subtotals
            .into_iter()
            .zip(qtys)
            .zip(costs)
            .zip(kinds)
            .zip(dps)
            .enumerate()
        {
            if kind.is_some() || is_dp {
                out.set(index, SaleOrderLine::MARGIN, Decimal::ZERO)?;
                out.set(index, SaleOrderLine::MARGIN_PERCENT, Decimal::ZERO)?;
            } else {
                let (m, pct) = compute_line_margin(sub, qty, cost);
                out.set(index, SaleOrderLine::MARGIN, m)?;
                out.set(index, SaleOrderLine::MARGIN_PERCENT, pct)?;
            }
        }
        Ok(())
    }
}

#[methods(SaleOrder, addon = "sale")]
impl SaleOrder {
    /// Total exact-decimal profit margin across all accountable product lines.
    #[compute(depends = ["order_line.margin", "order_line.price_subtotal", "order_line.purchase_price", "amount_untaxed"])]
    async fn _compute_margin(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, SaleOrder>,
    ) -> Result<()> {
        let untaxed = this.mapped(tx, SaleOrder::AMOUNT_UNTAXED).await?;
        let ids = this.as_records().ids();
        let rows = if ids.is_empty() {
            Vec::new()
        } else {
            tx.query(
                "SELECT order_id::int8, \
                        COALESCE(SUM(COALESCE(price_subtotal, 0) - COALESCE(purchase_price, 0) * COALESCE(product_uom_qty, 0)), 0) \
                 FROM sale_order_line \
                 WHERE order_id = ANY($1) AND display_type IS NULL AND NOT COALESCE(is_downpayment, false) \
                 GROUP BY order_id",
                &[&ids],
            )
            .await?
        };
        let mut by_order = std::collections::BTreeMap::new();
        for row in rows {
            let oid: i64 = row
                .try_get(0)
                .map_err(|e| Error::db(format!("margin sum: {e}")))?;
            let total: Decimal = row
                .try_get(1)
                .map_err(|e| Error::db(format!("margin sum: {e}")))?;
            by_order.insert(oid, total.round_dp(2));
        }
        for (index, (oid, unt)) in ids.iter().zip(untaxed).enumerate() {
            let margin = by_order.get(oid).copied().unwrap_or(Decimal::ZERO);
            let pct = compute_order_margin_percent(margin, unt);
            out.set(index, SaleOrder::MARGIN, margin)?;
            out.set(index, SaleOrder::MARGIN_PERCENT, pct)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_line_margin, compute_order_margin_percent};
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn margin_calculation_is_exact_decimal() {
        let (margin, pct) = compute_line_margin(
            Decimal::from_str("250.00").unwrap(),
            Decimal::from(5),
            Decimal::from_str("32.50").unwrap(),
        );
        // cost = 162.50, margin = 87.50, pct = 87.50 / 250.00 = 35.00%
        assert_eq!(margin, Decimal::from_str("87.50").unwrap());
        assert_eq!(pct, Decimal::from_str("35.00").unwrap());

        let order_pct = compute_order_margin_percent(
            Decimal::from_str("87.50").unwrap(),
            Decimal::from_str("250.00").unwrap(),
        );
        assert_eq!(order_pct, Decimal::from_str("35.00").unwrap());
        assert_eq!(
            compute_order_margin_percent(Decimal::ZERO, Decimal::ZERO),
            Decimal::ZERO
        );
    }
}
