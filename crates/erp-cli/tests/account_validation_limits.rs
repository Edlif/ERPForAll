// SPDX-License-Identifier: MIT OR Apache-2.0
//! Installed Accounting upgrade for the two moved validation limits.
#![allow(clippy::unwrap_used, clippy::panic, reason = "integration assertions")]

#[path = "support/accounting.rs"]
mod accounting;

use accounting::{big_stack, install, main_company, policy_orm, system, user, xml_id};
use erp_addon_account::journal::settings::{CODE_MAX_LENGTH, JournalSettings};
use erp_addon_base::ir_config_setting::read_values;
use erp_modules::ModuleName;
use erp_sdk::prelude::*;
use erp_sdk::views::wire::SettingsScope;
use erp_sdk::{Context, ErrorKind, SUPERUSER_ID, UserId};

const TAX_LIMIT: &str = "account.tax_repartition_lines_max";

#[test]
fn installed_validation_limits_move_atomically_to_system_parameters() {
    big_stack(upgrade);
}

async fn upgrade() {
    let (db, installed) = install().await;
    let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
    for key in [CODE_MAX_LENGTH, TAX_LIMIT] {
        let unavailable = read_values(
            &tx.env(),
            &mut tx,
            SettingsScope::Global {},
            vec![key.into()],
        )
        .await
        .unwrap_err();
        assert_eq!(unavailable.kind(), ErrorKind::Access);
        assert_eq!(
            unavailable.message(),
            format!("Setting {key} is unavailable or access is denied.")
        );
        let rows = tx
            .query(
                "SELECT value FROM ir_config_parameter WHERE key=$1",
                &[&key],
            )
            .await
            .unwrap();
        let [row] = rows.as_slice() else {
            panic!("expected one installed parameter row for {key}")
        };
        assert_eq!(row.get::<_, String>(0), "16");
    }
    assert_eq!(
        JournalSettings::read(&mut tx)
            .await
            .unwrap()
            .code_max_length,
        16
    );
    assert_eq!(
        erp_sdk::params::get_int(&mut tx, &erp_addon_account::ADDON, TAX_LIMIT)
            .await
            .unwrap(),
        16
    );
    let company = main_company(&mut tx).await;
    let admins = xml_id(&mut tx, "base", "group_system").await;
    let managers = xml_id(&mut tx, "account", "group_account_manager").await;
    let administrator = user(&mut tx, "settings.limits.admin", company, &[admins]).await;
    let manager = user(&mut tx, "settings.limits.manager", company, &[managers]).await;
    tx.commit().await.unwrap();
    drop(tx);

    let client = db.client().await.unwrap();
    // Keep the installed schema and both homes. Recreate the 1.9 catalog for these keys.
    client
        .batch_execute(
            "INSERT INTO ir_config_setting_scope(identity,revision) VALUES ('global','7')
           ON CONFLICT(identity) DO UPDATE SET revision='7';
         INSERT INTO ir_config_setting_value(scope_id,setting_key,value_text)
           SELECT id,'account.journal_code_max_length','6'
           FROM ir_config_setting_scope WHERE identity='global';
         INSERT INTO ir_config_setting_value(scope_id,setting_key,value_text)
           SELECT id,'account.tax_repartition_lines_max','1'
           FROM ir_config_setting_scope WHERE identity='global';
         UPDATE ir_config_parameter SET value='20' WHERE key='account.journal_code_max_length';
         UPDATE ir_config_parameter SET value='18' WHERE key='account.tax_repartition_lines_max';
         UPDATE ir_module_module SET latest_version='1.9.0' WHERE name='account';",
        )
        .await
        .unwrap();
    let lifecycle = system(&db, None);
    let account = [ModuleName::new("account").unwrap()];
    let caller = lifecycle.system_env(Context::new());
    let error = migration_error(
        lifecycle
            .button_immediate_upgrade(&caller, &account)
            .await
            .unwrap_err(),
    );
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "account.tax_repartition_lines_max takes a whole number from 2 to 64."
    );
    assert_stores(&client, "1.9.0", [("20", "6"), ("18", "1")], &[]).await;

    client
        .execute(
            "UPDATE ir_config_setting_value SET value_text='3' WHERE setting_key=$1",
            &[&TAX_LIMIT],
        )
        .await
        .unwrap();
    let upgraded = lifecycle
        .button_immediate_upgrade(&caller, &account)
        .await
        .unwrap();
    assert!(upgraded.post_commit.is_clean());
    let audit = [
        "account.journal_code_max_length@7 20>6",
        "account.tax_repartition_lines_max@7 18>3",
    ];
    assert_stores(&client, "1.11.0", [("6", "6"), ("3", "3")], &audit).await;
    let upgraded = lifecycle
        .button_immediate_upgrade(&caller, &account)
        .await
        .unwrap();
    assert!(upgraded.post_commit.is_clean());
    assert_stores(&client, "1.11.0", [("6", "6"), ("3", "3")], &audit).await;

    let orm = policy_orm(&db, &upgraded.orm).await;
    let mut tx = orm.begin(UserId::new(manager).unwrap()).await.unwrap();
    assert_eq!(
        JournalSettings::read(&mut tx)
            .await
            .unwrap()
            .code_max_length,
        6
    );
    assert_eq!(
        erp_sdk::params::get_int(&mut tx, &erp_addon_account::ADDON, TAX_LIMIT)
            .await
            .unwrap(),
        3
    );
    let params = tx.env().records("ir.config_parameter").unwrap();
    for key in [CODE_MAX_LENGTH, TAX_LIMIT] {
        let denied = params
            .call::<_, i64>(&mut tx, "set_int", (key.to_owned(), Some(10_i64)))
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Access);
    }
    tx.rollback().await.unwrap();
    drop(tx);

    let mut tx = orm
        .begin(UserId::new(administrator).unwrap())
        .await
        .unwrap();
    let params = tx.env().records("ir.config_parameter").unwrap();
    for (key, sample, bounds) in [
        (CODE_MAX_LENGTH, 33_i64, "1 to 32"),
        (TAX_LIMIT, 65_i64, "2 to 64"),
    ] {
        let invalid = tx
            .savepoint(async |tx| {
                params
                    .call::<_, i64>(tx, "set_int", (key.to_owned(), Some(sample)))
                    .await
            })
            .await
            .unwrap_err();
        assert_eq!(invalid.kind(), ErrorKind::Validation);
        assert_eq!(
            invalid.message(),
            format!("{key} takes a whole number from {bounds}.")
        );
    }
    let prior = params
        .call::<_, i64>(
            &mut tx,
            "set_int",
            (CODE_MAX_LENGTH.to_owned(), Some(7_i64)),
        )
        .await
        .unwrap();
    assert_eq!(prior, 6);
    assert_eq!(
        JournalSettings::read(&mut tx)
            .await
            .unwrap()
            .code_max_length,
        7
    );
    tx.rollback().await.unwrap();
    drop(tx);
    assert_stores(&client, "1.11.0", [("6", "6"), ("3", "3")], &audit).await;
}

#[test]
fn direct_1_8_0_account_upgrade_moves_sequence_and_validation_limits() {
    big_stack(direct_upgrade);
}

async fn direct_upgrade() {
    use erp_addon_account::numbering::LockWait;
    let (db, _) = install().await;
    let client = db.client().await.unwrap();
    client
        .batch_execute(
            "INSERT INTO ir_config_setting_scope(identity,revision) VALUES ('global','7')
           ON CONFLICT(identity) DO UPDATE SET revision='7';
         INSERT INTO ir_config_setting_value(scope_id,setting_key,value_text)
            SELECT s.id, chosen.key, chosen.value
              FROM ir_config_setting_scope s
              CROSS JOIN (VALUES ('account.sequence_lock_wait_ms','1650'),
                                 ('account.journal_code_max_length','9'),
                                 ('account.tax_repartition_lines_max','7')) AS chosen(key, value)
             WHERE s.identity='global';
         DELETE FROM ir_config_parameter
            WHERE key IN ('account.sequence_lock_wait_ms', 'account.journal_code_max_length',
                          'account.tax_repartition_lines_max');
         UPDATE ir_module_module SET latest_version='1.8.0' WHERE name='account';",
        )
        .await
        .unwrap();
    let lifecycle = system(&db, None);
    let account = [ModuleName::new("account").unwrap()];
    let upgraded = lifecycle
        .button_immediate_upgrade(&lifecycle.system_env(Context::new()), &account)
        .await
        .unwrap();
    assert!(upgraded.post_commit.is_clean());
    let mut tx = upgraded.orm.begin(SUPERUSER_ID).await.unwrap();
    assert_eq!(
        LockWait::from_settings(&mut tx).await.unwrap().millis(),
        1650
    );
    assert_eq!(
        JournalSettings::read(&mut tx)
            .await
            .unwrap()
            .code_max_length,
        9
    );
    assert_eq!(
        erp_sdk::params::get_int(&mut tx, &erp_addon_account::ADDON, TAX_LIMIT)
            .await
            .unwrap(),
        7,
    );
    tx.rollback().await.unwrap();
}

async fn assert_stores(
    client: &tokio_postgres::Client,
    version: &str,
    stores: [(&str, &str); 2],
    audit: &[&str],
) {
    for (key, (expected_parameter, expected_legacy)) in
        [CODE_MAX_LENGTH, TAX_LIMIT].into_iter().zip(stores)
    {
        let row = client
            .query_one(
                "SELECT p.value, v.value_text, s.revision, m.latest_version
             FROM ir_config_parameter p
             JOIN ir_config_setting_value v ON v.setting_key=p.key
             JOIN ir_config_setting_scope s ON s.id=v.scope_id AND s.identity='global'
             JOIN ir_module_module m ON m.name='account' WHERE p.key=$1",
                &[&key],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, String>(0), expected_parameter, "{key}");
        assert_eq!(row.get::<_, String>(1), expected_legacy, "{key}");
        assert_eq!(row.get::<_, String>(2), "7");
        assert_eq!(row.get::<_, String>(3), version);
    }
    let rows: Vec<String> = client.query(
        "SELECT setting_key || '@' || revision || ' ' || old_value || '>' || new_value
           FROM ir_config_setting_audit
          WHERE actor='migration:account@1.10' AND (setting_key=$1 OR setting_key=$2) ORDER BY setting_key",
        &[&CODE_MAX_LENGTH, &TAX_LIMIT],
    ).await.unwrap().into_iter().map(|row| row.get(0)).collect();
    assert_eq!(rows, audit);
}

fn migration_error(error: erp_modules::Error) -> Error {
    match error {
        erp_modules::Error::StepFailed { source, .. } => migration_error(*source),
        erp_modules::Error::MigrationFailed { source, .. } => *source,
        erp_modules::Error::Orm(source) => source,
        other => panic!("unexpected migration error: {other:?}"),
    }
}
