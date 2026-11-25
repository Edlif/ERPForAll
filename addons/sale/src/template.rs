// SPDX-License-Identifier: MIT OR Apache-2.0
//! Quotation Templates (`sale.order.template`, `sale.order.template.line`,
//! `sale.order.template.option`) and Optional Products on Sales Orders (`sale.order.option`).

use erp_sdk::Operation;
use erp_sdk::prelude::*;
use erp_sdk::serde_json::json;

use crate::discount::default_unit_uom_id;
use crate::order::SaleOrder;

/// Pure exact-decimal line pricing helper for optional products added to an order.
pub fn compute_option_subtotal(
    quantity: Decimal,
    price_unit: Decimal,
    discount_pct: Decimal,
) -> Decimal {
    let gross = quantity * price_unit;
    let net = gross * (Decimal::ONE - discount_pct / Decimal::from(100));
    net.round_dp(2)
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Quotation template query failed: {err}"))
}

/// `sale.order.template`: reusable quotation blueprint with standard lines, optional products,
/// validity duration, signature/payment policy, and terms.
#[model(
    name = "sale.order.template",
    description = "Quotation Template",
    order = "name, id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "ancestor", empty = "deny")
)]
pub struct SaleOrderTemplate {
    /// Template name.
    #[field(char, required = true, translate = true, string = "Quotation Template")]
    pub name: Option<Arc<str>>,
    /// Owning company.
    #[field(many2one, comodel = "res.company", required = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// Active flag.
    #[field(boolean, default = true, string = "Active")]
    pub active: bool,
    /// Quotation validity in days.
    #[field(integer, default = 30, string = "Quotation Expires After (Days)")]
    pub number_of_days: i32,
    /// Require online signature.
    #[field(boolean, default = true, string = "Online Signature")]
    pub require_signature: bool,
    /// Require online payment.
    #[field(boolean, default = false, string = "Online Payment")]
    pub require_payment: bool,
    /// Default terms and conditions copied to quotations.
    #[field(text, string = "Terms & Conditions")]
    pub note: Option<Arc<str>>,
    /// Template product/section/note lines.
    #[field(
        one2many,
        comodel = "sale.order.template.line",
        inverse_name = "template_id",
        string = "Lines"
    )]
    pub line_ids: Vec<RecId>,
    /// Template optional products.
    #[field(
        one2many,
        comodel = "sale.order.template.option",
        inverse_name = "template_id",
        string = "Optional Products"
    )]
    pub option_ids: Vec<RecId>,
}

#[methods(SaleOrderTemplate, addon = "sale")]
impl SaleOrderTemplate {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<SaleOrderTemplate>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<erp_sdk::ValueMap> {
        let mut values: erp_sdk::ValueMap =
            sup.call(tx, this.as_records(), (names.clone(),)).await?;
        if names.iter().any(|n| n == "company_id")
            && !values.contains_key("company_id")
            && let Some(cid) = this.env().company_id()?
        {
            values.insert("company_id".to_owned(), Value::Int(cid));
        }
        Ok(values)
    }
}

/// `sale.order.template.line`: product, section, or note row on a quotation template.
#[model(
    name = "sale.order.template.line",
    description = "Quotation Template Line",
    order = "sequence, id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "ancestor", empty = "deny")
)]
pub struct SaleOrderTemplateLine {
    /// Owning template.
    #[field(
        many2one,
        comodel = "sale.order.template",
        required = true,
        ondelete = "cascade",
        string = "Quotation Template"
    )]
    pub template_id: Option<RecId>,
    /// Owning company.
    #[field(
        many2one,
        comodel = "res.company",
        related = "template_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Display order.
    #[field(integer, default = 10, string = "Sequence")]
    pub sequence: i32,
    /// Optional section or note display type.
    #[field(
        selection,
        selection = [
            ("line_section", "Section"),
            ("line_note", "Note"),
        ],
        string = "Display Type"
    )]
    pub display_type: Option<Arc<str>>,
    /// Product to add (empty for section/note lines).
    #[field(many2one, comodel = "product.product", string = "Product")]
    pub product_id: Option<RecId>,
    /// Line description (defaults to product display name if left empty).
    #[field(char, string = "Description")]
    pub name: Option<Arc<str>>,
    /// Quantity.
    #[field(float, digits = "Product Unit", default = 1, string = "Quantity")]
    pub product_uom_qty: Decimal,
    /// Unit of measure.
    #[field(many2one, comodel = "uom.uom", string = "Unit of Measure")]
    pub product_uom_id: Option<RecId>,
}

#[methods(SaleOrderTemplateLine, addon = "sale")]
impl SaleOrderTemplateLine {
    #[method(override)]
    async fn create(
        this: &RecordSet<SaleOrderTemplateLine>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let env = this.env().clone();
        for vals in &mut vals_list {
            if let Some(Cell::Id(pid)) = vals.get("product_id").cloned() {
                let prod = env.records("product.product")?.browse([pid])?;
                let needs_name = match vals.get("name") {
                    None | Some(Cell::Null) => true,
                    Some(Cell::Str(s)) => s.trim().is_empty(),
                    _ => false,
                };
                if needs_name && let Ok(Cell::Str(pname)) = prod.cell(tx, "display_name").await {
                    vals.insert("name".into(), Cell::Str(pname));
                }
                if !vals.contains("product_uom_id")
                    && let Ok(Cell::Id(uid)) = prod.cell(tx, "uom_id").await
                {
                    vals.insert("product_uom_id".into(), Cell::Id(uid));
                }
            }
            let empty_name = match vals.get("name") {
                None | Some(Cell::Null) => true,
                Some(Cell::Str(s)) => s.trim().is_empty(),
                _ => false,
            };
            if empty_name {
                return Err(Error::validation(
                    "Each quotation template line requires a product or a description.",
                ));
            }
        }
        sup.call(tx, this.as_records(), (vals_list,)).await
    }
}

/// `sale.order.template.option`: optional upsell product suggested by a quotation template.
#[model(
    name = "sale.order.template.option",
    description = "Quotation Template Option",
    order = "id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "ancestor", empty = "deny")
)]
pub struct SaleOrderTemplateOption {
    /// Owning template.
    #[field(
        many2one,
        comodel = "sale.order.template",
        required = true,
        ondelete = "cascade",
        string = "Quotation Template"
    )]
    pub template_id: Option<RecId>,
    /// Owning company.
    #[field(
        many2one,
        comodel = "res.company",
        related = "template_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Suggested product.
    #[field(
        many2one,
        comodel = "product.product",
        required = true,
        string = "Product"
    )]
    pub product_id: Option<RecId>,
    /// Optional description (defaults to product display name).
    #[field(char, string = "Description")]
    pub name: Option<Arc<str>>,
    /// Quantity.
    #[field(float, digits = "Product Unit", default = 1, string = "Quantity")]
    pub quantity: Decimal,
    /// Unit of measure.
    #[field(many2one, comodel = "uom.uom", string = "Unit of Measure")]
    pub uom_id: Option<RecId>,
}

#[methods(SaleOrderTemplateOption, addon = "sale")]
impl SaleOrderTemplateOption {
    #[method(override)]
    async fn create(
        this: &RecordSet<SaleOrderTemplateOption>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let env = this.env().clone();
        for vals in &mut vals_list {
            if let Some(Cell::Id(pid)) = vals.get("product_id").cloned() {
                let prod = env.records("product.product")?.browse([pid])?;
                let needs_name = match vals.get("name") {
                    None | Some(Cell::Null) => true,
                    Some(Cell::Str(s)) => s.trim().is_empty(),
                    _ => false,
                };
                if needs_name && let Ok(Cell::Str(pname)) = prod.cell(tx, "display_name").await {
                    vals.insert("name".into(), Cell::Str(pname));
                }
                if !vals.contains("uom_id")
                    && let Ok(Cell::Id(uid)) = prod.cell(tx, "uom_id").await
                {
                    vals.insert("uom_id".into(), Cell::Id(uid));
                }
            }
        }
        sup.call(tx, this.as_records(), (vals_list,)).await
    }
}

/// `sale.order.option`: optional product attached to a quotation that can be promoted to an
/// order line with one click.
#[model(
    name = "sale.order.option",
    description = "Sale Option",
    order = "id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct SaleOrderOption {
    /// Owning sales order.
    #[field(
        many2one,
        comodel = "sale.order",
        required = true,
        ondelete = "cascade",
        string = "Sales Order"
    )]
    pub order_id: Option<RecId>,
    /// Owning company.
    #[field(
        many2one,
        comodel = "res.company",
        related = "order_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Order currency.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "order_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Optional product.
    #[field(
        many2one,
        comodel = "product.product",
        required = true,
        string = "Product"
    )]
    pub product_id: Option<RecId>,
    /// Description.
    #[field(char, string = "Description")]
    pub name: Option<Arc<str>>,
    /// Quantity.
    #[field(float, digits = "Product Unit", default = 1, string = "Quantity")]
    pub quantity: Decimal,
    /// Unit of measure.
    #[field(many2one, comodel = "uom.uom", string = "Unit of Measure")]
    pub uom_id: Option<RecId>,
    /// Unit price.
    #[field(monetary, currency_field = "currency_id", string = "Unit Price")]
    pub price_unit: Decimal,
    /// Discount percentage.
    #[field(float, digits = "Discount", default = 0, string = "Discount (%)")]
    pub discount: Decimal,
    /// True once added to the main order lines.
    #[field(
        boolean,
        readonly = true,
        default = false,
        copy = false,
        string = "Added to Order"
    )]
    pub is_present: bool,
}

#[methods(SaleOrderOption, addon = "sale")]
impl SaleOrderOption {
    #[method(override)]
    async fn create(
        this: &RecordSet<SaleOrderOption>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let env = this.env().clone();
        for vals in &mut vals_list {
            if let Some(Cell::Id(pid)) = vals.get("product_id").cloned() {
                let prod = env.records("product.product")?.browse([pid])?;
                let needs_name = match vals.get("name") {
                    None | Some(Cell::Null) => true,
                    Some(Cell::Str(s)) => s.trim().is_empty(),
                    _ => false,
                };
                if needs_name && let Ok(Cell::Str(pname)) = prod.cell(tx, "display_name").await {
                    vals.insert("name".into(), Cell::Str(pname));
                }
                if !vals.contains("uom_id")
                    && let Ok(Cell::Id(uid)) = prod.cell(tx, "uom_id").await
                {
                    vals.insert("uom_id".into(), Cell::Id(uid));
                }
                let price_zero = match vals.get("price_unit") {
                    None | Some(Cell::Null) => true,
                    Some(Cell::Decimal(d)) => d.is_zero(),
                    _ => false,
                };
                if price_zero && let Ok(Cell::Decimal(lp)) = prod.cell(tx, "lst_price").await {
                    vals.insert("price_unit".into(), Cell::Decimal(lp));
                }
            }
        }
        sup.call(tx, this.as_records(), (vals_list,)).await
    }

    /// Adds this optional product as a real line on its quotation.
    #[method]
    #[rpc(effect = write)]
    pub async fn button_add_to_order(
        this: &RecordSet<SaleOrderOption>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let opt = this.ensure_one()?;
        opt.as_records().check_access(tx, Operation::Write).await?;
        let opt_id = opt.as_records().ids()[0];
        let order_id = opt
            .get(tx, SaleOrderOption::ORDER_ID)
            .await?
            .ok_or_else(|| Error::validation("Option has no sales order."))?
            .get();
        let product_id = opt
            .get(tx, SaleOrderOption::PRODUCT_ID)
            .await?
            .ok_or_else(|| Error::validation("Option has no product."))?
            .get();
        let name = opt.get(tx, SaleOrderOption::NAME).await?;
        let qty = opt.get(tx, SaleOrderOption::QUANTITY).await?;
        let uom_id = match opt.get(tx, SaleOrderOption::UOM_ID).await? {
            Some(u) => u.get(),
            None => default_unit_uom_id(tx).await?,
        };
        let price = opt.get(tx, SaleOrderOption::PRICE_UNIT).await?;
        let discount = opt.get(tx, SaleOrderOption::DISCOUNT).await?;

        let env = opt.env().with_context_replaced(erp_sdk::Context::default());
        let mut lvals = RawVals::new();
        lvals.insert("order_id".into(), Cell::Id(order_id));
        lvals.insert("product_id".into(), Cell::Id(product_id));
        if let Some(n) = name {
            lvals.insert("name".into(), Cell::Str(n));
        }
        lvals.insert("product_uom_qty".into(), Cell::Decimal(qty));
        lvals.insert("product_uom_id".into(), Cell::Id(uom_id));
        lvals.insert("price_unit".into(), Cell::Decimal(price));
        lvals.insert("discount".into(), Cell::Decimal(discount));
        env.records("sale.order.line")?
            .create(tx, vec![lvals])
            .await?;

        tx.execute(
            "UPDATE sale_order_option SET is_present = true, \
             write_date = now() AT TIME ZONE 'UTC' WHERE id = $1",
            &[&opt_id],
        )
        .await?;
        tx.invalidate_records(opt.as_records(), Some(&["is_present"]))
            .await?;
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.client",
            "tag": "display_notification",
            "params": {
                "message": "Optional product added to order lines.",
                "type": "success",
            },
        }))))
    }
}

#[methods(SaleOrder, addon = "sale")]
impl SaleOrder {
    /// Populates order lines, optional products, validity date, and signature/payment flags
    /// from the chosen `sale_order_template_id`.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_apply_template(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let order = this.ensure_one()?;
        order
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let oid = order.as_records().ids()[0];
        let state = order.get(tx, SaleOrder::STATE).await?;
        if !matches!(state.as_deref(), Some("draft" | "sent")) {
            return Err(Error::user(
                "Quotation templates can only be applied to draft or sent quotations.",
            ));
        }
        let tpl_id = order
            .get(tx, SaleOrder::SALE_ORDER_TEMPLATE_ID)
            .await?
            .ok_or_else(|| Error::user("Select a Quotation Template first."))?
            .get();

        let env = order
            .env()
            .with_context_replaced(erp_sdk::Context::default());
        let trows = tx
            .query(
                "SELECT COALESCE(number_of_days, 30), COALESCE(require_signature, true), \
                        COALESCE(require_payment, false), note::text \
                 FROM sale_order_template WHERE id = $1",
                &[&tpl_id],
            )
            .await?;
        let Some(trow) = trows.first() else {
            return Err(Error::missing("Quotation template not found."));
        };
        let days: i32 = trow.try_get(0).map_err(db)?;
        let req_sig: bool = trow.try_get(1).map_err(db)?;
        let req_pay: bool = trow.try_get(2).map_err(db)?;
        let note: Option<String> = trow.try_get(3).map_err(db)?;

        let mut uvals = RawVals::new();
        if days > 0 {
            let today = Date::today();
            uvals.insert(
                "validity_date".into(),
                Cell::Date(Date::from_days(today.days() + days)),
            );
        }
        uvals.insert("require_signature".into(), Cell::Bool(req_sig));
        uvals.insert("require_payment".into(), Cell::Bool(req_pay));
        if let Some(n) = note
            && !n.trim().is_empty()
        {
            uvals.insert("note".into(), Cell::Str(n.into()));
        }
        env.records("sale.order")?
            .browse([oid])?
            .write(tx, uvals)
            .await?;

        let fallback_uom = default_unit_uom_id(tx).await?;
        let lrows = tx
            .query(
                "SELECT sequence, display_type::text, product_id::int8, name::text, \
                        COALESCE(product_uom_qty, 1), product_uom_id::int8 \
                 FROM sale_order_template_line WHERE template_id = $1 ORDER BY sequence, id",
                &[&tpl_id],
            )
            .await?;
        let mut new_lines = Vec::with_capacity(lrows.len());
        for lrow in lrows {
            let seq: i32 = lrow.try_get(0).map_err(db)?;
            let dtype: Option<String> = lrow.try_get(1).map_err(db)?;
            let pid: Option<i64> = lrow.try_get(2).map_err(db)?;
            let lname: String = lrow.try_get(3).map_err(db)?;
            let qty: Decimal = lrow.try_get(4).map_err(db)?;
            let uom: Option<i64> = lrow.try_get(5).map_err(db)?;

            let mut v = RawVals::new();
            v.insert("order_id".into(), Cell::Id(oid));
            v.insert("sequence".into(), Cell::Int(seq));
            v.insert("name".into(), Cell::Str(lname.into()));
            if let Some(dt) = dtype {
                v.insert("display_type".into(), Cell::Str(dt.into()));
            } else {
                if let Some(p) = pid {
                    v.insert("product_id".into(), Cell::Id(p));
                }
                v.insert("product_uom_qty".into(), Cell::Decimal(qty));
                let chosen_uom = match uom {
                    Some(u) => u,
                    None => {
                        if let Some(p) = pid {
                            match env
                                .records("product.product")?
                                .browse([p])?
                                .cell(tx, "uom_id")
                                .await
                            {
                                Ok(Cell::Id(u)) => u,
                                _ => fallback_uom,
                            }
                        } else {
                            fallback_uom
                        }
                    }
                };
                v.insert("product_uom_id".into(), Cell::Id(chosen_uom));
            }
            new_lines.push(v);
        }
        let added_lines = new_lines.len();
        if !new_lines.is_empty() {
            env.records("sale.order.line")?
                .create(tx, new_lines)
                .await?;
        }

        let orows = tx
            .query(
                "SELECT product_id::int8, name::text, COALESCE(quantity, 1), uom_id::int8 \
                 FROM sale_order_template_option WHERE template_id = $1 ORDER BY id",
                &[&tpl_id],
            )
            .await?;
        let mut new_opts = Vec::with_capacity(orows.len());
        for orow in orows {
            let pid: i64 = orow.try_get(0).map_err(db)?;
            let oname: Option<String> = orow.try_get(1).map_err(db)?;
            let qty: Decimal = orow.try_get(2).map_err(db)?;
            let uom: Option<i64> = orow.try_get(3).map_err(db)?;
            let mut v = RawVals::new();
            v.insert("order_id".into(), Cell::Id(oid));
            v.insert("product_id".into(), Cell::Id(pid));
            if let Some(n) = oname {
                v.insert("name".into(), Cell::Str(n.into()));
            }
            v.insert("quantity".into(), Cell::Decimal(qty));
            if let Some(u) = uom {
                v.insert("uom_id".into(), Cell::Id(u));
            }
            new_opts.push(v);
        }
        let added_opts = new_opts.len();
        if !new_opts.is_empty() {
            env.records("sale.order.option")?
                .create(tx, new_opts)
                .await?;
        }

        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.client",
            "tag": "display_notification",
            "params": {
                "message": format!("Applied template: {added_lines} line(s) and {added_opts} optional product(s) added."),
                "type": "success",
            },
        }))))
    }

    /// Promotes every non-added optional product on this quotation into order lines.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_add_optional_products(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let order = this.ensure_one()?;
        order
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let oid = order.as_records().ids()[0];
        let rows = tx
            .query(
                "SELECT id::int8 FROM sale_order_option \
                 WHERE order_id = $1 AND NOT COALESCE(is_present, false) ORDER BY id",
                &[&oid],
            )
            .await?;
        if rows.is_empty() {
            return Err(Error::user(
                "There are no pending optional products left to add.",
            ));
        }
        let ids: Vec<i64> = rows
            .iter()
            .map(|r| r.try_get::<_, i64>(0).map_err(db))
            .collect::<Result<_>>()?;
        let opts = order.env().records("sale.order.option")?.browse(ids)?;
        let count = opts.len();
        for opt in opts.iter() {
            let _: Value = opt.call(tx, "button_add_to_order", ()).await?;
        }
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.client",
            "tag": "display_notification",
            "params": {
                "message": format!("Added {count} optional product(s) to order lines."),
                "type": "success",
            },
        }))))
    }
}

#[cfg(test)]
mod tests {
    use super::compute_option_subtotal;
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn optional_product_subtotal_is_exact_decimal() {
        // 3 * 49.95 * 0.90 = 134.865 -> rounds half-to-even to 134.86
        let sub = compute_option_subtotal(
            Decimal::from(3),
            Decimal::from_str("49.95").unwrap(),
            Decimal::from(10),
        );
        assert_eq!(sub, Decimal::from_str("134.86").unwrap());
        let sub2 = compute_option_subtotal(
            Decimal::from(2),
            Decimal::from_str("50.00").unwrap(),
            Decimal::from(15),
        );
        assert_eq!(sub2, Decimal::from_str("85.00").unwrap());
    }
}
