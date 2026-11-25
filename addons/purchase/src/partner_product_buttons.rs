// SPDX-License-Identifier: MIT OR Apache-2.0
//! Smart buttons that connect vendors (`res.partner`) and products (`product.template`)
//! directly to their readable purchase orders.

use erp_addon_base::ResPartner;
use erp_addon_product::ProductTemplate;
use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::bill_buttons::{counts, flat, readable_links, window};

/// Partner id, purchase order id.
const PARTNER_PURCHASES: &str =
    "SELECT partner_id::int8, id::int8 FROM purchase_order WHERE partner_id = ANY($1)";

/// Product template id, purchase order id.
const PRODUCT_PURCHASES: &str = "SELECT DISTINCT p.product_tmpl_id::int8, l.order_id::int8 \
    FROM purchase_order_line l \
    JOIN product_product p ON p.id = l.product_id \
    WHERE p.product_tmpl_id = ANY($1)";

/// `res.partner` with its readable purchase order count.
#[extend(ResPartner, addon = "purchase")]
pub struct PartnerPurchaseOrders {
    /// Readable RFQs and purchase orders placed with this vendor.
    #[field(
        integer,
        compute = "_compute_purchase_order_count",
        string = "Purchase Orders"
    )]
    pub purchase_order_count: i32,
}

#[methods(ResPartner, addon = "purchase")]
impl PartnerPurchaseOrders {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_purchase_order_count(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ResPartner>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, PARTNER_PURCHASES, "purchase.order").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, PartnerPurchaseOrders::PURCHASE_ORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the readable purchase orders for this vendor.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_purchase_orders(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PARTNER_PURCHASES, "purchase.order").await?;
        Ok(window(
            "Purchase Orders",
            "purchase.order",
            Vec::new(),
            flat(found),
        ))
    }
}

/// `product.template` with the count of purchase orders sourcing its variants.
#[extend(ProductTemplate, addon = "purchase")]
pub struct ProductTemplatePurchases {
    /// Readable purchase orders containing this product.
    #[field(
        integer,
        compute = "_compute_purchase_order_count",
        string = "Purchase Count"
    )]
    pub purchase_order_count: i32,
}

#[methods(ProductTemplate, addon = "purchase")]
impl ProductTemplatePurchases {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_purchase_order_count(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductTemplate>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, PRODUCT_PURCHASES, "purchase.order").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, ProductTemplatePurchases::PURCHASE_ORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the readable purchase orders sourcing this product.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_purchases(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PRODUCT_PURCHASES, "purchase.order").await?;
        Ok(window(
            "Purchase Orders",
            "purchase.order",
            Vec::new(),
            flat(found),
        ))
    }
}
