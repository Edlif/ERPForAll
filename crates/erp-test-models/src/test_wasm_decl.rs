// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixture "wasm decl" (WASM.MODEL-DECL): one model with the field attributes and model
//! arguments a package may now declare, and a tree model with a parent store. The package test
//! reads the macro's own `describe()` text of this addon, so the token spacing the installer
//! parses is the real one.

use erp_sdk::prelude::*;

erp_sdk::addon!(name = "test_wasm_decl", depends = ["base"]);

/// Every field attribute and the business key.
#[model(
    name = "test_wasm_decl.asset",
    description = "Asset",
    addon = "test_wasm_decl",
    keys = [
        key(
            name = "_serial_uniq",
            fields = ["serial_no"],
            message = "The serial number must be unique.",
            nulls_distinct = true
        ),
        key(
            name = "_name_site_key",
            fields = ["name", "site"],
            message = "Name and site must be unique."
        )
    ]
)]
pub struct Asset {
    #[field(char, index)]
    pub name: Option<Arc<str>>,
    #[field(char, index = "trigram")]
    pub label: Option<Arc<str>>,
    #[field(char, index = true)]
    pub site: Option<Arc<str>>,
    #[field(char)]
    pub serial_no: Option<Arc<str>>,
    #[field(selection, selection = [("draft", "Draft"), ("done", "Done")], default = "draft")]
    pub state: Option<Arc<str>>,
    #[field(float, digits = (16, 2), default = 1.5)]
    pub amount: Decimal,
    #[field(integer, default = -1)]
    pub rank: i32,
    #[field(float, default = -2.5)]
    pub ratio: f64,
    #[field(boolean, default = true)]
    pub active_flag: bool,
    #[field(char, default = False)]
    pub note: Option<Arc<str>>,
    #[field(char, groups = "base.group_system")]
    pub secret: Option<Arc<str>>,
}

/// A tree with a parent store.
#[model(
    name = "test_wasm_decl.node",
    description = "Node",
    addon = "test_wasm_decl",
    parent_store = true,
    parent_name = "parent_id"
)]
pub struct Node {
    #[field(char)]
    pub name: Option<Arc<str>>,
    #[field(many2one, comodel = "test_wasm_decl.node")]
    pub parent_id: Option<RecId>,
    #[field(char, index)]
    pub parent_path: Option<Arc<str>>,
}
