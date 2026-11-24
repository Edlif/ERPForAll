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
use erp_fields::{Cell, RecId};
use erp_orm::{Command, Context, Orm, RawVals, SUPERUSER_ID};
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

/// A cached database with sales, purchase and inventory installed, with their bridges; the ORM
/// with the compiled policy, and an administrator of the four apps who reads the forms (the
/// compiled forms resolve their groups for a user, never for the system principal).
async fn installed() -> (TestDb, Orm, erp_sdk::UserId) {
    let db = TemplateDb::clone_or_init("erp-suite-sale:final-state:v1", |pool| async move {
        let system = system_for_pool(pool);
        let initialized = Box::pin(system.initialize(InitOptions::new()))
            .await
            .map_err(|e| e.to_string())?;
        if !initialized.post_commit.is_clean() {
            return Err(format!("initialize: {:?}", initialized.post_commit));
        }
        let modules = ["sale_stock", "purchase_stock"].map(|name| ModuleName::new(name).unwrap());
        let installed =
            Box::pin(system.button_immediate_install(&system.system_env(Context::new()), &modules))
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
    let mut setup = loaded.orm.begin(SUPERUSER_ID).await.unwrap();
    let id = |row: &erp_testkit::tokio_postgres::Row| RecId::from_real(row.get(0)).unwrap();
    let groups: Vec<RecId> = setup
        .query(
            "SELECT res_id::int8 FROM ir_model_data WHERE model = 'res.groups' AND (module, name) IN \
             (('base', 'group_user'), ('base', 'group_system'), ('account', 'group_account_manager'), \
              ('stock', 'group_stock_manager'), ('sales_team', 'group_sale_manager'), \
              ('purchase', 'group_purchase_manager'))",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(id)
        .collect();
    assert_eq!(groups.len(), 6, "the app manager groups");
    let company = id(&setup
        .query(
            "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
            &[],
        )
        .await
        .unwrap()[0]);
    let reader = setup
        .env()
        .records("res.users")
        .unwrap()
        .create(
            &mut setup,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("Final-state reader".into()))
                    .set_cell("login", Cell::Str("final-state-reader".into()))
                    .set_cell("company_id", Cell::Id(company.as_real().unwrap().get()))
                    .set_commands("company_ids", [Command::set([company])])
                    .set_commands("group_ids", [Command::set(groups)]),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    setup.commit().await.unwrap();
    drop(setup);
    let orm = policy_orm(&db, &loaded.orm).await;
    (db, orm, erp_sdk::UserId::new(reader).unwrap())
}

/// The installed ORM with the database's compiled access policy, as a served node builds it: the
/// compiled forms resolve the groups their nodes name.
async fn policy_orm(db: &TestDb, installed: &Orm) -> Orm {
    let registry = std::sync::Arc::clone(installed.registry());
    let source = erp_security::source::PolicySource::build(registry.clone(), db.pool().clone())
        .await
        .unwrap();
    Orm::builder(registry.clone(), db.pool().clone())
        .generation_attachment(erp_orm::GenerationAttachment::new(
            registry.clone(),
            installed
                .env(SUPERUSER_ID, Context::new())
                .generation_attachment::<erp_views::PublishedViewTypes>()
                .unwrap(),
        ))
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .resolver(erp_security::resolver::Resolver::for_registry(&registry).unwrap())
        .policy(std::sync::Arc::new(source) as std::sync::Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap()
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

/// The form's read-only map of `guard.model` with its status field set to `state`.
async fn form_readonly(
    tx: &mut erp_orm::Transaction,
    guard: &Guarded,
    state: &str,
) -> BTreeMap<String, bool> {
    let env = tx.env();
    erp_addon_web::metadata_test_support::test_form_readonly(
        &env,
        tx,
        guard.model,
        &[(guard.status, ExprValue::str(state))],
    )
    .await
    .unwrap()
}

/// Every field of each guarded form that is editable in draft is, in each final state, read-only
/// exactly when the server refuses it.
#[test]
fn final_state_forms_show_read_only_exactly_what_the_server_refuses() {
    run(async {
        let (db, orm, reader) = installed().await;
        let mut tx = orm.begin(reader).await.unwrap();
        let mut disagreements = Vec::new();
        for guard in guarded() {
            // A field read-only already in draft is never edited on the form, so the final-state
            // allow-list says nothing about it.
            let draft = form_readonly(&mut tx, &guard, "draft").await;
            assert!(!draft.is_empty(), "{} has no form fields", guard.model);
            for (state, writable) in &guard.states {
                let form = form_readonly(&mut tx, &guard, state).await;
                for (field, readonly) in &form {
                    if draft.get(field) == Some(&true) {
                        continue;
                    }
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
