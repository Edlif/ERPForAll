// SPDX-License-Identifier: MIT OR Apache-2.0
//! The form and the server agree on records in a final state: for each guarded model and each of
//! its final states, every field of the compiled form outside the server's allow-list is read-only,
//! and every allow-listed field on the form is not. The allow-lists are the constants the server
//! guards use, so the two layers cannot drift apart unnoticed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]

use std::collections::BTreeMap;
use std::path::Path;

use erp_modules::{AddonIndex, InitOptions, ModuleName, ModuleSystem};
use erp_orm::{Context, Orm, SUPERUSER_ID};
use erp_sdk::expr::ExprValue;
use erp_testkit::{TemplateDb, TestDb};

fn system_for_pool(pool: erp_testkit::deadpool_postgres::Pool) -> ModuleSystem {
    let mut index = AddonIndex::new(true);
    index.add_dev_path(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../addons")
            .canonicalize()
            .unwrap(),
    );
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
    ModuleSystem::builder(pool, index)
        .native_artifact_identities(identities)
        .view_types(erp_addon_web::view_type_contributions())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_barcodes::ADDON,
            &erp_addon_barcodes_gs1_nomenclature::ADDON,
            &erp_addon_uom::ADDON,
            &erp_addon_mail::ADDON,
            &erp_addon_product::ADDON,
            &erp_addon_product_mail::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_analytic::ADDON,
            &erp_addon_account::ADDON,
            &erp_addon_account_mail::ADDON,
            &erp_addon_reporting::ADDON,
            &erp_addon_stock::ADDON,
            &erp_addon_stock_account::ADDON,
            &erp_addon_valuation::ADDON,
            &erp_addon_stock_mail::ADDON,
            &erp_addon_sales_team::ADDON,
            &erp_addon_sale::ADDON,
            &erp_addon_sale_stock::ADDON,
            &erp_addon_purchase::ADDON,
            &erp_addon_purchase_mail::ADDON,
            &erp_addon_purchase_stock::ADDON,
        ])
        .require_sql_objects([
            &erp_addon_account::reconcile::history_guard::MANIFEST,
            &erp_addon_stock::schema_guards::MANIFEST,
            &erp_addon_stock_account::history_guards::MANIFEST,
            &erp_addon_valuation::MANIFEST,
        ])
        .require_sql_uninstall_guards([
            &erp_addon_account::reconcile::history_guard::UNINSTALL,
            &erp_addon_stock_account::uninstall_guard::MANIFEST,
            &erp_addon_valuation::lifecycle::MANIFEST,
        ])
        .build()
        .unwrap()
}

/// Run `body` on a thread with a large stack: a full install chain exceeds the default one.
fn run(body: impl std::future::Future<Output = ()> + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(body);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A cached database with sales, purchase and inventory installed, with their bridges.
async fn installed() -> (TestDb, Orm) {
    let db = TemplateDb::clone_or_init("erp-suite-sale:final-state:v1", |pool| async move {
        let system = system_for_pool(pool);
        let initialized = Box::pin(system.initialize(InitOptions::new()))
            .await
            .map_err(|e| e.to_string())?;
        if !initialized.post_commit.is_clean() {
            return Err(format!("initialize: {:?}", initialized.post_commit));
        }
        let modules = ["sale_stock", "purchase_stock"].map(|name| ModuleName::new(name).unwrap());
        let installed = Box::pin(
            system.button_immediate_install(&system.system_env(Context::new()), &modules),
        )
        .await
        .map_err(|e| e.to_string())?;
        if !installed.post_commit.is_clean() {
            return Err(format!("install: {:?}", installed.post_commit));
        }
        Ok::<(), String>(())
    })
    .await
    .unwrap();
    let loaded = Box::pin(system_for_pool(db.pool().clone()).load())
        .await
        .unwrap();
    (db, loaded.orm)
}

/// One guarded model: its form, the field that holds its status, and per final state the fields
/// the server still accepts.
struct Guarded {
    model: &'static str,
    status: &'static str,
    states: Vec<(&'static str, &'static [&'static str])>,
}

fn guarded() -> Vec<Guarded> {
    vec![
        Guarded {
            model: "stock.picking",
            status: "state",
            states: vec![
                ("done", &erp_addon_stock::DONE_PICKING_WRITABLE),
                ("cancel", &erp_addon_stock::CANCELLED_PICKING_WRITABLE),
            ],
        },
        Guarded {
            model: "stock.move",
            status: "state",
            states: vec![
                ("done", &erp_addon_stock::DONE_MOVE_WRITABLE),
                ("cancel", &erp_addon_stock::DONE_MOVE_WRITABLE),
            ],
        },
        Guarded {
            model: "stock.picking.batch",
            status: "state",
            states: vec![
                ("done", &erp_addon_stock::batch::FINAL_BATCH_WRITABLE),
                ("cancel", &erp_addon_stock::batch::FINAL_BATCH_WRITABLE),
            ],
        },
    ]
}

/// Every field of each guarded form, in each final state, is read-only exactly when the server
/// refuses it.
#[test]
fn final_state_forms_show_read_only_exactly_what_the_server_refuses() {
    run(async {
        let (db, orm) = installed().await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let mut disagreements = Vec::new();
        for guard in guarded() {
            for (state, writable) in &guard.states {
                let form: BTreeMap<String, bool> =
                    erp_addon_web::metadata_test_support::test_form_readonly(
                        &tx.env(),
                        &mut tx,
                        guard.model,
                        &[(guard.status, ExprValue::str(*state))],
                    )
                    .await
                    .unwrap();
                assert!(!form.is_empty(), "{} has no form fields", guard.model);
                for (field, readonly) in &form {
                    let allowed = writable.contains(&field.as_str());
                    if allowed == *readonly {
                        disagreements.push(format!(
                            "{} {state}: {field} is {} on the form but {} by the server",
                            guard.model,
                            if *readonly { "read-only" } else { "editable" },
                            if allowed { "accepted" } else { "refused" },
                        ));
                    }
                }
            }
        }
        assert!(disagreements.is_empty(), "{disagreements:#?}");
        tx.rollback().await.unwrap();
        db.cleanup().await.unwrap();
    });
}
