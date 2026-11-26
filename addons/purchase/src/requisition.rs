// SPDX-License-Identifier: MIT OR Apache-2.0
//! Purchase Agreements (`purchase.requisition` & `purchase.requisition.line`): Blanket Orders and
//! Calls for Tenders / Purchase Templates with remaining quantity tracking and 1-click RFQ creation.

use std::collections::BTreeMap;

use erp_sdk::prelude::*;
use erp_sdk::sequence::draw_by_code;
use erp_sdk::serde_json::json;
use erp_sdk::{CompanyId, Operation, ValueMap};

/// Sequence code for Purchase Agreements (`data/purchase_data.xml`).
pub const REQUISITION_SEQUENCE_CODE: &str = "purchase.requisition";

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Purchase agreement query failed: {err}"))
}

/// Pure exact-decimal remaining quantity for a blanket order line (never below zero).
pub fn compute_agreement_remaining_qty(agreed_qty: Decimal, ordered_qty: Decimal) -> Decimal {
    let diff = agreed_qty - ordered_qty;
    if diff > Decimal::ZERO {
        diff
    } else {
        Decimal::ZERO
    }
}

/// `purchase.requisition`: Blanket Order or Call for Tenders agreement with a vendor or multiple
/// vendors.
#[model(
    name = "purchase.requisition",
    description = "Purchase Agreement",
    order = "id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct PurchaseRequisition {
    /// Agreement reference number drawn from sequence.
    #[field(
        char,
        required = true,
        copy = false,
        default = "New",
        string = "Agreement"
    )]
    pub name: Option<Arc<str>>,
    /// Blanket order vs Call for Tenders / Purchase Template.
    #[field(
        selection,
        selection = [
            ("blanket_order", "Blanket Order"),
            ("purchase_template", "Call for Tenders"),
        ],
        required = true,
        default = "blanket_order",
        string = "Agreement Type"
    )]
    pub requisition_type: Option<Arc<str>>,
    /// Vendor (optional for Calls for Tenders, typical for Blanket Orders).
    #[field(
        many2one,
        comodel = "res.partner",
        check_company = true,
        string = "Vendor"
    )]
    pub vendor_id: Option<RecId>,
    /// Responsible buyer.
    #[field(
        many2one,
        comodel = "res.users",
        check_company = true,
        string = "Purchase Representative"
    )]
    pub user_id: Option<RecId>,
    /// Owning company.
    #[field(many2one, comodel = "res.company", required = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// Company currency.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// External vendor contract reference.
    #[field(char, string = "Reference")]
    pub reference: Option<Arc<str>>,
    /// Agreement start date.
    #[field(date, string = "Start Date")]
    pub date_start: Option<Date>,
    /// Agreement expiration / deadline date.
    #[field(date, string = "Agreement Deadline")]
    pub date_end: Option<Date>,
    /// Lifecycle state.
    #[field(
        selection,
        selection = [
            ("draft", "Draft"),
            ("confirmed", "Confirmed"),
            ("done", "Closed"),
            ("cancel", "Cancelled"),
        ],
        required = true,
        default = "draft",
        copy = false,
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
    /// Terms and notes.
    #[field(text, string = "Terms and Conditions")]
    pub description: Option<Arc<str>>,
    /// Agreement product lines.
    #[field(
        one2many,
        comodel = "purchase.requisition.line",
        inverse_name = "requisition_id",
        copy = true,
        string = "Products"
    )]
    pub line_ids: Vec<RecId>,
    /// Number of RFQs / Purchase Orders linked to this agreement.
    #[field(integer, compute = "_compute_order_count", string = "RFQs / Orders")]
    pub order_count: i32,
}

#[methods(PurchaseRequisition, addon = "purchase")]
impl PurchaseRequisition {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<ValueMap> {
        let mut values: ValueMap = sup.call(tx, this.as_records(), (names.clone(),)).await?;
        let env = this.env().clone();
        if names.iter().any(|n| n == "user_id") && !values.contains_key("user_id") {
            values.insert("user_id".into(), Value::Int(env.uid()));
        }
        if names.iter().any(|n| n == "company_id")
            && !values.contains_key("company_id")
            && let Some(cid) = env.company_id()?
        {
            values.insert("company_id".into(), Value::Int(cid));
        }
        if names.iter().any(|n| n == "date_start") && !values.contains_key("date_start") {
            values.insert("date_start".into(), Value::Date(Date::today()));
        }
        Ok(values)
    }

    #[method(override)]
    async fn create(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let env = this.env().clone();
        this.as_records()
            .check_access(tx, Operation::Create)
            .await?;
        let default_company = env.company_id()?;
        for vals in &mut vals_list {
            if !vals.contains("company_id")
                && let Some(cid) = default_company
            {
                vals.insert("company_id".into(), Cell::Id(cid));
            }
            let unnamed = match vals.get("name") {
                None | Some(Cell::Null) => true,
                Some(Cell::Str(s)) => s.trim().is_empty() || s.as_ref() == "New",
                _ => false,
            };
            if unnamed {
                let cid = match vals.get("company_id") {
                    Some(Cell::Id(id)) => *id,
                    _ => default_company.ok_or_else(|| {
                        Error::validation("A purchase agreement needs a company.")
                    })?,
                };
                let company = CompanyId::new(cid)?;
                let seq = draw_by_code(
                    tx,
                    &env.sudo(true),
                    REQUISITION_SEQUENCE_CODE,
                    company,
                    Some(Date::today()),
                )
                .await?;
                vals.insert("name".into(), Cell::Str(seq.into()));
            }
        }
        sup.call(tx, this.as_records(), (vals_list,)).await
    }

    #[compute(depends = ["name"])]
    async fn _compute_order_count(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
        out: &mut Assign<'_, PurchaseRequisition>,
    ) -> Result<()> {
        let rec_ids = this.as_records().rec_ids();
        let real_ids: Vec<i64> = rec_ids
            .iter()
            .filter_map(|r| r.as_real().or(r.origin()).map(|id| id.get()))
            .collect();
        let rows = if real_ids.is_empty() {
            Vec::new()
        } else {
            tx.query(
                "SELECT requisition_id::int8, COUNT(*)::int4 \
                 FROM purchase_order WHERE requisition_id = ANY($1) GROUP BY requisition_id",
                &[&real_ids],
            )
            .await?
        };
        let mut counts: BTreeMap<i64, i32> = BTreeMap::new();
        for row in rows {
            counts.insert(row.try_get(0).map_err(db)?, row.try_get(1).map_err(db)?);
        }
        for (i, rec_id) in rec_ids.iter().enumerate() {
            let count = rec_id
                .as_real()
                .or(rec_id.origin())
                .and_then(|id| counts.get(&id.get()).copied())
                .unwrap_or(0);
            out.set(i, PurchaseRequisition::ORDER_COUNT, count)?;
        }
        Ok(())
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_confirm(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Write).await?;
        let rid = rec.as_records().ids()[0];
        let lines = tx
            .query(
                "SELECT 1 FROM purchase_requisition_line WHERE requisition_id = $1 LIMIT 1",
                &[&rid],
            )
            .await?;
        if lines.is_empty() {
            return Err(Error::user(
                "Add at least one product line before confirming a purchase agreement.",
            ));
        }
        rec.as_records()
            .write(
                tx,
                RawVals::new().set_cell("state", Cell::Str("confirmed".into())),
            )
            .await
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_done(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records().check_access(tx, Operation::Write).await?;
        this.as_records()
            .write(
                tx,
                RawVals::new().set_cell("state", Cell::Str("done".into())),
            )
            .await
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_cancel(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records().check_access(tx, Operation::Write).await?;
        this.as_records()
            .write(
                tx,
                RawVals::new().set_cell("state", Cell::Str("cancel".into())),
            )
            .await
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_draft(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records().check_access(tx, Operation::Write).await?;
        this.as_records()
            .write(
                tx,
                RawVals::new().set_cell("state", Cell::Str("draft".into())),
            )
            .await
    }

    /// Opens the RFQs / Purchase Orders created from this agreement.
    #[method]
    #[rpc(effect = read)]
    pub async fn action_view_orders(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Read).await?;
        let rid = rec.as_records().ids()[0];
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.act_window",
            "name": "RFQs / Orders",
            "res_model": "purchase.order",
            // The web client takes a domain only from a result that opens in the page, with list then form.
            "target": "current",
            "views": [[false, "list"], [false, "form"]],
            "domain": [["requisition_id", "=", rid]],
            "context": {"default_requisition_id": rid},
        }))))
    }

    /// Creates a new Request for Quotation populated from this confirmed agreement's lines
    /// (remaining blanket quantities and agreed unit prices) and opens it.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_create_rfq(
        this: &RecordSet<PurchaseRequisition>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Read).await?;
        let rid = rec.as_records().ids()[0];
        let state = rec.get(tx, PurchaseRequisition::STATE).await?;
        if state.as_deref() != Some("confirmed") {
            return Err(Error::user(
                "Confirm the purchase agreement before creating a Request for Quotation.",
            ));
        }
        let vendor_id = rec
            .get(tx, PurchaseRequisition::VENDOR_ID)
            .await?
            .ok_or_else(|| {
                Error::user("Select a Vendor on the purchase agreement before creating an RFQ.")
            })?
            .get();
        let company_id = rec
            .get(tx, PurchaseRequisition::COMPANY_ID)
            .await?
            .ok_or_else(|| Error::invariant("Agreement has no company."))?
            .get();
        let currency_id = rec.get(tx, PurchaseRequisition::CURRENCY_ID).await?;
        let ag_name = rec
            .get(tx, PurchaseRequisition::NAME)
            .await?
            .unwrap_or_else(|| Arc::from("Agreement"));

        let env = rec
            .env()
            .with_context_replaced(erp_sdk::Context::new().with_company_id(company_id));
        let mut po_vals = RawVals::new();
        po_vals.insert("partner_id".into(), Cell::Id(vendor_id));
        po_vals.insert("company_id".into(), Cell::Id(company_id));
        if let Some(cur) = currency_id {
            po_vals.insert("currency_id".into(), Cell::Id(cur.get()));
        }
        po_vals.insert("origin".into(), Cell::Str(ag_name));
        po_vals.insert("requisition_id".into(), Cell::Id(rid));
        let po = env
            .records("purchase.order")?
            .create(tx, vec![po_vals])
            .await?;
        let po_id = po.ensure_one()?.int()?;

        let line_rows = tx
            .query(
                "SELECT id::int8, product_id::int8, product_qty, product_uom_id::int8, price_unit \
                 FROM purchase_requisition_line WHERE requisition_id = $1 ORDER BY id",
                &[&rid],
            )
            .await?;

        // Read already ordered quantities for this agreement across non-cancelled POs.
        let ord_rows = tx
            .query(
                "SELECT l.product_id::int8, COALESCE(SUM(l.product_qty), 0) \
                 FROM purchase_order_line l \
                 JOIN purchase_order o ON o.id = l.order_id \
                 WHERE o.requisition_id = $1 AND o.id != $2 AND o.state != 'cancel' \
                   AND l.product_id IS NOT NULL \
                 GROUP BY l.product_id",
                &[&rid, &po_id],
            )
            .await?;
        let mut ordered_by_prod: BTreeMap<i64, Decimal> = BTreeMap::new();
        for r in ord_rows {
            ordered_by_prod.insert(r.try_get(0).map_err(db)?, r.try_get(1).map_err(db)?);
        }

        let mut po_lines = Vec::new();
        for row in line_rows {
            let pid: i64 = row.try_get(1).map_err(db)?;
            let agreed: Decimal = row.try_get(2).map_err(db)?;
            let uom: Option<i64> = row.try_get(3).map_err(db)?;
            let price: Decimal = row.try_get(4).map_err(db)?;
            let already = ordered_by_prod.get(&pid).copied().unwrap_or(Decimal::ZERO);
            let rem = compute_agreement_remaining_qty(agreed, already);
            let qty = if rem > Decimal::ZERO { rem } else { agreed };
            let mut lv = RawVals::new();
            lv.insert("order_id".into(), Cell::Id(po_id));
            lv.insert("product_id".into(), Cell::Id(pid));
            lv.insert("product_qty".into(), Cell::Decimal(qty));
            if let Some(u) = uom {
                lv.insert("uom_id".into(), Cell::Id(u));
            }
            if price > Decimal::ZERO {
                lv.insert("price_unit".into(), Cell::Decimal(price));
            }
            po_lines.push(lv);
        }
        if !po_lines.is_empty() {
            env.records("purchase.order.line")?
                .create(tx, po_lines)
                .await?;
        }

        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.act_window",
            "name": "Request for Quotation",
            "res_model": "purchase.order",
            "res_id": po_id,
            "views": [[false, "form"]],
            "target": "current",
        }))))
    }
}

/// `purchase.requisition.line`: product, agreed quantity, agreed price, ordered & remaining qty.
#[model(
    name = "purchase.requisition.line",
    description = "Purchase Agreement Line",
    order = "id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct PurchaseRequisitionLine {
    /// Owning agreement.
    #[field(
        many2one,
        comodel = "purchase.requisition",
        required = true,
        ondelete = "cascade",
        string = "Agreement"
    )]
    pub requisition_id: Option<RecId>,
    /// Company from the agreement.
    #[field(
        many2one,
        comodel = "res.company",
        related = "requisition_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Currency from the agreement.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "requisition_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Product covered by the agreement.
    #[field(
        many2one,
        comodel = "product.product",
        required = true,
        string = "Product"
    )]
    pub product_id: Option<RecId>,
    /// Agreed quantity.
    #[field(
        float,
        digits = "Product Unit",
        default = 1,
        required = true,
        string = "Quantity"
    )]
    pub product_qty: Decimal,
    /// Unit of measure.
    #[field(many2one, comodel = "uom.uom", string = "Unit")]
    pub product_uom_id: Option<RecId>,
    /// Agreed unit price.
    #[field(float, digits = "Product Price", default = 0, string = "Unit Price")]
    pub price_unit: Decimal,
    /// Quantity already ordered across non-cancelled RFQs / POs linked to this agreement.
    #[field(
        float,
        digits = "Product Unit",
        compute = "_compute_ordered_qty",
        string = "Ordered Qty"
    )]
    pub qty_ordered: Decimal,
    /// Quantity remaining under this agreement.
    #[field(
        float,
        digits = "Product Unit",
        compute = "_compute_ordered_qty",
        string = "Remaining Qty"
    )]
    pub qty_remaining: Decimal,
}

#[methods(PurchaseRequisitionLine, addon = "purchase")]
impl PurchaseRequisitionLine {
    #[method(override)]
    async fn create(
        this: &RecordSet<PurchaseRequisitionLine>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let env = this.env().clone();
        for vals in &mut vals_list {
            if let Some(Cell::Id(pid)) = vals.get("product_id").cloned()
                && !vals.contains("product_uom_id")
                && let Ok(Cell::Id(uid)) = env
                    .records("product.product")?
                    .browse([pid])?
                    .cell(tx, "uom_id")
                    .await
            {
                vals.insert("product_uom_id".into(), Cell::Id(uid));
            }
        }
        sup.call(tx, this.as_records(), (vals_list,)).await
    }

    #[compute(depends = ["requisition_id", "product_id", "product_qty"])]
    async fn _compute_ordered_qty(
        this: &RecordSet<PurchaseRequisitionLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, PurchaseRequisitionLine>,
    ) -> Result<()> {
        let reqs = this
            .mapped(tx, PurchaseRequisitionLine::REQUISITION_ID)
            .await?;
        let prods = this.mapped(tx, PurchaseRequisitionLine::PRODUCT_ID).await?;
        let qtys = this
            .mapped(tx, PurchaseRequisitionLine::PRODUCT_QTY)
            .await?;
        let req_ids: Vec<i64> = reqs.iter().flatten().map(|r| r.get()).collect();
        let mut by_key: BTreeMap<(i64, i64), Decimal> = BTreeMap::new();
        if !req_ids.is_empty() {
            let rows = tx
                .query(
                    "SELECT o.requisition_id::int8, l.product_id::int8, COALESCE(SUM(l.product_qty), 0) \
                     FROM purchase_order_line l \
                     JOIN purchase_order o ON o.id = l.order_id \
                     WHERE o.requisition_id = ANY($1) AND o.state != 'cancel' \
                       AND l.product_id IS NOT NULL \
                     GROUP BY o.requisition_id, l.product_id",
                    &[&req_ids],
                )
                .await?;
            for row in rows {
                let rid: i64 = row.try_get(0).map_err(db)?;
                let pid: i64 = row.try_get(1).map_err(db)?;
                let sum: Decimal = row.try_get(2).map_err(db)?;
                by_key.insert((rid, pid), sum);
            }
        }
        for (i, ((req, prod), agreed)) in reqs.iter().zip(&prods).zip(qtys).enumerate() {
            let ordered = match (req, prod) {
                (Some(r), Some(p)) => by_key
                    .get(&(r.get(), p.get()))
                    .copied()
                    .unwrap_or(Decimal::ZERO),
                _ => Decimal::ZERO,
            };
            let remaining = compute_agreement_remaining_qty(agreed, ordered);
            out.set(i, PurchaseRequisitionLine::QTY_ORDERED, ordered)?;
            out.set(i, PurchaseRequisitionLine::QTY_REMAINING, remaining)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::compute_agreement_remaining_qty;
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn agreement_remaining_quantity_is_exact_and_non_negative() {
        assert_eq!(
            compute_agreement_remaining_qty(
                Decimal::from_str("100.50").unwrap(),
                Decimal::from_str("35.25").unwrap(),
            ),
            Decimal::from_str("65.25").unwrap()
        );
        assert_eq!(
            compute_agreement_remaining_qty(Decimal::from(10), Decimal::from(15)),
            Decimal::ZERO
        );
    }
}
