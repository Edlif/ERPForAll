// SPDX-License-Identifier: MIT OR Apache-2.0
//! A create with a company-dependent many2one costs one statement once the transaction has
//! prepared the model's fallbacks, and the prepared fallbacks are never served
//! after the rows they were read from changed (supplement of 2026-11-15).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use erp_fields::{Cell, Error, ErrorKind, Value};
use erp_orm::{Env, Operation, Records, Transaction, ir_default};
use erp_test_models::base_users;
use erp_testkit::{DomainPolicy, Harness, Otherwise};

mod fixture {
    use erp_sdk::prelude::*;
    erp_sdk::addon!(name = "property_create_queries", depends = ["base"]);
    #[model(name = "pcq.eggs", addon = "property_create_queries")]
    pub struct Eggs {
        #[field(char)]
        pub name: Option<Arc<str>>,
        #[field(many2one, comodel = "pcq.eggs", ondelete = "cascade")]
        pub parent_id: Option<RecId>,
    }
    #[model(name = "pcq.bacon", addon = "property_create_queries")]
    pub struct Bacon {
        #[field(char)]
        pub name: Option<Arc<str>>,
        #[field(many2one, comodel = "pcq.eggs", company_dependent = true)]
        pub eggs_id: Option<RecId>,
    }
    /// A holder whose record rule reads the eggs model through a plain many2one.
    #[model(name = "pcq.ham", addon = "property_create_queries")]
    pub struct Ham {
        #[field(char)]
        pub name: Option<Arc<str>>,
        #[field(many2one, comodel = "pcq.eggs")]
        pub plain_id: Option<RecId>,
        #[field(many2one, comodel = "pcq.eggs", company_dependent = true)]
        pub eggs_id: Option<RecId>,
    }
}

const EGGS: &str = "pcq.eggs";
const BACON: &str = "pcq.bacon";
const HAM: &str = "pcq.ham";
const FIELD: &str = "eggs_id";

async fn harness() -> Harness {
    Harness::builder([&base_users::ADDON, &erp_addon_base::ADDON, &fixture::ADDON])
        .build()
        .await
        .unwrap()
}
/// A harness whose read rule on ham reads the name of the eggs its plain many2one points to.
async fn ruled_harness() -> Harness {
    let visible =
        erp_sdk::Domain::condition("plain_id.name", erp_sdk::Operator::Ne, "hidden").unwrap();
    let policy = DomainPolicy::builder(Otherwise::Allow).rule(HAM, Operation::Read, visible);
    Harness::builder([&base_users::ADDON, &erp_addon_base::ADDON, &fixture::ADDON])
        .policy(policy.build().unwrap())
        .build()
        .await
        .unwrap()
}
fn company(env: &Env, id: i64) -> Env {
    env.with_context([("allowed_company_ids", Value::list([Value::Int(id)]))])
}
fn vals(items: &[(&str, Value)]) -> std::collections::BTreeMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}
async fn seed(tx: &mut Transaction) {
    let currency = erp_testkit::fixtures::seed_currency_in(tx).await.unwrap();
    tx.execute("INSERT INTO res_company (id, name, currency_id) VALUES (1, 'One', $1), (2, 'Two', $1) ON CONFLICT DO NOTHING", &[&currency]).await.unwrap();
}
async fn eggs(tx: &mut Transaction, env: &Env, name: &str, parent: Option<i64>) -> i64 {
    let mut values = vec![("name", Value::str(name))];
    if let Some(parent) = parent {
        values.push(("parent_id", Value::Int(parent)));
    }
    env.records(EGGS)
        .unwrap()
        .create_values(tx, &[vals(&values)])
        .await
        .unwrap()
        .ids()[0]
}
async fn bacon(tx: &mut Transaction, env: &Env, items: &[(&str, Value)]) -> Records {
    env.records(BACON)
        .unwrap()
        .create_values(tx, &[vals(items)])
        .await
        .unwrap()
}
/// The eggs a bacon row with no company entry reads in `env`: the fallback this transaction
/// prepared. The row is written straight to its table, so that no creation default is involved
/// (a create copies the creation default into the row, whatever became of its target).
async fn inherited(tx: &mut Transaction, env: &Env) -> Cell {
    let rows = tx
        .query(
            "INSERT INTO pcq_bacon (name) VALUES ('no entry') RETURNING id::int8",
            &[],
        )
        .await
        .unwrap();
    let id: i64 = rows[0].get(0);
    let record = env.records(BACON).unwrap().browse([id]).unwrap();
    record.cell(tx, FIELD).await.unwrap()
}
/// Set the eggs default, for every company or for one.
async fn set_default(tx: &mut Transaction, env: &Env, eggs: i64, company: Option<i64>) {
    ir_default::set(
        tx,
        env,
        BACON,
        FIELD,
        &Value::Int(eggs),
        None,
        company.map(|id| erp_fields::RecId::from_real(id).unwrap()),
        None,
    )
    .await
    .unwrap();
}
/// One create of a bacon plus the flush: the statement count and the statement texts.
async fn counted_create(
    tx: &mut Transaction,
    env: &Env,
    items: &[(&str, Value)],
) -> (u64, Vec<String>) {
    let start = tx.query_count();
    tx.capture_statements(true);
    bacon(tx, env, items).await;
    tx.flush().await.unwrap();
    let statements = tx.captured().to_vec();
    tx.capture_statements(false);
    (tx.query_count() - start, statements)
}
/// The create sent one statement, the `INSERT` into the bacon table.
fn assert_one_insert(label: &str, (count, statements): &(u64, Vec<String>)) {
    assert_eq!(
        (*count, statements.len()),
        (1, 1),
        "{label}: statements sent: {statements:#?}"
    );
    assert!(
        statements[0].contains("INSERT INTO \"pcq_bacon\""),
        "{label}: not the INSERT: {}",
        statements[0]
    );
}

/// After one create that warms the creation defaults of this user and company (the case's
/// "caches warmed by one create"), the next create sends one statement, the `INSERT`.
async fn assert_warmed_create_is_one_insert(tx: &mut Transaction, env: &Env, label: &str) {
    bacon(tx, env, &[]).await;
    assert_one_insert(label, &counted_create(tx, env, &[]).await);
}

/// A create scenario: its name, the environment it runs in and the values it passes.
type Scenario<'a> = (&'a str, Env, Vec<(&'a str, Value)>);

#[tokio::test]
async fn create_with_a_company_dependent_many2one_costs_one_statement() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let default = eggs(&mut tx, &env, "Eggs", None).await;
    let other = eggs(&mut tx, &env, "Other eggs", None).await;
    for phase in ["no default", "default set to a record"] {
        if phase == "default set to a record" {
            set_default(&mut tx, &env, default, None).await;
        }
        // one create warms the caches
        counted_create(&mut tx, &env, &[]).await;
        let with_default = |value: Value| env.with_context([("default_eggs_id".to_owned(), value)]);
        let scenarios: [Scenario<'_>; 5] = [
            ("create({})", env.clone(), vec![]),
            ("context default false", with_default(Value::False), vec![]),
            (
                "context default id",
                with_default(Value::Int(other)),
                vec![],
            ),
            ("value false", env.clone(), vec![(FIELD, Value::False)]),
            ("value id", env.clone(), vec![(FIELD, Value::Int(other))]),
        ];
        for (label, scoped, items) in scenarios {
            let outcome = counted_create(&mut tx, &scoped, &items).await;
            assert_one_insert(&format!("{phase} / {label}"), &outcome);
        }
    }
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_default_changed_by_the_helper_api_replaces_the_prepared_fallback() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let first = eggs(&mut tx, &env, "First", None).await;
    let second = eggs(&mut tx, &env, "Second", None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);

    set_default(&mut tx, &env, first, None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(first));
    assert_warmed_create_is_one_insert(&mut tx, &env, "after set").await;

    set_default(&mut tx, &env, second, None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(second));
    assert_warmed_create_is_one_insert(&mut tx, &env, "after replace").await;

    ir_default::discard_values(&mut tx, &env, BACON, FIELD, &[Value::Int(second)])
        .await
        .unwrap();
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);
    assert_warmed_create_is_one_insert(&mut tx, &env, "after discard").await;
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_default_changed_by_the_record_api_replaces_the_prepared_fallback() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let first = eggs(&mut tx, &env, "First", None).await;
    let second = eggs(&mut tx, &env, "Second", None).await;
    set_default(&mut tx, &env, first, None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(first));

    let rows = tx
        .query(
            "SELECT d.id FROM ir_default d JOIN ir_model_fields f ON f.id = d.field_id \
             WHERE f.model = 'pcq.bacon' AND f.name = 'eggs_id'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let id: i64 = rows[0].get(0);
    let row = env.records("ir.default").unwrap().browse([id]).unwrap();
    // a pending write: nothing is flushed by the test before the next create
    row.write_values(
        &mut tx,
        &vals(&[("json_value", Value::str(second.to_string()))]),
    )
    .await
    .unwrap();
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(second));

    row.unlink(&mut tx).await.unwrap();
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn raw_statements_that_name_their_table_end_the_prepared_fallback() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let fallback = eggs(&mut tx, &env, "Fallback", None).await;
    let second = eggs(&mut tx, &env, "Second", None).await;
    set_default(&mut tx, &env, fallback, None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(fallback));
    tx.flush().await.unwrap();

    // the target disappears outside the ORM; the default row stays
    assert_eq!(
        tx.execute("DELETE FROM pcq_eggs WHERE id = $1", &[&fallback])
            .await
            .unwrap(),
        1
    );
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);

    assert_eq!(
        tx.execute(
            "INSERT INTO pcq_eggs (id, name) VALUES ($1, 'Fallback again')",
            &[&fallback],
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(fallback));

    assert_eq!(
        tx.execute(
            "UPDATE ir_default SET json_value = $1 WHERE field_id IN \
             (SELECT id FROM ir_model_fields WHERE model = 'pcq.bacon' AND name = 'eggs_id')",
            &[&second.to_string()],
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(second));
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn raw_statements_of_unknown_effect_end_the_prepared_fallback() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let fallback = eggs(&mut tx, &env, "Fallback", None).await;
    set_default(&mut tx, &env, fallback, None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(fallback));
    tx.flush().await.unwrap();

    // CASCADE: the ham table holds a foreign key to eggs
    tx.execute("TRUNCATE pcq_eggs CASCADE", &[]).await.unwrap();
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);

    tx.execute(
        "INSERT INTO pcq_eggs (id, name) VALUES ($1, 'Fallback again')",
        &[&fallback],
    )
    .await
    .unwrap();
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(fallback));

    tx.batch(&format!(
        "DELETE FROM pcq_bacon WHERE false; DELETE FROM pcq_eggs WHERE id = {fallback}"
    ))
    .await
    .unwrap();
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);
    drop(tx);
    h.cleanup().await.unwrap();
}

/// The documented limit (supplement of 2026-11-15): a raw data-changing `WITH` is not
/// followed, so the prepared fallback is served until the caller invalidates the target model or
/// everything; after that the dangling rule holds again.
#[tokio::test]
async fn a_raw_data_changing_with_is_the_callers_to_invalidate() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let fallback = eggs(&mut tx, &env, "Fallback", None).await;
    set_default(&mut tx, &env, fallback, None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(fallback));
    tx.flush().await.unwrap();

    let gone = tx
        .query(
            "WITH gone AS (DELETE FROM pcq_eggs WHERE id = $1 RETURNING id) \
             SELECT count(*)::int8 FROM gone",
            &[&fallback],
        )
        .await
        .unwrap();
    assert_eq!(gone[0].get::<_, i64>(0), 1);
    // not followed: the entry prepared before the statement is still served
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(fallback));
    tx.invalidate_model(EGGS, None).await.unwrap();
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);

    let back = tx
        .query(
            "WITH back AS (INSERT INTO pcq_eggs (id, name) VALUES ($1, 'Fallback again') \
             RETURNING id) SELECT count(*)::int8 FROM back",
            &[&fallback],
        )
        .await
        .unwrap();
    assert_eq!(back[0].get::<_, i64>(0), 1);
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);
    tx.invalidate_all().await.unwrap();
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(fallback));
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_target_deleted_by_cascade_ends_the_prepared_fallback() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let parent = eggs(&mut tx, &env, "Parent", None).await;
    let child = eggs(&mut tx, &env, "Child", Some(parent)).await;
    set_default(&mut tx, &env, child, None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(child));

    env.records(EGGS)
        .unwrap()
        .browse([parent])
        .unwrap()
        .unlink(&mut tx)
        .await
        .unwrap();
    let left = tx
        .query(
            "SELECT count(*)::int8 FROM pcq_eggs WHERE id = $1",
            &[&child],
        )
        .await
        .unwrap();
    assert_eq!(left[0].get::<_, i64>(0), 0, "the cascade deleted the child");
    assert_eq!(inherited(&mut tx, &env).await, Cell::Null);
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_rolled_back_default_is_not_served_from_the_prepared_fallback() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let kept = eggs(&mut tx, &env, "Kept", None).await;
    let temporary = eggs(&mut tx, &env, "Temporary", None).await;
    set_default(&mut tx, &env, kept, None).await;
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(kept));
    tx.flush().await.unwrap();

    let result: erp_fields::Result<()> = tx
        .savepoint(async |tx| {
            set_default(tx, &env, temporary, None).await;
            assert_eq!(inherited(tx, &env).await, Cell::Id(temporary));
            Err(Error::validation("rollback the default"))
        })
        .await;
    let error = result.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(error.message(), "rollback the default");
    assert_eq!(inherited(&mut tx, &env).await, Cell::Id(kept));
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn each_company_keeps_its_own_prepared_fallback() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let one = company(&tx.env(), 1);
    let two = company(&tx.env(), 2);
    let shared = eggs(&mut tx, &one, "Shared", None).await;
    let own = eggs(&mut tx, &one, "Company two", None).await;
    set_default(&mut tx, &one, shared, None).await;
    set_default(&mut tx, &one, own, Some(2)).await;

    assert_eq!(inherited(&mut tx, &one).await, Cell::Id(shared));
    assert_eq!(inherited(&mut tx, &two).await, Cell::Id(own));
    assert_eq!(inherited(&mut tx, &one).await, Cell::Id(shared));
    assert_warmed_create_is_one_insert(&mut tx, &one, "company one").await;
    assert_warmed_create_is_one_insert(&mut tx, &two, "company two").await;
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn another_transactions_default_stays_out_of_this_snapshot() {
    let h = harness().await;
    let mut setup = h.begin().await.unwrap();
    seed(&mut setup).await;
    let env = company(&setup.env(), 1);
    let before = eggs(&mut setup, &env, "Before", None).await;
    let after = eggs(&mut setup, &env, "After", None).await;
    set_default(&mut setup, &env, before, None).await;
    setup.commit().await.unwrap();

    let mut reader = h.begin().await.unwrap();
    let reader_env = company(&reader.env(), 1);
    assert_eq!(inherited(&mut reader, &reader_env).await, Cell::Id(before));

    let mut writer = h.begin().await.unwrap();
    let writer_env = company(&writer.env(), 1);
    set_default(&mut writer, &writer_env, after, None).await;
    writer.commit().await.unwrap();

    assert_eq!(inherited(&mut reader, &reader_env).await, Cell::Id(before));
    reader.rollback().await.unwrap();

    let mut later = h.begin().await.unwrap();
    let later_env = company(&later.env(), 1);
    assert_eq!(inherited(&mut later, &later_env).await, Cell::Id(after));
    drop(later);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_restricted_read_that_flushes_the_target_model_still_reads_the_fallback() {
    let h = ruled_harness().await;
    let mut setup = h.begin().await.unwrap();
    seed(&mut setup).await;
    // a user without elevation (the system principal is always elevated)
    setup
        .execute(
            "INSERT INTO res_users (id, name, login, active) VALUES (7, 'plain', 'plain', true) ON CONFLICT DO NOTHING",
            &[],
        )
        .await
        .unwrap();
    let env = company(&setup.env(), 1);
    let fallback = eggs(&mut setup, &env, "Fallback", None).await;
    ir_default::set(
        &mut setup,
        &env,
        HAM,
        FIELD,
        &Value::Int(fallback),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let rows = setup
        .query(
            "INSERT INTO pcq_ham (name, plain_id) VALUES ('first', $1), ('second', $1) \
             RETURNING id::int8",
            &[&fallback],
        )
        .await
        .unwrap();
    let (first, second): (i64, i64) = (rows[0].get(0), rows[1].get(0));
    setup.commit().await.unwrap();

    let mut tx = h.begin_as(7).await.unwrap();
    let user = company(&tx.env(), 1);
    let ham = user.records(HAM).unwrap();
    assert_eq!(
        ham.browse([first])
            .unwrap()
            .cell(&mut tx, FIELD)
            .await
            .unwrap(),
        Cell::Id(fallback)
    );
    // a pending write of the model the rule reads: the next restricted read flushes it
    user.records(EGGS)
        .unwrap()
        .browse([fallback])
        .unwrap()
        .write_values(&mut tx, &vals(&[("name", Value::str("Renamed"))]))
        .await
        .unwrap();
    tx.capture_statements(true);
    let read = ham.browse([second]).unwrap().cell(&mut tx, FIELD).await;
    let statements = tx.captured().to_vec();
    tx.capture_statements(false);
    // the premise of this test: the read itself sent the write of the target table
    assert!(
        statements
            .iter()
            .any(|statement| statement.contains("UPDATE \"pcq_eggs\"")),
        "the restricted read did not flush the pending write of eggs: {statements:#?}"
    );
    assert_eq!(read.unwrap(), Cell::Id(fallback));
    drop(tx);
    h.cleanup().await.unwrap();
}
