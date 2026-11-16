// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `account` for ERPForAll: the accounting application.
//! This file holds only the declaration and, later, sorted `mod` lines; one directory per slice family.

erp_sdk::addon!(
    name = "account",
    depends = ["base", "web", "product", "analytic"],
    exact_only = true
);

pub mod company;
pub mod fiscal;
pub mod fpos;
pub mod invoice;
pub mod journal;
pub mod ledger;
pub mod locking;
pub mod moves;
pub mod numbering;

pub mod tax;
