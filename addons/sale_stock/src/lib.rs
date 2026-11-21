// SPDX-License-Identifier: MIT OR Apache-2.0
//! The direct one-step delivery bridge between Sales and Inventory.

erp_sdk::addon!(
    name = "sale_stock",
    depends = ["base", "sale", "stock", "stock_account"]
);

use erp_sdk::Result;
use erp_sdk::settings::ParameterDecl;

mod delivered;
mod delivery;
mod route;

const ROW_LIMIT: &str = "sale_stock.max_confirmation_source_rows";

fn parameters() -> Result<Vec<ParameterDecl>> {
    Ok(vec![ParameterDecl::integer(
        ROW_LIMIT,
        "Source rows in one Sales delivery confirmation",
        10_000,
        (1, 100_000),
    )?])
}
erp_sdk::system_parameters!(parameters);
