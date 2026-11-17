// SPDX-License-Identifier: MIT OR Apache-2.0
//! `ondelete = "restrict"` of a company-dependent many2one under a concurrent write (
//! supplement of 2026-11-15).
//!
//! Two transactions on two connections: one stores a reference to a target in a holder's company
//! map, the other deletes the target. Whatever the order, a target named by a committed entry is
//! never deleted: one of the two fails with a serialization error and its retry sees the other's
//! work. Both run at `REPEATABLE READ`: the record API refuses a `READ COMMITTED` transaction.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use erp_fields::{Error, ErrorKind, Value};
use erp_orm::{Env, SUPERUSER_ID, Transaction};
use erp_test_models::base_users;
use erp_testkit::Harness;
use serde_json::json;

mod fixture {
    use erp_sdk::prelude::*;
    erp_sdk::addon!(name = "property_lock", depends = ["base"]);
    #[model(name = "plock.tag", addon = "property_lock")]
    pub struct Tag {
        #[field(char)]
        pub name: Option<Arc<str>>,
    }
    #[model(name = "plock.holder", addon = "property_lock")]
    pub struct Holder {
        #[field(char)]
        pub name: Option<Arc<str>>,
        #[field(
            many2one,
            comodel = "plock.tag",
            company_dependent = true,
            ondelete = "restrict"
        )]
        pub restrict_id: Option<RecId>,
    }
}

const REFUSAL: &str = "You cannot delete a record used by a company-dependent field";
/// How long a test waits for the other transaction to block on a row lock.
const BLOCK: Duration = Duration::from_secs(20);

async fn harness() -> Harness {
    Harness::builder([&base_users::ADDON, &erp_addon_base::ADDON, &fixture::ADDON])
        .build()
        .await
        .unwrap()
}
fn company(env: &Env) -> Env {
    env.with_context([("allowed_company_ids", Value::list([Value::Int(1)]))])
}
fn vals(items: &[(&str, Value)]) -> std::collections::BTreeMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}
fn programming(error: impl std::fmt::Display) -> Error {
    Error::programming(error.to_string())
}

/// A committed target and a committed holder without a reference.
struct World {
    tag: i64,
    holder: i64,
}
async fn world(h: &Harness) -> World {
    let mut tx = h.begin().await.unwrap();
    let currency = erp_testkit::fixtures::seed_currency_in(&mut tx)
        .await
        .unwrap();
    tx.execute(
        "INSERT INTO res_company (id, name, currency_id) VALUES (1, 'One', $1) ON CONFLICT DO NOTHING",
        &[&currency],
    )
    .await
    .unwrap();
    let env = company(&tx.env());
    let tag = new_tag(&mut tx, &env, "t").await;
    let holder = new_holder(&mut tx, None).await.unwrap();
    tx.commit().await.unwrap();
    World { tag, holder }
}
async fn new_tag(tx: &mut Transaction, env: &Env, name: &str) -> i64 {
    env.records("plock.tag")
        .unwrap()
        .create_values(tx, &[vals(&[("name", Value::str(name))])])
        .await
        .unwrap()
        .ids()[0]
}
/// Create a holder through the ORM, with a reference to `tag` when given, and flush.
async fn new_holder(tx: &mut Transaction, tag: Option<i64>) -> Result<i64, Error> {
    let env = company(&tx.env());
    let mut values = vec![("name", Value::str("h"))];
    if let Some(tag) = tag {
        values.push(("restrict_id", Value::Int(tag)));
    }
    let created = env
        .records("plock.holder")?
        .create_values(tx, &[vals(&values)])
        .await?;
    tx.flush().await?;
    Ok(created.ids()[0])
}
/// Store `value` in `holder`'s `restrict_id` for company 1 through the ORM, and flush.
async fn assign(tx: &mut Transaction, holder: i64, value: Value) -> Result<(), Error> {
    let env = company(&tx.env());
    env.records("plock.holder")?
        .browse([holder])?
        .write_values(tx, &vals(&[("restrict_id", value)]))
        .await?;
    tx.flush().await
}
async fn refer(tx: &mut Transaction, holder: i64, tag: i64) -> Result<(), Error> {
    assign(tx, holder, Value::Int(tag)).await
}
async fn delete(tx: &mut Transaction, tag: i64) -> Result<(), Error> {
    let env = company(&tx.env());
    env.records("plock.tag")?.browse([tag])?.unlink(tx).await
}
async fn tag_exists(h: &Harness, tag: i64) -> bool {
    let mut tx = h.begin().await.unwrap();
    let found = !tx
        .query("SELECT 1 FROM plock_tag WHERE id = $1", &[&tag])
        .await
        .unwrap()
        .is_empty();
    tx.rollback().await.unwrap();
    found
}
async fn map_of(h: &Harness, holder: i64) -> Option<serde_json::Value> {
    let mut tx = h.begin().await.unwrap();
    let map = tx
        .query(
            "SELECT restrict_id FROM plock_holder WHERE id = $1",
            &[&holder],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.rollback().await.unwrap();
    map
}
/// The guard and link rows kept for `tag`.
async fn traces(h: &Harness, tag: i64) -> (i64, i64) {
    let mut tx = h.begin().await.unwrap();
    let guards: i64 = tx
        .query(
            "SELECT count(*) FROM erp_company_ref_guard WHERE comodel = 'plock.tag' AND target_id = $1",
            &[&tag],
        )
        .await
        .unwrap()[0]
        .get(0);
    let links: i64 = tx
        .query(
            "SELECT count(*) FROM erp_company_ref_link WHERE comodel = 'plock.tag' AND target_id = $1",
            &[&tag],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.rollback().await.unwrap();
    (guards, links)
}
fn assert_serialization(error: &Error) {
    assert_eq!(error.kind(), ErrorKind::Concurrency, "{error:?}");
    assert_eq!(error.sqlstate(), Some("40001"), "{error:?}");
}
fn assert_refusal(error: &Error) {
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    assert_eq!(error.message(), REFUSAL);
}

/// What the other transaction commits after the deleter took its snapshot.
#[derive(Clone, Copy)]
enum Late {
    /// The first reference the target ever had (its guard row is new).
    FirstReference,
    /// A reference from another holder; the target had one before, since removed.
    OtherHolder,
    /// A holder that names another target is pointed at this one (its link row is rewritten).
    Retargeted,
    /// A new holder created with the reference.
    CreatedHolder,
}

/// A deletion whose `REPEATABLE READ` snapshot is older than a committed reference fails with a
/// serialization error; the retried unit of work sees the reference and is refused.
async fn late_reference_case(late: Late) {
    let h = harness().await;
    let w = world(&h).await;
    if matches!(late, Late::OtherHolder | Late::Retargeted) {
        // the target was referenced before: its guard row stays, the link row went with the entry
        h.run(async |tx| refer(tx, w.holder, w.tag).await)
            .await
            .unwrap();
        h.run(async |tx| assign(tx, w.holder, Value::False).await)
            .await
            .unwrap();
        assert_eq!(map_of(&h, w.holder).await, None);
        assert_eq!(traces(&h, w.tag).await, (1, 0));
    }
    if matches!(late, Late::Retargeted) {
        h.run(async |tx| {
            let env = company(&tx.env());
            let spare = new_tag(tx, &env, "spare").await;
            refer(tx, w.holder, spare).await
        })
        .await
        .unwrap();
    }
    let orm = h.orm().clone();
    let attempts = AtomicUsize::new(0);
    let referrer: Mutex<Option<i64>> = Mutex::new(None);
    let refused = h
        .run(async |tx| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            tx.query("SELECT 1", &[]).await?; // takes the snapshot
            if attempt > 1 {
                return Ok(delete(tx, w.tag).await.unwrap_err());
            }
            let mut other = orm.begin(SUPERUSER_ID).await?;
            let holder = match late {
                Late::FirstReference | Late::Retargeted => {
                    refer(&mut other, w.holder, w.tag).await?;
                    w.holder
                }
                Late::OtherHolder => {
                    let holder = new_holder(&mut other, None).await?;
                    refer(&mut other, holder, w.tag).await?;
                    holder
                }
                Late::CreatedHolder => new_holder(&mut other, Some(w.tag)).await?,
            };
            other.commit().await?;
            *referrer.lock().unwrap() = Some(holder);
            let error = delete(tx, w.tag).await.unwrap_err();
            assert_serialization(&error);
            Err(error)
        })
        .await
        .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "40001, then a retry");
    assert_refusal(&refused);
    assert!(tag_exists(&h, w.tag).await);
    let holder = referrer.lock().unwrap().unwrap();
    assert_eq!(map_of(&h, holder).await, Some(json!({"1": w.tag})));
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn first_reference_committed_after_the_deleters_snapshot_stops_the_deletion() {
    late_reference_case(Late::FirstReference).await;
}

#[tokio::test]
async fn later_reference_from_another_holder_stops_the_deletion() {
    late_reference_case(Late::OtherHolder).await;
}

#[tokio::test]
async fn entry_pointed_at_the_target_after_the_deleters_snapshot_stops_the_deletion() {
    late_reference_case(Late::Retargeted).await;
}

#[tokio::test]
async fn target_created_after_the_writers_snapshot_is_still_guarded() {
    let h = harness().await;
    let w = world(&h).await;
    let mut writer = h.begin().await.unwrap();
    erp_testkit::backend_pid(&mut writer).await.unwrap(); // takes the writer's snapshot
    let fresh = h
        .run(async |tx| {
            let env = company(&tx.env());
            Ok(new_tag(tx, &env, "fresh").await)
        })
        .await
        .unwrap();
    let writer = Mutex::new(Some(writer));
    let attempts = AtomicUsize::new(0);
    let refused = h
        .run(async |tx| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            tx.query("SELECT 1", &[]).await?; // takes the snapshot: it shows the new target
            if attempt > 1 {
                return Ok(delete(tx, fresh).await.unwrap_err());
            }
            // the writer's snapshot does not show the target it names
            let mut writer = writer.lock().unwrap().take().unwrap();
            refer(&mut writer, w.holder, fresh).await?;
            writer.commit().await?;
            let error = delete(tx, fresh).await.unwrap_err();
            assert_serialization(&error);
            Err(error)
        })
        .await
        .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "40001, then a retry");
    assert_refusal(&refused);
    assert!(tag_exists(&h, fresh).await);
    assert_eq!(map_of(&h, w.holder).await, Some(json!({"1": fresh})));
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn holder_created_with_the_reference_stops_the_deletion() {
    late_reference_case(Late::CreatedHolder).await;
}

#[tokio::test]
async fn deleter_waits_for_a_writer_in_flight_and_is_refused_on_retry() {
    let h = harness().await;
    let w = world(&h).await;
    let mut writer = h.begin().await.unwrap();
    refer(&mut writer, w.holder, w.tag).await.unwrap();
    let writer = Mutex::new(Some(writer));
    let attempts = AtomicUsize::new(0);
    let refused = h
        .run(async |tx| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt > 1 {
                return Ok(delete(tx, w.tag).await.unwrap_err());
            }
            let pid = erp_testkit::backend_pid(tx).await.map_err(programming)?;
            let mut writer = writer.lock().unwrap().take().unwrap();
            let (ours, committed) = tokio::join!(delete(tx, w.tag), async {
                erp_testkit::wait_until_blocked(&h, pid, BLOCK)
                    .await
                    .map_err(programming)?;
                writer.commit().await
            });
            committed?;
            let error = ours.unwrap_err();
            assert_serialization(&error);
            Err(error)
        })
        .await
        .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "40001, then a retry");
    assert_refusal(&refused);
    assert!(tag_exists(&h, w.tag).await);
    assert_eq!(map_of(&h, w.holder).await, Some(json!({"1": w.tag})));
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn writer_waits_for_a_deletion_in_flight_and_fails_when_it_commits() {
    let h = harness().await;
    let w = world(&h).await;
    let mut deleter = h.begin().await.unwrap();
    delete(&mut deleter, w.tag).await.unwrap();
    let mut writer = h.begin().await.unwrap();
    let pid = erp_testkit::backend_pid(&mut writer).await.unwrap();
    let (ours, committed) = tokio::join!(refer(&mut writer, w.holder, w.tag), async {
        erp_testkit::wait_until_blocked(&h, pid, BLOCK)
            .await
            .unwrap();
        deleter.commit().await
    });
    committed.unwrap();
    assert_serialization(&ours.unwrap_err());
    drop(writer);
    assert!(!tag_exists(&h, w.tag).await);
    assert_eq!(map_of(&h, w.holder).await, None);
    assert_eq!(traces(&h, w.tag).await, (0, 0));
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn writer_continues_when_the_deletion_rolls_back() {
    let h = harness().await;
    let w = world(&h).await;
    let mut deleter = h.begin().await.unwrap();
    delete(&mut deleter, w.tag).await.unwrap();
    let mut writer = h.begin().await.unwrap();
    let pid = erp_testkit::backend_pid(&mut writer).await.unwrap();
    let (ours, rolled_back) = tokio::join!(refer(&mut writer, w.holder, w.tag), async {
        erp_testkit::wait_until_blocked(&h, pid, BLOCK)
            .await
            .unwrap();
        deleter.rollback().await
    });
    rolled_back.unwrap();
    ours.unwrap();
    writer.commit().await.unwrap();
    assert_eq!(map_of(&h, w.holder).await, Some(json!({"1": w.tag})));
    assert_eq!(traces(&h, w.tag).await, (1, 1));
    let mut again = h.begin().await.unwrap();
    assert_refusal(&delete(&mut again, w.tag).await.unwrap_err());
    again.rollback().await.unwrap();
    assert!(tag_exists(&h, w.tag).await);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn second_writer_of_a_first_reference_waits_and_fails_once() {
    let h = harness().await;
    let w = world(&h).await;
    let other = h.run(async |tx| new_holder(tx, None).await).await.unwrap();
    let mut first = h.begin().await.unwrap();
    refer(&mut first, w.holder, w.tag).await.unwrap();
    let first = Mutex::new(Some(first));
    let attempts = AtomicUsize::new(0);
    h.run(async |tx| {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt > 1 {
            return refer(tx, other, w.tag).await;
        }
        let pid = erp_testkit::backend_pid(tx).await.map_err(programming)?;
        let mut first = first.lock().unwrap().take().unwrap();
        let (ours, committed) = tokio::join!(refer(tx, other, w.tag), async {
            erp_testkit::wait_until_blocked(&h, pid, BLOCK)
                .await
                .map_err(programming)?;
            first.commit().await
        });
        committed?;
        let error = ours.unwrap_err();
        assert_serialization(&error);
        Err(error)
    })
    .await
    .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "40001, then a retry");
    assert_eq!(map_of(&h, w.holder).await, Some(json!({"1": w.tag})));
    assert_eq!(map_of(&h, other).await, Some(json!({"1": w.tag})));
    assert_eq!(traces(&h, w.tag).await, (1, 2));
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn one_transaction_that_references_and_deletes_the_target() {
    let h = harness().await;
    let w = world(&h).await;
    let mut tx = h.begin().await.unwrap();
    refer(&mut tx, w.holder, w.tag).await.unwrap();
    assert_refusal(&delete(&mut tx, w.tag).await.unwrap_err());
    assign(&mut tx, w.holder, Value::False).await.unwrap();
    delete(&mut tx, w.tag).await.unwrap();
    tx.commit().await.unwrap();
    assert!(!tag_exists(&h, w.tag).await);
    assert_eq!(map_of(&h, w.holder).await, None);
    assert_eq!(traces(&h, w.tag).await, (0, 0));
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn link_rows_go_with_their_entry_their_holder_and_their_target() {
    let h = harness().await;
    let w = world(&h).await;
    h.run(async |tx| refer(tx, w.holder, w.tag).await)
        .await
        .unwrap();
    assert_eq!(traces(&h, w.tag).await, (1, 1));
    let mut tx = h.begin().await.unwrap();
    assert_refusal(&delete(&mut tx, w.tag).await.unwrap_err());
    tx.rollback().await.unwrap();
    assert_eq!(traces(&h, w.tag).await, (1, 1), "a refusal keeps the rows");

    // the entry is removed: its link row goes, the guard row stays
    h.run(async |tx| assign(tx, w.holder, Value::False).await)
        .await
        .unwrap();
    assert_eq!(traces(&h, w.tag).await, (1, 0));

    // the holder is deleted: its link row goes
    h.run(async |tx| refer(tx, w.holder, w.tag).await)
        .await
        .unwrap();
    assert_eq!(traces(&h, w.tag).await, (1, 1));
    h.run(async |tx| {
        let env = company(&tx.env());
        env.records("plock.holder")?
            .browse([w.holder])?
            .unlink(tx)
            .await
    })
    .await
    .unwrap();
    assert_eq!(traces(&h, w.tag).await, (1, 0));

    // the target is deleted: nothing is left
    h.run(async |tx| delete(tx, w.tag).await).await.unwrap();
    assert!(!tag_exists(&h, w.tag).await);
    assert_eq!(traces(&h, w.tag).await, (0, 0));
    h.cleanup().await.unwrap();
}

/// The statements of `tx` recorded so far that touch the guard table.
fn guard_statements(tx: &Transaction) -> Vec<String> {
    tx.captured()
        .iter()
        .filter(|sql| sql.contains("erp_company_ref_guard"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_flush_with_several_guard_statements_locks_every_target_first() {
    let h = harness().await;
    let w = world(&h).await;
    let mut tx = h.begin().await.unwrap();
    let env = company(&tx.env());
    let other_tag = new_tag(&mut tx, &env, "u").await;
    let other_holder = new_holder(&mut tx, None).await.unwrap();
    let holders = env.records("plock.holder").unwrap();
    tx.capture_statements(true);
    // Two sets of written fields, so two UPDATE statements with a guard statement each.
    holders
        .browse([w.holder])
        .unwrap()
        .write_values(&mut tx, &vals(&[("restrict_id", Value::Int(other_tag))]))
        .await
        .unwrap();
    holders
        .browse([other_holder])
        .unwrap()
        .write_values(
            &mut tx,
            &vals(&[
                ("name", Value::str("renamed")),
                ("restrict_id", Value::Int(w.tag)),
            ]),
        )
        .await
        .unwrap();
    tx.flush().await.unwrap();
    let guards = guard_statements(&tx);
    tx.capture_statements(false);
    assert_eq!(guards.len(), 3, "{guards:#?}");
    // the lock statement: both targets at once, no link row yet
    assert!(guards[0].contains("FOR KEY SHARE"), "{}", guards[0]);
    assert!(!guards[0].contains("erp_company_ref_link"), "{}", guards[0]);
    for writer in &guards[1..] {
        assert!(writer.contains("erp_company_ref_link"), "{writer}");
    }

    tx.capture_statements(true);
    refer(&mut tx, w.holder, w.tag).await.unwrap();
    let guards = guard_statements(&tx);
    tx.capture_statements(false);
    assert_eq!(guards.len(), 1, "{guards:#?}");
    assert!(guards[0].contains("erp_company_ref_link"), "{}", guards[0]);
    tx.commit().await.unwrap();
    assert_eq!(
        map_of(&h, w.holder).await,
        Some(json!({"1": w.tag})),
        "the last write of the holder is the stored reference"
    );
    assert_eq!(map_of(&h, other_holder).await, Some(json!({"1": w.tag})));
    h.cleanup().await.unwrap();
}
