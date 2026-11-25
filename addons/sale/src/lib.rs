// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `sale`: quotations and sales orders (`sale.order`) and their lines (`sale.order.line`):
//! the order number, states, lock, addresses, salesperson and team (B2), the lines with
//! their guards, quantities and statuses, and the invoicing policy of products and companies (B3).

erp_sdk::addon!(
    name = "sale",
    exact_only = true,
    depends = [
        "base",
        "web",
        "reporting",
        "sales_team",
        "product",
        "mail",
        "account"
    ]
);

mod amounts;
mod create_invoice;
pub mod discount;
pub mod downpayment;
mod invoice_buttons;
pub use invoice_buttons::{counts, flat, readable_links, window};
mod invoice_lineage;
pub mod line;
pub mod loyalty;
pub mod margin;
mod numbering;
pub mod order;
pub mod order_ops;
pub mod order_status;
mod partner_product_buttons;
pub mod product;
pub mod report;
pub mod res_groups;
pub mod send;
pub mod settings;
pub mod team_dashboard;
pub mod template;

pub use line::SaleOrderLine;
pub use order::{PLACEHOLDER, ProtectedFields, SEQUENCE_CODE, SaleOrder};
pub use order_ops::LOCK_GROUP;
pub use product::{CompanySale, ProductTemplateSale};
pub use settings::LOCK_CONFIRMED;
