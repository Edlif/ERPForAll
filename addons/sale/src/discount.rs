// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sales Order Discount wizard (`sale.order.discount`): line percentage discount, global
//! percentage discount line, or fixed discount amount line.

use erp_sdk::Operation;
use erp_sdk::prelude::*;
use erp_sdk::serde_json::json;

use crate::order::SaleOrder;

/// Pure exact-decimal calculation for a global percentage discount on an order's untaxed amount.
pub fn compute_global_discount(untaxed_amount: Decimal, percent: Decimal) -> Result<Decimal> {
    if percent <= Decimal::ZERO || percent > Decimal::from(100) {
        return Err(Error::user(
            "Discount percentage must be greater than 0 and at most 100.",
        ));
    }
    if untaxed_amount <= Decimal::ZERO {
        return Err(Error::user(
            "Add at least one positive order line before applying a percentage discount.",
        ));
    }
    Ok((untaxed_amount * percent / Decimal::from(100)).round_dp(2))
}

/// `sale.order.discount`: transient dialog opened from a Sales Order to apply discounts.
#[model(
    name = "sale.order.discount",
    description = "Apply Discount",
    kind = "transient",
    transient_max_age_seconds = 3600,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct SaleOrderDiscount {
    /// Target quotation or sales order.
    #[field(
        many2one,
        comodel = "sale.order",
        required = true,
        ondelete = "cascade",
        string = "Sales Order"
    )]
    pub sale_order_id: Option<RecId>,
    /// Order company.
    #[field(
        many2one,
        comodel = "res.company",
        related = "sale_order_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Order currency.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "sale_order_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// How the discount is applied.
    #[field(
        selection,
        selection = [
            ("sol_discount", "On All Order Lines (%)"),
            ("so_discount", "Global Discount (%)"),
            ("amount", "Fixed Amount"),
        ],
        required = true,
        default = "sol_discount",
        string = "Discount Type"
    )]
    pub discount_type: Option<Arc<str>>,
    /// Discount percentage (used when `discount_type` is `sol_discount` or `so_discount`).
    #[field(float, digits = (16, 2), default = 10, string = "Discount (%)")]
    pub discount_percentage: Decimal,
    /// Fixed discount amount (used when `discount_type` is `amount`).
    #[field(monetary, currency_field = "currency_id", string = "Amount")]
    pub discount_amount: Decimal,
}

#[methods(SaleOrderDiscount, addon = "sale")]
impl SaleOrderDiscount {
    #[method]
    #[rpc(effect = write)]
    pub async fn action_apply_discount(
        this: &RecordSet<SaleOrderDiscount>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let wizard = this.ensure_one()?;
        wizard
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let order_id = wizard
            .get(tx, SaleOrderDiscount::SALE_ORDER_ID)
            .await?
            .ok_or_else(|| Error::validation("Choose a sales order."))?
            .get();
        let kind = wizard
            .get(tx, SaleOrderDiscount::DISCOUNT_TYPE)
            .await?
            .unwrap_or_else(|| Arc::from("sol_discount"));
        let pct = wizard
            .get(tx, SaleOrderDiscount::DISCOUNT_PERCENTAGE)
            .await?;
        let fixed_amt = wizard.get(tx, SaleOrderDiscount::DISCOUNT_AMOUNT).await?;

        let env = wizard
            .env()
            .with_context_replaced(erp_sdk::Context::default());
        let order_rec = env.records("sale.order")?.browse([order_id])?;
        order_rec.check_access(tx, Operation::Write).await?;

        let msg = match kind.as_ref() {
            "sol_discount" => {
                if pct < Decimal::ZERO || pct > Decimal::from(100) {
                    return Err(Error::user(
                        "Discount percentage must be between 0 and 100.",
                    ));
                }
                let lines = env
                    .records("sale.order.line")?
                    .search(
                        tx,
                        &Domain::and([
                            Domain::condition("order_id", Operator::Eq, order_id)?,
                            Domain::condition("display_type", Operator::Eq, false)?,
                        ]),
                    )
                    .await?;
                if lines.is_empty() {
                    return Err(Error::user(
                        "Add at least one product line before applying a line discount.",
                    ));
                }
                let mut vals = RawVals::new();
                vals.insert("discount".into(), Cell::Decimal(pct));
                lines.write(tx, vals).await?;
                format!("Applied {pct}% discount to all order lines.")
            }
            "so_discount" => {
                let untaxed = match order_rec.cell(tx, "amount_untaxed").await? {
                    Cell::Decimal(d) => d,
                    _ => Decimal::ZERO,
                };
                let disc = compute_global_discount(untaxed, pct)?;
                let uom_id = default_unit_uom_id(tx).await?;
                let mut lvals = RawVals::new();
                lvals.insert("order_id".into(), Cell::Id(order_id));
                lvals.insert(
                    "name".into(),
                    Cell::Str(format!("Global Discount ({pct}%)").into()),
                );
                lvals.insert("product_uom_id".into(), Cell::Id(uom_id));
                lvals.insert("product_uom_qty".into(), Cell::Decimal(Decimal::ONE));
                lvals.insert("price_unit".into(), Cell::Decimal(-disc));
                env.records("sale.order.line")?
                    .create(tx, vec![lvals])
                    .await?;
                format!("Added global discount line (-{disc}).")
            }
            "amount" => {
                if fixed_amt <= Decimal::ZERO {
                    return Err(Error::user(
                        "Fixed discount amount must be greater than zero.",
                    ));
                }
                let uom_id = default_unit_uom_id(tx).await?;
                let mut lvals = RawVals::new();
                lvals.insert("order_id".into(), Cell::Id(order_id));
                lvals.insert("name".into(), Cell::Str("Fixed Discount".into()));
                lvals.insert("product_uom_id".into(), Cell::Id(uom_id));
                lvals.insert("product_uom_qty".into(), Cell::Decimal(Decimal::ONE));
                lvals.insert("price_unit".into(), Cell::Decimal(-fixed_amt));
                env.records("sale.order.line")?
                    .create(tx, vec![lvals])
                    .await?;
                format!("Added fixed discount line (-{fixed_amt}).")
            }
            other => {
                return Err(Error::validation(format!("Unknown discount type: {other}")));
            }
        };

        wizard.as_records().unlink(tx).await?;
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.client",
            "tag": "display_notification",
            "params": {
                "message": msg,
                "type": "success",
                "next": {"type": "ir.actions.act_window_close"},
            },
        }))))
    }
}

/// Resolves the default unit of measure (`uom.product_uom_unit`, else first active UoM)
/// required by non-section `sale.order.line` records.
pub(crate) async fn default_unit_uom_id(tx: &mut Transaction) -> Result<i64> {
    let rows = tx
        .query(
            "SELECT res_id::int8 FROM ir_model_data \
             WHERE module = 'uom' AND name = 'product_uom_unit' LIMIT 1",
            &[],
        )
        .await?;
    if let Some(id) = rows.first().and_then(|r| r.try_get::<_, i64>(0).ok()) {
        return Ok(id);
    }
    let fallback = tx
        .query("SELECT id::int8 FROM uom_uom ORDER BY id LIMIT 1", &[])
        .await?;
    fallback
        .first()
        .and_then(|r| r.try_get::<_, i64>(0).ok())
        .ok_or_else(|| Error::validation("No unit of measure is configured."))
}

#[methods(SaleOrder, addon = "sale")]
impl SaleOrder {
    /// Opens the Discount wizard dialog for this sales order.
    #[method]
    #[rpc(effect = read)]
    pub async fn action_open_discount_wizard(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Write).await?;
        let oid = rec.as_records().ids()[0];
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.act_window",
            "name": "Discount",
            "res_model": "sale.order.discount",
            "target": "new",
            "views": [[false, "form"]],
            "context": {"default_sale_order_id": oid},
        }))))
    }
}

#[cfg(test)]
mod tests {
    use super::compute_global_discount;
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn global_discount_uses_exact_decimals_and_rejects_invalid_percent() {
        let untaxed = Decimal::from_str("250.50").unwrap();
        let disc = compute_global_discount(untaxed, Decimal::from_str("15").unwrap()).unwrap();
        assert_eq!(disc, Decimal::from_str("37.58").unwrap());
        assert!(compute_global_discount(untaxed, Decimal::from_str("120").unwrap()).is_err());
        assert!(compute_global_discount(untaxed, Decimal::ZERO).is_err());
    }
}
