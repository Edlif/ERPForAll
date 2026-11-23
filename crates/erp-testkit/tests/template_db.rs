// SPDX-License-Identifier: MIT OR Apache-2.0
//! Integration tests for [`erp_testkit::TemplateDb`]: single initialisation, concurrent cloning
//! without `55006` object-in-use errors, clone isolation, failed-init cleanup, and run-scoped
//! sweeping.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration tests")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use erp_testkit::{
    SweepOptions, SweepReason, TemplateDb, TestkitError, database_exists, database_url_from_env,
    drop_database_url, sweep,
};

fn unique_run_id(label: &str) -> String {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("tpltest-{label}-{}-{n}", std::process::id())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn template_initialises_once_and_clones_in_parallel_with_isolation() {
    let url = database_url_from_env().expect("DATABASE_URL must be set");
    let run_id = unique_run_id("parallel");
    let key = "erp-testkit:parallel-seed:v1";
    let tpl_name = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_id)
        .template_name()
        .unwrap()
        .render();

    let init_calls = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for worker in 0..8usize {
        let url = url.clone();
        let run_id = run_id.clone();
        let init_calls = Arc::clone(&init_calls);
        handles.push(tokio::spawn(async move {
            let db = TemplateDb::builder(key)
                .database_url(&url)
                .run_id(&run_id)
                .clone_or_init(move |pool| async move {
                    init_calls.fetch_add(1, Ordering::SeqCst);
                    let client = pool.get().await.map_err(|e| e.to_string())?;
                    client
                        .batch_execute(
                            "CREATE TABLE seeded_items (id int PRIMARY KEY, label text NOT NULL); \
                             INSERT INTO seeded_items (id, label) VALUES (1, 'from-template');",
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok::<(), String>(())
                })
                .await
                .unwrap();

            // Verify seeded data and pg_trgm extension are present.
            let client = db.client().await.unwrap();
            let label: String = client
                .query_one("SELECT label FROM seeded_items WHERE id = 1", &[])
                .await
                .unwrap()
                .get(0);
            assert_eq!(label, "from-template");
            let has_trgm: bool = client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm')",
                    &[],
                )
                .await
                .unwrap()
                .get(0);
            assert!(has_trgm, "cloned database must inherit pg_trgm");

            // Mutate this clone and verify only this clone sees the mutation.
            client
                .execute(
                    "INSERT INTO seeded_items (id, label) VALUES ($1, $2)",
                    &[&(100 + worker as i32), &format!("worker-{worker}")],
                )
                .await
                .unwrap();
            let count: i64 = client
                .query_one("SELECT count(*) FROM seeded_items", &[])
                .await
                .unwrap()
                .get(0);
            assert_eq!(count, 2, "each clone must be isolated from sibling clones");
            drop(client);
            db.cleanup().await.unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(
        init_calls.load(Ordering::SeqCst),
        1,
        "template initialiser must run exactly once across 8 concurrent callers"
    );
    assert!(
        database_exists(&url, &tpl_name).await.unwrap(),
        "cached template database {tpl_name} should remain for the duration of the run"
    );

    // Sweeping the run id removes the cached template cleanly.
    let report = sweep(
        &url,
        &SweepOptions {
            max_age: Duration::from_secs(24 * 3600),
            run_id: Some(run_id),
            dry_run: false,
        },
    )
    .await
    .unwrap();
    assert!(report.is_clean(), "sweep failures: {:?}", report.failed);
    assert!(
        report
            .dropped
            .iter()
            .any(|(name, reason)| name == &tpl_name && *reason == SweepReason::RunId),
        "expected {tpl_name} to be dropped by RunId; got {:?}",
        report.dropped
    );
    assert!(!database_exists(&url, &tpl_name).await.unwrap());
}

#[tokio::test]
async fn failed_initialisation_drops_incomplete_template_and_recovers() {
    let url = database_url_from_env().expect("DATABASE_URL must be set");
    let run_id = unique_run_id("recover");
    let key = "erp-testkit:recover:v1";
    let tpl_name = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_id)
        .template_name()
        .unwrap()
        .render();

    let err = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_id)
        .clone_or_init(|pool| async move {
            let client = pool.get().await.map_err(|e| e.to_string())?;
            client
                .batch_execute("CREATE TABLE partial_table (id int)")
                .await
                .map_err(|e| e.to_string())?;
            Err::<(), String>("intentional seed failure".into())
        })
        .await
        .expect_err("failing initialiser must return TemplateInit");

    match err {
        TestkitError::TemplateInit {
            key: err_key,
            message,
        } => {
            assert_eq!(err_key, key);
            assert!(message.contains("intentional seed failure"), "{message}");
        }
        other => panic!("expected TemplateInit, got {other:?}"),
    }

    assert!(
        !database_exists(&url, &tpl_name).await.unwrap(),
        "failed initialisation must drop the incomplete template {tpl_name}"
    );

    // A subsequent attempt builds a clean template without partial_table.
    let db = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_id)
        .clone_or_init(|pool| async move {
            let client = pool.get().await.map_err(|e| e.to_string())?;
            client
                .batch_execute("CREATE TABLE clean_table (id int)")
                .await
                .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .await
        .unwrap();

    let client = db.client().await.unwrap();
    let has_partial: bool = client
        .query_one(
            "SELECT to_regclass('public.partial_table') IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let has_clean: bool = client
        .query_one("SELECT to_regclass('public.clean_table') IS NOT NULL", &[])
        .await
        .unwrap()
        .get(0);
    assert!(
        !has_partial,
        "partial table from failed init must not exist"
    );
    assert!(has_clean, "clean table from recovery init must exist");
    drop(client);
    db.cleanup().await.unwrap();

    // Clean up template.
    drop_database_url(&url, &tpl_name).await.unwrap();
}

#[tokio::test]
async fn sweep_keeps_other_runs_templates() {
    let url = database_url_from_env().expect("DATABASE_URL must be set");
    let run_a = unique_run_id("keep-a");
    let run_b = unique_run_id("keep-b");
    let key = "erp-testkit:sweep-isolation:v1";

    let tpl_a = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_a)
        .template_name()
        .unwrap()
        .render();
    let tpl_b = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_b)
        .template_name()
        .unwrap()
        .render();

    let db_a = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_a)
        .clone_or_init(|_| async { Ok::<(), String>(()) })
        .await
        .unwrap();
    let db_b = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_b)
        .clone_or_init(|_| async { Ok::<(), String>(()) })
        .await
        .unwrap();
    db_a.cleanup().await.unwrap();
    db_b.cleanup().await.unwrap();

    // Sweep run_a only: tpl_a drops, tpl_b stays.
    let report = sweep(
        &url,
        &SweepOptions {
            max_age: Duration::from_secs(24 * 3600),
            run_id: Some(run_a),
            dry_run: false,
        },
    )
    .await
    .unwrap();
    assert!(report.is_clean());
    assert!(!database_exists(&url, &tpl_a).await.unwrap());
    assert!(database_exists(&url, &tpl_b).await.unwrap());

    // Sweep run_b to leave the server clean.
    let report_b = sweep(
        &url,
        &SweepOptions {
            max_age: Duration::from_secs(24 * 3600),
            run_id: Some(run_b),
            dry_run: false,
        },
    )
    .await
    .unwrap();
    assert!(report_b.is_clean());
    assert!(!database_exists(&url, &tpl_b).await.unwrap());
}

#[tokio::test]
async fn crashed_ready0_template_is_rebuilt_and_unreadable_comment_is_refused() {
    let url = database_url_from_env().expect("DATABASE_URL must be set");
    let run_id = unique_run_id("crashed");
    let key = "erp-testkit:crashed-recovery:v1";
    let tpl = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_id)
        .template_name()
        .unwrap();
    let tpl_name = tpl.render();

    let (admin, conn) =
        erp_testkit::tokio_postgres::connect(&url, erp_testkit::tokio_postgres::NoTls)
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    // Simulate a crashed builder that left a `ready=0` template behind.
    admin
        .batch_execute(&format!(
            "CREATE DATABASE \"{tpl_name}\" TEMPLATE template0 ENCODING 'UTF8' LC_COLLATE 'C'"
        ))
        .await
        .unwrap();
    admin
        .batch_execute(&format!(
            "COMMENT ON DATABASE \"{tpl_name}\" IS 'erpforall-testkit-tpl v1 pid=1 run_id={run_id} created=1 ready=0'"
        ))
        .await
        .unwrap();

    let db = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_id)
        .clone_or_init(|pool| async move {
            let client = pool.get().await.map_err(|e| e.to_string())?;
            client
                .batch_execute("CREATE TABLE rebuilt (id int)")
                .await
                .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .await
        .unwrap();
    let client = db.client().await.unwrap();
    let has_rebuilt: bool = client
        .query_one("SELECT to_regclass('public.rebuilt') IS NOT NULL", &[])
        .await
        .unwrap()
        .get(0);
    assert!(has_rebuilt, "crashed ready=0 template must be rebuilt");
    drop(client);
    db.cleanup().await.unwrap();

    // Holding the template's advisory lock makes sweep skip it instead of dropping it.
    admin
        .query_one("SELECT pg_advisory_lock($1)", &[&tpl.lock_key()])
        .await
        .unwrap();
    let skipped_report = sweep(
        &url,
        &SweepOptions {
            max_age: Duration::ZERO,
            run_id: Some(run_id.clone()),
            dry_run: false,
        },
    )
    .await
    .unwrap();
    assert!(
        skipped_report.kept.iter().any(|n| n == &tpl_name),
        "sweep must keep an actively locked template; got {skipped_report:?}"
    );
    assert!(database_exists(&url, &tpl_name).await.unwrap());
    admin
        .query_one("SELECT pg_advisory_unlock($1)", &[&tpl.lock_key()])
        .await
        .unwrap();

    // An unreadable comment must be refused (never silently dropped).
    admin
        .batch_execute(&format!(
            "COMMENT ON DATABASE \"{tpl_name}\" IS 'erpforall-testkit-tpl v99 unknown=1';"
        ))
        .await
        .unwrap();
    let err = TemplateDb::builder(key)
        .database_url(&url)
        .run_id(&run_id)
        .clone_or_init(|_| async { Ok::<(), String>(()) })
        .await
        .expect_err("unreadable template comment must fail loudly");
    assert!(
        matches!(err, TestkitError::Configuration(_)),
        "expected Configuration error, got {err:?}"
    );
    assert!(
        database_exists(&url, &tpl_name).await.unwrap(),
        "unreadable template must not be dropped"
    );

    drop_database_url(&url, &tpl_name).await.unwrap();
}
