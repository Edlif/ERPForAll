// SPDX-License-Identifier: MIT OR Apache-2.0
//! Keep the restricted writer policy installed while any admitted control survives.
use erp_sdk::sql_uninstall::{SqlGuardRead, SqlUninstallGuard, SqlUninstallGuardManifest};
use erp_sdk::{BoxFuture, Result};

/// Guard inventory checked by module lifecycle.
pub static MANIFEST: SqlUninstallGuardManifest =
    SqlUninstallGuardManifest::new("valuation", &["admitted_receipts"]);

#[erp_sdk::sql_uninstall::linkme::distributed_slice(erp_sdk::sql_uninstall::SQL_UNINSTALL_GUARDS)]
#[linkme(crate = erp_sdk::sql_uninstall::linkme)]
static GUARD: SqlUninstallGuard = SqlUninstallGuard::new(
    "valuation",
    "admitted_receipts",
    &["stock.valuation.control"],
    "valuation_controls_present",
    "Stock valuation admission controls prevent uninstalling Stock Valuation Admission.",
    controls,
);

fn controls<'a>(read: &'a mut SqlGuardRead<'_>) -> BoxFuture<'a, Result<u64>> {
    Box::pin(async move { read.count_all("stock.valuation.control").await })
}
