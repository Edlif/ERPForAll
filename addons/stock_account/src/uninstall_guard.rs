// SPDX-License-Identifier: MIT OR Apache-2.0
//! Lifecycle protection for historical valuation evidence.
use erp_sdk::sql_uninstall::{SqlGuardRead, SqlUninstallGuard, SqlUninstallGuardManifest};
use erp_sdk::{BoxFuture, Error, Result};

/// The lifecycle builder checks that the linked guard is present before admitting a module operation.
pub static MANIFEST: SqlUninstallGuardManifest =
    SqlUninstallGuardManifest::new("stock_account", &["valuation_history"]);

#[erp_sdk::sql_uninstall::linkme::distributed_slice(erp_sdk::sql_uninstall::SQL_UNINSTALL_GUARDS)]
#[linkme(crate = erp_sdk::sql_uninstall::linkme)]
static VALUATION_HISTORY: SqlUninstallGuard = SqlUninstallGuard::new(
    "stock_account",
    "valuation_history",
    &["stock.valuation.source", "stock.valuation.journal"],
    "stock_valuation_history_present",
    "Stock valuation history prevents uninstalling Stock Account.",
    history_rows,
);

fn history_rows<'a>(read: &'a mut SqlGuardRead<'_>) -> BoxFuture<'a, Result<u64>> {
    Box::pin(async move {
        let sources = read.count_all("stock.valuation.source").await?;
        let journal = read.count_all("stock.valuation.journal").await?;
        sources.checked_add(journal).ok_or_else(|| {
            Error::programming("Valuation history count exceeds the lifecycle limit.")
        })
    })
}
