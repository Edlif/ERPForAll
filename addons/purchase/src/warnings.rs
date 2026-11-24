// SPDX-License-Identifier: MIT OR Apache-2.0
//! Non-blocking purchase warning messages on vendors, products, purchase orders, and order lines.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_base::ResPartner;
use erp_addon_base::warning_text::{check_length, non_empty_text, optional_rec_id, readable_ids};
use erp_addon_product::ProductTemplate;
use erp_sdk::prelude::*;

use crate::WARNING_GROUP;
use crate::line::PurchaseOrderLine;
use crate::order::PurchaseOrder;

#[derive(Clone)]
struct PartnerWarningInfo {
    label: Arc<str>,
    parent_id: Option<RecId>,
    warn_msg: Option<Arc<str>>,
}

/// The vendor data of the partners among `ids` that the caller can read; a partner the caller's own
/// rights hide is absent from the map and contributes no message.
async fn read_partner_info(
    env: &Env,
    tx: &mut Transaction,
    ids: Vec<RecId>,
) -> Result<BTreeMap<RecId, PartnerWarningInfo>> {
    let ids = readable_ids(tx, env, "res.partner", ids).await?;
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let partners = env.records("res.partner")?.browse_ids(ids);
    let rec_ids = partners.rec_ids().to_vec();
    let names = partners.values(tx, "name").await?;
    let parents = partners.values(tx, "parent_id").await?;
    let warnings = partners.values(tx, "purchase_warn_msg").await?;
    let mut labels: BTreeMap<RecId, Arc<str>> = BTreeMap::new();
    let mut unnamed = Vec::new();
    for (&id, name) in rec_ids.iter().zip(names) {
        match non_empty_text("res.partner.name", name)? {
            Some(label) => {
                labels.insert(id, label);
            }
            None => unnamed.push(id),
        }
    }
    if !unnamed.is_empty() {
        let fallback = env.records("res.partner")?.browse_ids(unnamed);
        let ids = fallback.rec_ids().to_vec();
        for (id, display) in ids.into_iter().zip(fallback.values(tx, "display_name").await?) {
            if let Some(label) = non_empty_text("res.partner.display_name", display)? {
                labels.insert(id, label);
            }
        }
    }
    let mut map = BTreeMap::new();
    for ((&id, parent), warn) in rec_ids.iter().zip(parents).zip(warnings) {
        map.insert(
            id,
            PartnerWarningInfo {
                label: labels.remove(&id).unwrap_or_else(|| Arc::from("")),
                parent_id: optional_rec_id("res.partner.parent_id", parent)?,
                warn_msg: non_empty_text("res.partner.purchase_warn_msg", warn)?,
            },
        );
    }
    Ok(map)
}

/// One banner line: the record label and its message; no separator when the record has no label.
fn banner_entry(label: &str, msg: &str) -> String {
    if label.is_empty() {
        msg.to_owned()
    } else {
        format!("{label} - {msg}")
    }
}

#[extend(ResPartner, addon = "purchase")]
pub(crate) struct PurchasePartnerWarnings {
    /// Non-blocking notice shown on purchase orders for this vendor. Orders show it only to a
    /// caller who can read this partner.
    #[field(
        text,
        groups = "purchase.group_warning_purchase",
        string = "Vendor notice"
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
        check_length(tx, &crate::ADDON, "Purchase order notice", values).await
    }
}

#[extend(ProductTemplate, addon = "purchase")]
pub(crate) struct PurchaseProductWarnings {
    /// Non-blocking notice shown on purchase order lines for this product. Lines show it only to a
    /// caller who can read this product.
    #[field(
        text,
        groups = "purchase.group_warning_purchase",
        string = "Product notice"
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
        check_length(tx, &crate::ADDON, "Purchase order line notice", values).await
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
        // elevated read, but only the products the caller's own rights can read give a message
        let unique_products =
            readable_ids(tx, this.env(), "product.product", unique_products).await?;
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

        // A product is in the map only when the caller's own rights can read it; its label may be empty.
        let mut product_names: BTreeMap<RecId, Arc<str>> = BTreeMap::new();
        let readable_products = readable_ids(
            tx,
            env,
            "product.product",
            product_ids_to_load.into_iter().collect(),
        )
        .await?;
        if !readable_products.is_empty() {
            let products = env.records("product.product")?.browse_ids(readable_products);
            let rec_ids = products.rec_ids().to_vec();
            let display_names = products.values(tx, "display_name").await?;
            for (id, display_name) in rec_ids.into_iter().zip(display_names) {
                let name = non_empty_text("product.product.display_name", display_name)?
                    .unwrap_or_else(|| Arc::from(""));
                product_names.insert(id, name);
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
                    push_unique(banner_entry(&partner.label, msg));
                }
                if let Some(parent_id) = partner.parent_id
                    && let Some(parent) = partners.get(&parent_id)
                    && let Some(parent_msg) = &parent.warn_msg
                {
                    push_unique(banner_entry(&parent.label, parent_msg));
                }
            }
            for line_id in lines {
                if let Some((prod_id, msg)) = line_warnings.get(&line_id)
                    && let Some(label) = prod_id.and_then(|pid| product_names.get(&pid))
                {
                    push_unique(banner_entry(label, msg));
                }
            }
            let combined = (!entries.is_empty()).then(|| Arc::<str>::from(entries.join("\n")));
            out.set(index, PurchaseOrder::PURCHASE_WARNING_TEXT, combined)?;
        }
        Ok(())
    }
}
