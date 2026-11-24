// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `stock_dropshipping` for ERPForAll.

erp_sdk::addon!(
    name = "stock_dropshipping",
    depends = ["purchase_stock", "sale_stock"]
);

mod picking_type;

pub use picking_type::{DropshipPicking, DropshipPickingType};
