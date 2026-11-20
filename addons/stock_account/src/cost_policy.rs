// SPDX-License-Identifier: MIT OR Apache-2.0
//! FIFO is an Inventory Accounting extension to the company's primary costing configuration.

use erp_addon_base::ResCompany;
use erp_addon_product::{ProductCategory, ProductTemplate};
use erp_sdk::prelude::*;

#[extend(ResCompany, addon = "stock_account")]
struct CompanyFifo {
    #[field(selection, selection_add = [("fifo", "First in, first out (FIFO)")], ondelete = [("fifo", "set default")])]
    cost_method: Option<Arc<str>>,
}

#[extend(ProductCategory, addon = "stock_account")]
struct CategoryFifo {
    #[field(selection, selection_add = [("fifo", "First in, first out (FIFO)")], ondelete = [("fifo", "set null")])]
    property_cost_method: Option<Arc<str>>,
}

#[extend(ProductTemplate, addon = "stock_account")]
struct ProductFifo {
    #[field(selection, selection_add = [("fifo", "First in, first out (FIFO)")], ondelete = [("fifo", "set null")])]
    cost_method: Option<Arc<str>>,
}
