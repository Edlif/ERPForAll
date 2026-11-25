// SPDX-License-Identifier: MIT OR Apache-2.0
//! Smart buttons that connect contacts (`res.partner`) and products (`product.template`)
//! directly to their readable confirmed sales orders (`state = 'sale'`), the same rule as the
//! purchase buttons. The partner count is one grouped read under the caller's read access; the
//! product count follows the order lines, which no single relation field of the order names, so
//! it filters the linked order ids by read access. Both actions open a domain, never an id list.

use erp_addon_base::ResPartner;
use erp_addon_product::ProductTemplate;
use erp_sdk::prelude::*;

use erp_addon_account::smart_buttons::{domain_action, grouped_counts, leaf, the_one};

use crate::invoice_buttons::{counts, readable_links};

/// The state of the orders the buttons count: confirmed sales orders.
const CONFIRMED: &str = "sale";

/// Product template id, confirmed sale order id.
const PRODUCT_SALES: &str = "SELECT DISTINCT p.product_tmpl_id::int8, l.order_id::int8 \
    FROM sale_order_line l \
    JOIN product_product p ON p.id = l.product_id \
    JOIN sale_order o ON o.id = l.order_id \
    WHERE p.product_tmpl_id = ANY($1) AND o.state = 'sale'";

/// `res.partner` with its readable sales order count.
#[extend(ResPartner, addon = "sale")]
pub struct PartnerSalesOrders {
    /// Readable confirmed sales orders placed by this contact.
    #[field(
        integer,
        compute = "_compute_sale_order_count",
        string = "Sales Orders"
    )]
    pub sale_order_count: i32,
}

#[methods(ResPartner, addon = "sale")]
impl PartnerSalesOrders {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_sale_order_count(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ResPartner>,
    ) -> Result<()> {
        let records = this.as_records();
        let confirmed = Domain::field("state")?.eq(CONFIRMED);
        let found = grouped_counts(tx, records, "sale.order", "partner_id", confirmed).await?;
        for (index, count) in found.into_iter().enumerate() {
            out.set(index, PartnerSalesOrders::SALE_ORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the confirmed sales orders of this contact.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_sale_orders(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let id = the_one(this.as_records(), tx).await?;
        domain_action(
            tx,
            this.as_records(),
            "Sales Orders",
            "sale.order",
            Vec::new(),
            vec![
                Value::str("&"),
                leaf("partner_id", "=", Value::Int(id)),
                leaf("state", "=", Value::str(CONFIRMED)),
            ],
        )
        .await
    }
}

/// `product.template` with the count of confirmed sales orders containing its variants.
#[extend(ProductTemplate, addon = "sale")]
pub struct ProductTemplateSales {
    /// Readable confirmed sales orders that sell this product.
    #[field(
        integer,
        compute = "_compute_sales_order_count",
        string = "Sales Count"
    )]
    pub sales_order_count: i32,
}

#[methods(ProductTemplate, addon = "sale")]
impl ProductTemplateSales {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_sales_order_count(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductTemplate>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, PRODUCT_SALES, "sale.order").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, ProductTemplateSales::SALES_ORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the confirmed sales orders containing this product.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_sales(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let id = the_one(this.as_records(), tx).await?;
        domain_action(
            tx,
            this.as_records(),
            "Sales Orders",
            "sale.order",
            Vec::new(),
            vec![
                Value::str("&"),
                leaf("order_line.product_id.product_tmpl_id", "=", Value::Int(id)),
                leaf("state", "=", Value::str(CONFIRMED)),
            ],
        )
        .await
    }
}
