// SPDX-License-Identifier: MIT OR Apache-2.0
//! Installing `sales_team` on a database whose first administrator already exists: the Sales
//! privilege and its groups, the access rows, the default team and its one membership, the menus.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::path::Path;

use erp_fields::{Cell, RecId};
use erp_modules::{AddonIndex, InitOptions, ModuleName, ModuleSystem};
use erp_orm::{Command, Context, RawVals, SUPERUSER_ID};
use erp_testkit::TestDb;

#[path = "support/install_defaults.rs"]
mod install_defaults;

fn system(db: &TestDb) -> ModuleSystem {
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
    ModuleSystem::builder(db.pool().clone(), index)
        .native_artifact_identities(identities)
        .view_types(erp_addon_web::view_type_contributions())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_mail::ADDON,
            &erp_addon_uom::ADDON,
            &erp_addon_sales_team::ADDON,
        ])
        .build()
        .unwrap()
}

async fn identity(tx: &mut erp_orm::Transaction, name: &str, model: &str) -> i64 {
    tx.query(
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = $1::text \
         AND model = $2::text",
        &[&name, &model],
    )
    .await
    .unwrap()[0]
        .get(0)
}

async fn strings(client: &tokio_postgres::Client, sql: &str) -> Vec<String> {
    client
        .query(sql, &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect()
}

#[test]
fn sales_team_installs_its_groups_access_and_default_team() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(install());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn install() {
    let db = TestDb::create().await.unwrap();
    let system = system(&db);
    let init = Box::pin(system.initialize(InitOptions::new().with_skip_auto_install(true)))
        .await
        .unwrap();
    // the first administrator, created as `erp db init` does before any application is installed
    let mut tx = init.orm.begin(SUPERUSER_ID).await.unwrap();
    let company = identity(&mut tx, "main_company", "res.company").await;
    let system_group = identity(&mut tx, "group_system", "res.groups").await;
    let partner = tx
        .env()
        .records("res.partner")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("Admin".into()))
                    .set_cell("company_id", Cell::Id(company)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    let admin = tx
        .env()
        .records("res.users")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("Admin".into()))
                    .set_cell("login", Cell::Str("admin".into()))
                    .set_cell("partner_id", Cell::Id(partner))
                    .set_cell("company_id", Cell::Id(company))
                    .set_commands(
                        "company_ids",
                        [Command::set([RecId::from_real(company).unwrap()])],
                    )
                    .set_commands(
                        "group_ids",
                        [Command::set([RecId::from_real(system_group).unwrap()])],
                    ),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    tx.commit().await.unwrap();
    let env = init.orm.env(SUPERUSER_ID, Context::new());
    let outcome =
        Box::pin(system.button_immediate_install(&env, &[ModuleName::new("sales_team").unwrap()]))
            .await
            .unwrap();
    assert!(outcome.post_commit.is_clean());
    let client = db.client().await.unwrap();

    for (sql, expected) in install_defaults::SALES_TEAM {
        assert_eq!(strings(&client, sql).await, *expected, "{sql}");
    }
    let leader = client
        .query_one(
            "SELECT t.user_id::int8, count(m.id)::int8, bool_and(m.active) FROM crm_team t \
             JOIN ir_model_data d ON d.res_id = t.id AND d.model = 'crm.team' \
              AND d.module = 'sales_team' AND d.name = 'team_sales_department' \
             LEFT JOIN crm_team_member m ON m.crm_team_id = t.id GROUP BY t.user_id",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(leader.get::<_, Option<i64>>(0), Some(admin), "team leader");
    assert_eq!(leader.get::<_, i64>(1), 1, "one membership");
    assert_eq!(
        leader.get::<_, Option<bool>>(2),
        Some(true),
        "the membership is active"
    );
    let member_of: Vec<i64> = client
        .query(
            "SELECT m.user_id::int8 FROM crm_team_member m ORDER BY m.id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(member_of, [admin], "the administrator is the only member");
}
