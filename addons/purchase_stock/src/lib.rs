// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `purchase_stock` for ERPForAll.

erp_sdk::addon!(
    name = "purchase_stock",
    depends = ["stock", "stock_account", "purchase"]
);

mod receipt;
mod receipt_access;
