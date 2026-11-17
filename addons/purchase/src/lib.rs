// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `purchase` for ERPForAll: requests for quotation and purchase orders.

erp_sdk::addon!(
    name = "purchase",
    depends = ["base", "account"],
    exact_only = true
);

pub mod line;
pub mod order;
pub mod settings;

/// The purchase user: creates, confirms and cancels orders.
pub const USER_GROUP: &str = "purchase.group_purchase_user";
/// The purchase administrator: approves orders above the amount and unlocks.
pub const MANAGER_GROUP: &str = "purchase.group_purchase_manager";
