// SPDX-License-Identifier: MIT OR Apache-2.0
//! Non-blocking purchase warning messages on vendors, products, purchase orders, and order lines.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_base::ResPartner;
use erp_addon_product::ProductTemplate;
use erp_sdk::prelude::*;

use crate::WARNING_GROUP;
use crate::line::PurchaseOrderLine;
use crate::order::PurchaseOrder;

/// Maximum length (in Unicode scalar values) of a purchase warning message.
pub const MAX_WARNING_CHARS: usize = 4_000;

fn check_warning_length(field_label: &str, values: Vec<Value>) -> Result<()> {
    for value in values {
        match value {
            Value::False => {}
            Value::Str(text) => {
                if text.chars().count() > MAX_WARNING_CHARS {
                    return Err(Error::validation(format!(
                        "{field_label} cannot exceed {MAX_WARNING_CHARS} characters."
                    )));
                }
            }
            other => {
                return Err(Error::programming(format!(
                    "{field_label} expected text or empty value, received {other:?}."
                )));
            }
        }
    }
    Ok(())
}

fn non_empty_text(context: &str, value: Value) -> Result<Option<Arc<str>>> {
    match value {
        Value::False => Ok(None),
        Value::Str(text) => Ok((!text.is_empty()).then_some(text)),
        other => Err(Error::programming(format!(
            "{context} expected text or empty value, received {other:?}."
        ))),
    }
}

fn optional_rec_id(context: &str, value: Value) -> Result<Option<RecId>> {
    match value {
        Value::False => Ok(None),
        Value::Int(id) => RecId::from_real(id)
            .ok_or_else(|| Error::programming(format!("{context} had invalid record id {id}.")))
            .map(Some),
        other => Err(Error::programming(format!(
            "{context} expected a relation id or empty value, received {other:?}."
        ))),
    }
}

#[derive(Clone)]
struct PartnerWarningInfo {
    label: Arc<str>,
    parent_id: Option<RecId>,
    warn_msg: Option<Arc<str>>,
}

async fn read_partner_info(
    env: &Env,
    tx: &mut Transaction,
    ids: Vec<RecId>,
) -> Result<BTreeMap<RecId, PartnerWarningInfo>> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let partners = env.records("res.partner")?.browse_ids(ids);
    let rec_ids = partners.rec_ids().to_vec();
    let names = partners.values(tx, "name").await?;
    let display_names = partners.values(tx, "display_name").await?;
    let parents = partners.values(tx, "parent_id").await?;
    let warnings = partners.values(tx, "purchase_warn_msg").await?;
    let mut map = BTreeMap::new();
    for ((((&id, name), display_name), parent), warn) in rec_ids
        .iter()
        .zip(names)
        .zip(display_names)
        .zip(parents)
        .zip(warnings)
    {
        let label = non_empty_text("res.partner.name", name)?
            .or(non_empty_text("res.partner.display_name", display_name)?)
            .unwrap_or_else(|| Arc::from(""));
        let parent_id = optional_rec_id("res.partner.parent_id", parent)?;
        let warn_msg = non_empty_text("res.partner.purchase_warn_msg", warn)?;
        map.insert(
            id,
            PartnerWarningInfo {
                label,
                parent_id,
                warn_msg,
            },
        );
    }
    Ok(map)
}

#[extend(ResPartner, addon = "purchase")]
pub(crate) struct PurchasePartnerWarnings {
    /// Non-blocking warning shown on purchase orders for this vendor.
    #[field(
        text,
        groups = "purchase.group_warning_purchase",
        string = "Purchase Order Warning"
    )]
    pub purchase_warn_msg: Option<Arc<str>>,
}

#[methods(ResPartner, addon = "purchase")]
impl PurchasePartnerWarnings {
    #[constrains("purchase_warn_msg")]
    async fn _check_purchase_warn_msg(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let values = this.as_records().values(tx, "purchase_warn_msg").await?;
        check_warning_length("Purchase order warning", values)
    }
}

#[extend(ProductTemplate, addon = "purchase")]
pub(crate) struct PurchaseProductWarnings {
    /// Non-blocking warning shown on purchase order lines for this product.
    #[field(
        text,
        groups = "purchase.group_warning_purchase",
        string = "Purchase Order Line Warning"
    )]
    pub purchase_line_warn_msg: Option<Arc<str>>,
}

#[methods(ProductTemplate, addon = "purchase")]
impl PurchaseProductWarnings {
    #[constrains("purchase_line_warn_msg")]
    async fn _check_purchase_line_warn_msg(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let values = this
            .as_records()
            .values(tx, "purchase_line_warn_msg")
            .await?;
        check_warning_length("Purchase order line warning", values)
    }
}

#[methods(PurchaseOrderLine, addon = "purchase")]
impl PurchaseOrderLine {
    /// Product warning message for this line, empty when the purchase warnings feature is disabled
    /// for the caller.
    #[compute(
        depends = ["product_id", "product_id.purchase_line_warn_msg"],
        depends_context = ["uid"]
    )]
    async fn _compute_purchase_line_warn_msg(
        this: &RecordSet<PurchaseOrderLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, PurchaseOrderLine>,
    ) -> Result<()> {
        if !tx.has_group(this.env(), WARNING_GROUP).await? {
            for index in 0..this.len() {
                out.set(index, PurchaseOrderLine::PURCHASE_LINE_WARN_MSG, None)?;
            }
            return Ok(());
        }
        let product_ids = this.mapped(tx, PurchaseOrderLine::PRODUCT_ID).await?;
        let unique_products: Vec<RecId> = product_ids
            .iter()
            .copied()
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut by_product: BTreeMap<RecId, Arc<str>> = BTreeMap::new();
        if !unique_products.is_empty() {
            let products = this
                .env()
                .records("product.product")?
                .browse_ids(unique_products);
            let rec_ids = products.rec_ids().to_vec();
            let msgs = products.values(tx, "purchase_line_warn_msg").await?;
            for (id, msg) in rec_ids.into_iter().zip(msgs) {
                if let Some(text) = non_empty_text("product.product.purchase_line_warn_msg", msg)? {
                    by_product.insert(id, text);
                }
            }
        }
        for (index, product_id) in product_ids.into_iter().enumerate() {
            let value = product_id.and_then(|id| by_product.get(&id).cloned());
            out.set(index, PurchaseOrderLine::PURCHASE_LINE_WARN_MSG, value)?;
        }
        Ok(())
    }
}

#[methods(PurchaseOrder, addon = "purchase")]
impl PurchaseOrder {
    /// Combined vendor, parent company, and product warnings for this order, empty when the
    /// purchase warnings feature is disabled for the caller.
    #[compute(
        depends = [
            "partner_id",
            "partner_id.name",
            "partner_id.display_name",
            "partner_id.purchase_warn_msg",
            "partner_id.parent_id",
            "partner_id.parent_id.name",
            "partner_id.parent_id.display_name",
            "partner_id.parent_id.purchase_warn_msg",
            "order_line",
            "order_line.purchase_line_warn_msg",
            "order_line.product_id.display_name"
        ],
        depends_context = ["uid"]
    )]
    async fn _compute_purchase_warning_text(
        this: &RecordSet<PurchaseOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, PurchaseOrder>,
    ) -> Result<()> {
        if !tx.has_group(this.env(), WARNING_GROUP).await? {
            for index in 0..this.len() {
                out.set(index, PurchaseOrder::PURCHASE_WARNING_TEXT, None)?;
            }
            return Ok(());
        }
        let env = this.env();
        let partner_ids = this.mapped(tx, PurchaseOrder::PARTNER_ID).await?;
        let order_lines = this.mapped(tx, PurchaseOrder::ORDER_LINE).await?;

        let unique_partners: Vec<RecId> = partner_ids
            .iter()
            .copied()
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut partners = read_partner_info(env, tx, unique_partners).await?;

        let missing_parents: Vec<RecId> = partners
            .values()
            .filter_map(|info| info.parent_id)
            .filter(|id| !partners.contains_key(id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !missing_parents.is_empty() {
            partners.extend(read_partner_info(env, tx, missing_parents).await?);
        }

        let unique_lines: Vec<RecId> = order_lines
            .iter()
            .flat_map(|ids| ids.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut line_warnings: BTreeMap<RecId, (Option<RecId>, Arc<str>)> = BTreeMap::new();
        let mut product_ids_to_load: BTreeSet<RecId> = BTreeSet::new();
        if !unique_lines.is_empty() {
            let lines = env.records("purchase.order.line")?.browse_ids(unique_lines);
            let rec_ids = lines.rec_ids().to_vec();
            let line_products = lines.values(tx, "product_id").await?;
            let line_msgs = lines.values(tx, "purchase_line_warn_msg").await?;
            for ((id, prod), msg) in rec_ids.into_iter().zip(line_products).zip(line_msgs) {
                if let Some(text) =
                    non_empty_text("purchase.order.line.purchase_line_warn_msg", msg)?
                {
                    let prod_id = optional_rec_id("purchase.order.line.product_id", prod)?;
                    if let Some(pid) = prod_id {
                        product_ids_to_load.insert(pid);
                    }
                    line_warnings.insert(id, (prod_id, text));
                }
            }
        }

        let mut product_names: BTreeMap<RecId, Arc<str>> = BTreeMap::new();
        if !product_ids_to_load.is_empty() {
            let products = env
                .records("product.product")?
                .browse_ids(product_ids_to_load);
            let rec_ids = products.rec_ids().to_vec();
            let display_names = products.values(tx, "display_name").await?;
            for (id, display_name) in rec_ids.into_iter().zip(display_names) {
                if let Some(name) = non_empty_text("product.product.display_name", display_name)? {
                    product_names.insert(id, name);
                }
            }
        }

        for (index, (partner_id, lines)) in partner_ids.into_iter().zip(order_lines).enumerate() {
            let mut entries: Vec<String> = Vec::new();
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut push_unique = |entry: String| {
                if seen.insert(entry.clone()) {
                    entries.push(entry);
                }
            };
            if let Some(pid) = partner_id
                && let Some(partner) = partners.get(&pid)
            {
                if let Some(msg) = &partner.warn_msg {
                    push_unique(format!("{} - {msg}", partner.label));
                }
                if let Some(parent_id) = partner.parent_id
                    && let Some(parent) = partners.get(&parent_id)
                    && let Some(parent_msg) = &parent.warn_msg
                {
                    push_unique(format!("{} - {parent_msg}", parent.label));
                }
            }
            for line_id in lines {
                if let Some((prod_id, msg)) = line_warnings.get(&line_id) {
                    let label = prod_id
                        .and_then(|pid| product_names.get(&pid))
                        .map_or("", Arc::as_ref);
                    push_unique(format!("{label} - {msg}"));
                }
            }
            let combined = (!entries.is_empty()).then(|| Arc::<str>::from(entries.join("\n")));
            out.set(index, PurchaseOrder::PURCHASE_WARNING_TEXT, combined)?;
        }
        Ok(())
    }
}
