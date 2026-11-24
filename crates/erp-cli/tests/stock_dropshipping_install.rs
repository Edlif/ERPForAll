// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `stock_dropshipping` addon installation: installing the addon on a fresh database
//! installs the module and its dependencies together, marked `installed` in `ir_module_module`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use erp_modules::system::{InitOptions, ModuleSystem};
use erp_modules::{AddonIndex, ModuleName};
use erp_orm::{Context, Orm, SUPERUSER_ID};
use erp_security::{resolver::Resolver, source::PolicySource};
use erp_testkit::TestDb;

fn addons_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("addons")
}

/// Run `body` on a thread with a large stack: a full install chain exceeds the default one.
fn run(body: impl std::future::Future<Output = ()> + Send + 'static) {
    run_body(move || body)
}

fn run_body<F: std::future::Future<Output = ()>>(build: impl FnOnce() -> F + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(build());
        })
        .unwrap()
        .join()
        .unwrap();
}

fn dropship_module_system(db: &TestDb) -> ModuleSystem {
    let mut index = AddonIndex::new(true);
    index.add_dev_path(addons_dir());
    let identities = index
        .discover_entries()
        .unwrap()
        .into_iter()
        .map(|(name, entry)| {
            let snapshot = erp_modules::artifact_identity::ArtifactSnapshot::capture(
                name.as_str(),
                Path::new(entry.source_location()),
            )
            .unwrap();
            (name, snapshot.digest().to_owned())
        })
        .collect();
    ModuleSystem::builder(db.pool().clone(), index)
        .view_types(erp_addon_web::view_type_contributions())
        .native_artifact_identities(identities)
        .code(erp_cli::native_addons::DESCRIPTORS.iter().copied())
        .require_sql_objects(erp_cli::native_addons::SQL_MANIFESTS.iter().copied())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .require_core_tables(erp_security::auth::core_tables())
        .build()
        .unwrap()
}

async fn install_module(db: &TestDb, module: &str) -> Orm {
    install_modules(db, &[module]).await
}

async fn install_modules(db: &TestDb, modules: &[&str]) -> Orm {
    let system = dropship_module_system(db);
    let init = Box::pin(system.initialize(InitOptions::new().with_skip_auto_install(true)))
        .await
        .unwrap();
    let env = init.orm.env(SUPERUSER_ID, Context::new());
    let installed = Box::pin(
        system.button_immediate_install(
            &env,
            &modules
                .iter()
                .map(|name| ModuleName::new(*name).unwrap())
                .collect::<Vec<_>>(),
        ),
    )
    .await
    .unwrap();
    assert!(
        installed.post_commit.is_clean(),
        "{:?}",
        installed.post_commit
    );
    dropship_policy_orm(db, &installed.orm).await
}

async fn dropship_policy_orm(db: &TestDb, installed: &Orm) -> Orm {
    let registry = Arc::clone(installed.registry());
    let compiler = erp_security::Compiler::new(Arc::clone(&registry));
    let source = PolicySource::build_with(registry, db.pool().clone(), compiler)
        .await
        .unwrap();
    Orm::builder(Arc::clone(installed.registry()), db.pool().clone())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .resolver(Resolver::for_registry(installed.registry()).unwrap())
        .policy(Arc::new(source) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap()
}

#[test]
fn stock_dropshipping_installs_on_fresh_database_with_dependencies() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "stock_dropshipping").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();

        let rows = tx
            .query(
                "SELECT state FROM ir_module_module WHERE name = 'stock_dropshipping'",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "stock_dropshipping row must exist");
        let state: String = rows[0].get(0);
        assert_eq!(state, "installed");

        for dep in [
            "base",
            "barcodes",
            "uom",
            "product",
            "stock",
            "account",
            "stock_account",
            "purchase",
            "sales_team",
            "sale",
            "purchase_stock",
            "sale_stock",
        ] {
            let dep_rows = tx
                .query(
                    "SELECT state FROM ir_module_module WHERE name = $1",
                    &[&dep],
                )
                .await
                .unwrap();
            assert_eq!(dep_rows.len(), 1, "module row for {dep} must exist");
            let dep_state: String = dep_rows[0].get(0);
            assert_eq!(
                dep_state, "installed",
                "expected dependency {dep} to be installed"
            );
        }
    });
}
