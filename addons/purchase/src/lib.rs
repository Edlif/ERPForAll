// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `purchase` for ERPForAll: requests for quotation and purchase orders.

erp_sdk::addon!(
    name = "purchase",
    depends = ["base", "account"],
    exact_only = true
);

mod bill_buttons;
mod bill_history;
mod bill_source_review;
mod direct_bill;
mod direct_bill_integrity;
pub mod line;
pub mod order;
mod res_groups;
pub mod settings;
mod warnings;

/// The purchase user: creates, confirms and cancels orders.
pub const USER_GROUP: &str = "purchase.group_purchase_user";
/// The purchase administrator: approves orders above the amount and unlocks.
pub const MANAGER_GROUP: &str = "purchase.group_purchase_manager";
/// The feature group controlling purchase warning messages on vendors, products, and orders.
pub const WARNING_GROUP: &str = "purchase.group_warning_purchase";
