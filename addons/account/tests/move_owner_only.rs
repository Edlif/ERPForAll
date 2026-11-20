// SPDX-License-Identifier: MIT OR Apache-2.0
//! Only the account addon declares stored fields on journal entries and items.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
use erp_addon_account::moves::AccountMoveLine;
use erp_sdk::prelude::*;
use erp_testkit::Harness;

erp_sdk::addon!(
    name = "move_satellite",
    depends = ["account"],
    exact_only = true
);

#[extend(AccountMoveLine, addon = "move_satellite")]
struct SatelliteLine {
    /// A stored column on the hot line table: refused.
    #[field(char)]
    pub satellite_note: Option<Arc<str>>,
}

#[tokio::test]
async fn another_addon_cannot_store_a_field_on_journal_items() {
    let error = Harness::builder([
        &erp_addon_base::ADDON,
        &erp_addon_web::ADDON,
        &erp_addon_uom::ADDON,
        &erp_addon_product::ADDON,
        &erp_addon_analytic::ADDON,
        &erp_addon_account::ADDON,
        &ADDON,
    ])
    .require_sql_objects([&erp_addon_account::reconcile::history_guard::MANIFEST])
    .schema_sync_options(erp_testkit::SchemaSyncOptions::inline().lifecycle(true))
    .build()
    .await
    .err()
    .expect("the registry refuses the satellite field");
    let message = format!("{error:?}");
    assert!(
        message.contains("move_satellite") && message.contains("satellite_note"),
        "{message}"
    );
}
