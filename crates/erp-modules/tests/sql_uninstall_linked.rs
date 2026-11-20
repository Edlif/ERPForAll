// SPDX-License-Identifier: MIT OR Apache-2.0
//! A linked guard of an unrelated addon is not a contribution to every module system.
#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod common;
use common::*;
use erp_orm::BoxFuture;
use erp_sdk::sql_uninstall::{SqlGuardRead, SqlUninstallGuard, SqlUninstallGuardManifest};
use erp_testkit::TestDb;
fn unused<'a>(_: &'a mut SqlGuardRead<'_>) -> BoxFuture<'a, erp_fields::Result<u64>> {
    Box::pin(async { Ok(0) })
}
#[erp_sdk::sql_uninstall::linkme::distributed_slice(erp_sdk::sql_uninstall::SQL_UNINSTALL_GUARDS)]
#[linkme(crate = erp_sdk::sql_uninstall::linkme)]
static UNRELATED: SqlUninstallGuard = SqlUninstallGuard::new(
    "unrelated",
    "protected",
    &["unrelated.item"],
    "blocked",
    "Blocked",
    unused,
);
#[tokio::test]
async fn unrelated_linked_guard_is_filtered_with_its_native_descriptor() {
    let db = TestDb::create().await.unwrap();
    let addons = Addons::new();
    // Only base is composed; unrelated's declaration is not this system's expected registration.
    builder(&db, &addons).build().unwrap();
    db.cleanup().await.unwrap();
}

static SDK_ADDON: erp_registry::AddonDescriptor = erp_registry::AddonDescriptor {
    name: "sdk_guard",
    depends: &["base"],
    exact_only: true,
};
static EXPECTED: SqlUninstallGuardManifest =
    SqlUninstallGuardManifest::new("sdk_guard", &["history"]);
fn history<'a>(read: &'a mut SqlGuardRead<'_>) -> BoxFuture<'a, erp_fields::Result<u64>> {
    Box::pin(async move { read.count_all("sdk_guard.item").await })
}
#[erp_sdk::sql_uninstall::linkme::distributed_slice(erp_sdk::sql_uninstall::SQL_UNINSTALL_GUARDS)]
#[linkme(crate = erp_sdk::sql_uninstall::linkme)]
static SDK_GUARD: SqlUninstallGuard = SqlUninstallGuard::new(
    "sdk_guard",
    "history",
    &["sdk_guard.item"],
    "history",
    "SDK linked history prevents removal",
    history,
);

#[tokio::test]
async fn sdk_guard_is_collected_by_an_independent_module_host() {
    let db = TestDb::create().await.unwrap();
    let addons = Addons::new();
    addons.add(
        "sdk_guard",
        "name='SDK Guard'\ndepends=['base']\nexact_only=true\n",
    );
    let system = builder(&db, &addons)
        .code([&SDK_ADDON])
        .decls([model("sdk_guard", "sdk_guard.item")])
        .require_sql_uninstall_guards([&EXPECTED])
        .no_data_loader()
        .build()
        .unwrap();
    let init = system
        .initialize(erp_modules::InitOptions::new().with_skip_auto_install(true))
        .await
        .unwrap();
    let installed = system
        .button_immediate_install(&su(&init.orm), &[n("sdk_guard")])
        .await
        .unwrap();
    let mut tx = installed.orm.begin(erp_orm::SUPERUSER_ID).await.unwrap();
    tx.execute("INSERT INTO sdk_guard_item(name) VALUES ('retained')", &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let error = system
        .button_immediate_uninstall(&su(&installed.orm), &[n("sdk_guard")])
        .await
        .unwrap_err();
    let erp_modules::Error::Orm(error) = error else {
        panic!("wrong error {error:?}")
    };
    assert_eq!(error.kind(), erp_fields::ErrorKind::UserError);
    assert_eq!(error.message(), "SDK linked history prevents removal (1)");
}
