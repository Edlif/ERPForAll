// SPDX-License-Identifier: MIT OR Apache-2.0
//! Genuine previous-binary company-calendar upgrade. Requires the declared baseline on a build host.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "explicit upgrade rehearsal assertions"
)]
#[path = "../tests/support/runtime_addons.rs"]
mod runtime_addons;
#[path = "../tests/support/company_upgrade.rs"]
mod support;
use erp_testkit::TestDb;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[tokio::main]
async fn main() {
    let old =
        PathBuf::from(std::env::var_os("ERP_CALENDAR_OLD_SOURCE").expect("old source required"));
    let old_bin =
        PathBuf::from(std::env::var_os("ERP_CALENDAR_OLD_BIN").expect("old binary required"));
    let old_revision = fs::read_to_string(old.join("REVISION")).unwrap();
    assert_eq!(
        old_revision.trim(),
        std::env::var("ERP_CALENDAR_OLD_REVISION").unwrap()
    );
    assert!(
        fs::read_to_string(old.join("addons/base/manifest.toml"))
            .unwrap()
            .contains("version = \"1.15\"")
    );
    eprintln!(
        "previous_source={} previous_binary_sha256={}",
        old_revision.trim(),
        erp_modules::package::sha256_hex(&fs::read(&old_bin).unwrap())
    );
    let db = TestDb::create().await.unwrap();
    let client = db.client().await.unwrap();
    let role = format!("{}_calendar", db.name());
    client.batch_execute(&format!("CREATE ROLE {} LOGIN PASSWORD 'fixture-runtime-password' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION", erp_sql::quote(&role))).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().canonicalize().unwrap();
    let old_addons = directory.join("old-addons");
    runtime_addons::stage_runtime_addons(&old.join("addons"), &old_addons).unwrap();
    let config = support::config(&directory, &db, &role, &old_addons);
    support::private(
        &directory.join("password"),
        "fixture-administrator-password",
    );
    support::succeeded(
        support::run(
            &old_bin,
            &[
                "db",
                "init",
                "--config",
                config.to_str().unwrap(),
                "--admin-login",
                "calendar-upgrade-admin",
                "--admin-password-file",
                directory.join("password").to_str().unwrap(),
            ],
            &db,
        ),
        "previous-db-init",
    );
    let missing_zone = async || {
        client.query_one("SELECT NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='public' AND table_name='res_company' AND column_name='tz')", &[]).await.unwrap().get::<_, bool>(0)
    };
    assert!(
        missing_zone().await,
        "baseline must genuinely predate company zones"
    );
    let original = client.query_one("SELECT latest_version, content_hash FROM ir_module_module m JOIN installed_addon_identity i USING(name) WHERE m.name='base'", &[]).await.unwrap();
    assert_eq!(original.get::<_, String>(0), "1.15.0");
    let before: Vec<(i64, String)> = client
        .query("SELECT id,name FROM res_company ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert!(!before.is_empty());
    let previous = directory.join("previous");
    fs::create_dir(&previous).unwrap();
    runtime_addons::stage_runtime_addons(&old_addons, &previous.join("addons")).unwrap();
    support::release_inventory(&previous, old_revision.trim(), Some(&client)).await;
    let candidate = directory.join("candidate");
    fs::create_dir(&candidate).unwrap();
    let current = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    runtime_addons::stage_runtime_addons(&current.join("addons"), &candidate.join("addons"))
        .unwrap();
    let source = fs::read_to_string(current.join("REVISION")).unwrap();
    support::release_inventory(&candidate, source.trim(), None).await;
    let current_binary =
        PathBuf::from(std::env::var_os("ERP_CALENDAR_NEW_BIN").expect("current binary required"));
    let binary = current_binary.as_path();
    let upgrade = || {
        support::run(
            binary,
            &[
                "module",
                "upgrade",
                "base",
                "-d",
                db.name(),
                "--addons-path",
                candidate.join("addons").to_str().unwrap(),
                "--identity-previous",
                previous.to_str().unwrap(),
            ],
            &db,
        )
    };
    client.batch_execute("CREATE FUNCTION fixture_calendar_late_refusal() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.name='base' AND NEW.latest_version IS DISTINCT FROM OLD.latest_version THEN IF EXISTS (SELECT 1 FROM res_company WHERE tz IS DISTINCT FROM 'UTC') THEN RAISE EXCEPTION 'fixture UTC backfill missing'; END IF; RAISE EXCEPTION 'fixture late calendar refusal' USING ERRCODE='23514'; END IF; RETURN NEW; END $$; CREATE TRIGGER fixture_calendar_late_refusal BEFORE UPDATE OF latest_version ON ir_module_module FOR EACH ROW EXECUTE FUNCTION fixture_calendar_late_refusal()").await.unwrap();
    let refused = upgrade();
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        std::str::from_utf8(&refused.stderr).unwrap().trim(),
        "Error: The database refused the change: fixture late calendar refusal: db error: ERROR: fixture late calendar refusal"
    );
    assert!(
        missing_zone().await,
        "late failure must roll back the zone column"
    );
    let unchanged = client.query_one("SELECT latest_version, content_hash FROM ir_module_module m JOIN installed_addon_identity i USING(name) WHERE m.name='base'", &[]).await.unwrap();
    assert_eq!(unchanged.get::<_, String>(0), original.get::<_, String>(0));
    assert_eq!(unchanged.get::<_, String>(1), original.get::<_, String>(1));
    client.batch_execute("DROP TRIGGER fixture_calendar_late_refusal ON ir_module_module; DROP FUNCTION fixture_calendar_late_refusal()").await.unwrap();
    support::succeeded(upgrade(), "calendar-upgrade");
    let after: Vec<(i64, String)> = client
        .query("SELECT id,name FROM res_company ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(before, after);
    let zones: Vec<String> = client
        .query("SELECT tz FROM res_company ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(zones, vec!["UTC"; before.len()]);
    assert_eq!(client.query_one("SELECT is_nullable FROM information_schema.columns WHERE table_schema='public' AND table_name='res_company' AND column_name='tz'", &[]).await.unwrap().get::<_, String>(0), "NO");
    client
        .execute(
            "UPDATE res_company SET tz='Asia/Jakarta' WHERE id=$1",
            &[&before[0].0],
        )
        .await
        .unwrap();
    support::succeeded(upgrade(), "calendar-repeat");
    assert_eq!(
        client
            .query_one("SELECT tz FROM res_company WHERE id=$1", &[&before[0].0])
            .await
            .unwrap()
            .get::<_, String>(0),
        "Asia/Jakarta"
    );
    drop(client);
    db.cleanup().await.unwrap();
    let cluster = TestDb::create().await.unwrap();
    cluster
        .client()
        .await
        .unwrap()
        .batch_execute(&format!("DROP ROLE {}", erp_sql::quote(&role)))
        .await
        .unwrap();
    cluster.cleanup().await.unwrap();
    println!("COMPANY-CALENDAR-UPGRADE-PASSED");
}
