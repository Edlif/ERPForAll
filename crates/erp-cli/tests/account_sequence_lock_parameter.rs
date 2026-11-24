// SPDX-License-Identifier: MIT OR Apache-2.0
//! Installed Accounting version transition for the sequence lock wait.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]

#[path = "support/accounting.rs"]
mod accounting;

use accounting::{big_stack, install, main_company, policy_orm, system_with_signal, user, xml_id};
use erp_addon_account::numbering::{LOCK_WAIT_SETTING, LockWait};
use erp_addon_base::ir_config_setting::read_values;
use erp_modules::ModuleName;
use erp_orm::{Changes, Orm, Refresh, Refreshed, Signal, SignalKey};
use erp_sdk::prelude::*;
use erp_sdk::views::wire::SettingsScope;
use erp_sdk::{Context, ErrorKind, SUPERUSER_ID, UserId};

struct UpgradeRefresh {
    retiring: bool,
}

impl Refresh for UpgradeRefresh {
    fn refresh<'a>(&'a self, changes: &'a Changes) -> erp_orm::BoxFuture<'a, Result<Refreshed>> {
        Box::pin(async move {
            // A running HTTP registry is superseded, whereas the owner CLI can continue its
            // lifecycle work. These match NativeRefresh and OwnerRefresh in the server and CLI.
            if self.retiring && changes.registry {
                Ok(Refreshed::Superseded)
            } else {
                Ok(Refreshed::InPlace)
            }
        })
    }
}

#[test]
fn installed_sequence_wait_moves_atomically_to_system_parameters() {
    big_stack(upgrade_body);
}

async fn upgrade_body() {
    let (db, installed) = install().await;
    let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
    assert_eq!(
        LockWait::from_settings(&mut tx).await.unwrap().millis(),
        5000
    );
    let company = main_company(&mut tx).await;
    let administrator_group = xml_id(&mut tx, "base", "group_system").await;
    let administrator = user(
        &mut tx,
        "sequence.wait.admin",
        company,
        &[administrator_group],
    )
    .await;
    let reader_group = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader = user(&mut tx, "sequence.wait.reader", company, &[reader_group]).await;
    tx.install_signal().await.unwrap();
    tx.commit().await.unwrap();

    let client = db.client().await.unwrap();
    let seeded: String = client
        .query_one(
            "SELECT value FROM ir_config_parameter WHERE key=$1",
            &[&LOCK_WAIT_SETTING],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(seeded, "5000");
    // Hold every other installed schema and artifact constant and exercise the 1.8 data shape.
    client
        .batch_execute(
            "INSERT INTO ir_config_setting_scope(identity,revision) VALUES ('global','7')
               ON CONFLICT (identity) DO UPDATE SET revision='7';
             INSERT INTO ir_config_setting_value(scope_id,setting_key,value_text)
               SELECT id,'account.sequence_lock_wait_ms','0' FROM ir_config_setting_scope WHERE identity='global';
             UPDATE ir_config_parameter SET value='1200' WHERE key='account.sequence_lock_wait_ms';
             UPDATE ir_module_module SET latest_version='1.8.0' WHERE name='account';",
        )
        .await
        .unwrap();
    let baseline = Signal::read(&client).await.unwrap();
    let retiring = Orm::builder(installed.registry().clone(), db.pool().clone())
        .signal(Signal::new(baseline.clone()).with_refresh(UpgradeRefresh { retiring: true }))
        .build()
        .unwrap();
    let system = system_with_signal(
        &db,
        None,
        Some(Signal::new(baseline.clone()).with_refresh(UpgradeRefresh { retiring: false })),
    );
    let account = [ModuleName::new("account").unwrap()];
    let service = system.system_env(Context::new());
    let refusal = system
        .button_immediate_upgrade(&service, &account)
        .await
        .unwrap_err();
    let refusal = migration_error(refusal);
    assert_eq!(refusal.kind(), ErrorKind::Validation);
    assert_eq!(
        refusal.message(),
        "account.sequence_lock_wait_ms takes a whole number from 100 to 60000."
    );
    assert_store(&client, "1200", "0", "1.8.0", &[]).await;
    let failed_epoch = Signal::read(&client).await.unwrap();
    assert_eq!(
        failed_epoch.get(&SignalKey::Registry),
        baseline.get(&SignalKey::Registry)
    );

    client
        .execute(
            "UPDATE ir_config_setting_value SET value_text='1800' WHERE setting_key=$1",
            &[&LOCK_WAIT_SETTING],
        )
        .await
        .unwrap();

    // The previous Settings runtime reads its declaration and begins REPEATABLE READ before
    // acquiring the scope lock for the compare-and-set. Keep that snapshot open across upgrade.
    let mut legacy_client = db.client().await.unwrap();
    let legacy_write = legacy_client
        .build_transaction()
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await
        .unwrap();
    let seen: String = legacy_write
        .query_one(
            "SELECT revision FROM ir_config_setting_scope WHERE identity='global'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(seen, "7");

    let upgraded = system
        .button_immediate_upgrade(&service, &account)
        .await
        .unwrap();
    assert!(upgraded.post_commit.is_clean());
    let refused = legacy_write
        .query_one(
            "SELECT revision FROM ir_config_setting_scope WHERE identity='global' FOR UPDATE",
            &[],
        )
        .await
        .expect_err("a predecessor snapshot must not pass the global Settings compare-and-set");
    assert_eq!(
        refused.code(),
        Some(&tokio_postgres::error::SqlState::T_R_SERIALIZATION_FAILURE)
    );
    legacy_write.rollback().await.unwrap();
    let committed_epoch = Signal::read(&client).await.unwrap();
    assert_eq!(
        committed_epoch.get(&SignalKey::Registry),
        baseline.get(&SignalKey::Registry) + 1
    );
    let mut next_request = retiring.begin(SUPERUSER_ID).await.unwrap();
    let error = next_request.query("SELECT 1", &[]).await.unwrap_err();
    assert!(error.is_superseded());
    let audit = ["migration:account@1.9 1200>1800 @7"];
    assert_store(&client, "1800", "1800", account_version(), &audit).await;
    let repeated = system
        .button_immediate_upgrade(&service, &account)
        .await
        .unwrap();
    assert!(repeated.post_commit.is_clean());
    assert_store(&client, "1800", "1800", account_version(), &audit).await;

    let orm = policy_orm(&db, &repeated.orm).await;
    let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
    assert_eq!(
        LockWait::from_settings(&mut tx).await.unwrap().millis(),
        1800
    );
    let params = tx.env().records("ir.config_parameter").unwrap();
    let denied = params
        .call::<_, i64>(
            &mut tx,
            "set_int",
            (LOCK_WAIT_SETTING.to_owned(), Some(2000_i64)),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.kind(), ErrorKind::Access);
    tx.rollback().await.unwrap();

    let mut tx = orm
        .begin(UserId::new(administrator).unwrap())
        .await
        .unwrap();
    let missing = read_values(
        &tx.env(),
        &mut tx,
        SettingsScope::Global {},
        vec![LOCK_WAIT_SETTING.into()],
    )
    .await
    .unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::Access);
    assert_eq!(
        missing.message(),
        "Setting account.sequence_lock_wait_ms is unavailable or access is denied."
    );
    let params = tx.env().records("ir.config_parameter").unwrap();
    let invalid = tx
        .savepoint(async |tx| {
            params
                .call::<_, i64>(tx, "set_int", (LOCK_WAIT_SETTING.to_owned(), Some(99_i64)))
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(invalid.kind(), ErrorKind::Validation);
    assert_eq!(
        invalid.message(),
        "account.sequence_lock_wait_ms takes a whole number from 100 to 60000."
    );
    let prior = params
        .call::<_, i64>(
            &mut tx,
            "set_int",
            (LOCK_WAIT_SETTING.to_owned(), Some(2100_i64)),
        )
        .await
        .unwrap();
    assert_eq!(prior, 1800);
    tx.commit().await.unwrap();
    let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
    assert_eq!(
        LockWait::from_settings(&mut tx).await.unwrap().millis(),
        2100
    );

    // Normal file rollback keeps the upgraded schema. Exercise the predecessor's SQL write
    // boundary after a fresh start, using the same scope lock and canonical legacy text.
    let rollback_write = legacy_client.transaction().await.unwrap();
    let old = rollback_write
        .query_one(
            "SELECT v.value_text FROM ir_config_setting_scope s
         JOIN ir_config_setting_value v ON v.scope_id=s.id
         WHERE s.identity='global' AND v.setting_key=$1 FOR UPDATE OF s",
            &[&LOCK_WAIT_SETTING],
        )
        .await
        .unwrap()
        .get::<_, String>(0);
    assert_eq!(old, "1800");
    let changed = rollback_write.execute(
        "UPDATE ir_config_setting_value SET value_text='2200'
         FROM ir_config_setting_scope s WHERE scope_id=s.id AND s.identity='global' AND setting_key=$1",
        &[&LOCK_WAIT_SETTING],
    ).await.unwrap();
    assert_eq!(changed, 1);
    rollback_write.commit().await.unwrap();
    let saved = client
        .query_one(
            "SELECT p.value, v.value_text FROM ir_config_parameter p
         JOIN ir_config_setting_value v ON p.key=v.setting_key
         JOIN ir_config_setting_scope s ON s.id=v.scope_id
         WHERE s.identity='global' AND p.key=$1",
            &[&LOCK_WAIT_SETTING],
        )
        .await
        .unwrap();
    assert_eq!(saved.get::<_, String>(0), "2100");
    assert_eq!(saved.get::<_, String>(1), "2200");
}

/// The account addon's version as its manifest states it: the upgrade leaves exactly that in the module row, so a
/// version bump of the addon does not need an edit here.
fn account_version() -> &'static str {
    include_str!("../../../addons/account/manifest.toml")
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap()
}

async fn assert_store(
    client: &tokio_postgres::Client,
    parameter: &str,
    legacy: &str,
    version: &str,
    audit: &[&str],
) {
    let row = client
        .query_one(
            "SELECT p.value, v.value_text, s.revision, m.latest_version
         FROM ir_config_parameter p
         JOIN ir_config_setting_value v ON v.setting_key=p.key
         JOIN ir_config_setting_scope s ON s.id=v.scope_id AND s.identity='global'
         JOIN ir_module_module m ON m.name='account' WHERE p.key=$1",
            &[&LOCK_WAIT_SETTING],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), parameter);
    assert_eq!(row.get::<_, String>(1), legacy);
    assert_eq!(row.get::<_, String>(2), "7");
    assert_eq!(row.get::<_, String>(3), version);
    let found: Vec<String> = client
        .query(
            "SELECT actor || ' ' || old_value || '>' || new_value || ' @' || revision
         FROM ir_config_setting_audit WHERE setting_key=$1 ORDER BY id",
            &[&LOCK_WAIT_SETTING],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(found, audit);
}

fn migration_error(error: erp_modules::Error) -> Error {
    match error {
        erp_modules::Error::StepFailed { source, .. } => migration_error(*source),
        erp_modules::Error::MigrationFailed { source, .. } => *source,
        erp_modules::Error::Orm(source) => source,
        other => panic!("unexpected migration error: {other:?}"),
    }
}
