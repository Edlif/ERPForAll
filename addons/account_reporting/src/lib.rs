// SPDX-License-Identifier: MIT OR Apache-2.0
//! Accounting report providers. Financial source reads always use the current caller.
erp_sdk::addon!(
    name = "account_reporting",
    exact_only = true,
    depends = ["account", "analytic", "reporting"]
);
mod aged_partner;
mod balance_sheet;
mod general_ledger;
mod migrations;
mod partner_ledger;
pub mod schema;
erp_addon_reporting::provider_declarations!(general_ledger::GeneralLedger);
