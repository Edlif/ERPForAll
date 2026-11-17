// SPDX-License-Identifier: MIT OR Apache-2.0
//! Acceptance of company-dependent fields on representative models: a product-like
//! exact cost, a partner-like relation, company isolation for a user, concurrent writers with a
//! retry, and rollback. Task.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod support;

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use erp_fields::{Cell, Decimal, Error, ErrorKind, Value};
use erp_orm::{
    BeginOptions, Env, Orm, ReadGroup, Records, RecordsAs, RetryPolicy, RunOptions, SUPERUSER_ID,
    SearchOptions, Transaction, ir_default,
};
use erp_sdk::{Domain, Operator};
use erp_test_models::base_users;
use erp_testkit::Harness;
use serde_json::json;

mod fixture {
    use erp_sdk::prelude::*;
    erp_sdk::addon!(name = "property_acceptance", depends = ["base"]);
    #[model(name = "acceptance.term", addon = "property_acceptance")]
    pub struct Term {
        #[field(char)]
        pub name: Option<Arc<str>>,
    }
    #[model(name = "acceptance.product", addon = "property_acceptance")]
    pub struct Product {
        #[field(char)]
        pub name: Option<Arc<str>>,
        #[field(float, digits = (0, 3), company_dependent = true)]
        pub cost: Decimal,
    }
    #[model(name = "acceptance.partner", addon = "property_acceptance")]
    pub struct Partner {
        #[field(char)]
        pub name: Option<Arc<str>>,
        #[field(many2one, comodel = "acceptance.term", company_dependent = true)]
        pub term_id: Option<RecId>,
        #[field(
            many2one,
            comodel = "acceptance.term",
            company_dependent = true,
            ondelete = "restrict"
        )]
        pub locked_term_id: Option<RecId>,
    }
    /// The fields, each with a variant that is not company-dependent.
    #[model(name = "acceptance.company", addon = "property_acceptance")]
    pub struct Company {
        #[field(char, company_dependent = true)]
        pub foo: Option<Arc<str>>,
        #[field(date, company_dependent = true)]
        pub date: Option<Date>,
        #[field(datetime, company_dependent = true)]
        pub moment: Option<DateTime>,
        #[field(many2one, comodel = "acceptance.term", company_dependent = true)]
        pub tag_id: Option<RecId>,
        #[field(char)]
        pub plain_foo: Option<Arc<str>>,
        #[field(date)]
        pub plain_date: Option<Date>,
        #[field(datetime)]
        pub plain_moment: Option<DateTime>,
        #[field(many2one, comodel = "acceptance.term")]
        pub plain_tag_id: Option<RecId>,
    }
    /// The model: one company-dependent many2one.
    #[model(name = "acceptance.bacon", addon = "property_acceptance")]
    pub struct Bacon {
        #[field(many2one, comodel = "acceptance.term", company_dependent = true)]
        pub eggs_id: Option<RecId>,
    }
}

const PRODUCT: &str = "acceptance.product";
const PARTNER: &str = "acceptance.partner";
const TERM: &str = "acceptance.term";
/// Users of the `base_users` fixture model, seeded by [`seed`] (the RPC harness seeds the same
/// ids itself).
const ALICE: i64 = support::rpc::ALICE;
const BOB: i64 = support::rpc::BOB;
const COMPANIES: [i64; 3] = [1, 2, 3];
const RESTRICT_REFUSAL: &str = "You cannot delete a record used by a company-dependent field";
const UNAUTHORIZED_COMPANY: &str =
    "One of the selected companies does not exist or is not one of yours.";
const SERIALIZATION_FAILURE: &str = "could not serialize access due to concurrent update";

async fn harness() -> Harness {
    Harness::builder([&base_users::ADDON, &erp_addon_base::ADDON, &fixture::ADDON])
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
fn dec(text: &str) -> Decimal {
    Decimal::from_str_exact(text).unwrap()
}
/// The text a company map holds for an exact value.
fn stored(value: Decimal) -> String {
    value.normalize().to_string()
}
fn cost(value: Decimal) -> std::collections::BTreeMap<String, Value> {
    vals(&[("cost", Value::Decimal(value))])
}
fn error(error: &Error, kind: ErrorKind, message: &str) {
    assert_eq!(error.kind(), kind);
    assert_eq!(error.message(), message);
}
async fn seed_companies(tx: &mut Transaction) {
    let currency = erp_testkit::fixtures::seed_currency_in(tx).await.unwrap();
    tx.execute("INSERT INTO res_company (id, name, currency_id) VALUES (1, 'One', $1), (2, 'Two', $1), (3, 'Three', $1) ON CONFLICT DO NOTHING", &[&currency]).await.unwrap();
}
/// Three companies and two users without elevation.
async fn seed(tx: &mut Transaction) {
    seed_companies(tx).await;
    tx.execute(
        "INSERT INTO res_users (id, login, name, active, share) VALUES ($1, 'alice', 'Alice', true, false), ($2, 'bob', 'Bob', true, false) ON CONFLICT DO NOTHING",
        &[&ALICE, &BOB],
    )
    .await
    .unwrap();
}
async fn create(tx: &mut Transaction, env: &Env, model: &str, items: &[(&str, Value)]) -> Records {
    env.records(model)
        .unwrap()
        .create_values(tx, &[vals(items)])
        .await
        .unwrap()
}
async fn named(tx: &mut Transaction, env: &Env, model: &str, name: &str) -> Records {
    create(tx, env, model, &[("name", Value::str(name))]).await
}
/// The stored company map of a column, `None` for SQL NULL.
async fn raw(
    tx: &mut Transaction,
    table: &str,
    column: &str,
    id: i64,
) -> Option<serde_json::Value> {
    let sql = format!("SELECT {column} FROM {table} WHERE id = $1");
    tx.query(&sql, &[&id]).await.unwrap()[0].get(0)
}
async fn raw_cost(tx: &mut Transaction, id: i64) -> Option<serde_json::Value> {
    raw(tx, "acceptance_product", "cost", id).await
}
fn product(env: &Env, cid: i64, id: i64) -> Records {
    company(env, cid)
        .records(PRODUCT)
        .unwrap()
        .browse([id])
        .unwrap()
}
async fn set_cost_default(tx: &mut Transaction, env: &Env, value: Decimal, company: Option<i64>) {
    ir_default::set(
        tx,
        env,
        PRODUCT,
        "cost",
        &Value::Decimal(value),
        None,
        company.map(|id| erp_fields::RecId::from_real(id).unwrap()),
        None,
    )
    .await
    .unwrap();
}
fn sum(values: &[Decimal]) -> Decimal {
    values
        .iter()
        .try_fold(Decimal::ZERO, |total, value| total.checked_add(*value))
        .unwrap()
}

#[tokio::test]
async fn product_cost_is_exact_per_company_through_reads_fallbacks_and_queries() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = tx.env();
    // Neighbours beyond 2^53: as f64 they would collapse onto the same or a wrong value.
    let a_one = dec("9007199254740993.125");
    let a_two = dec("9007199254740994.25");
    let b_one = dec("3.5");
    let global = dec("12.125");
    let two_default = dec("9007199254740995.5");
    let threshold = dec("9007199254740993.5");

    let one = company(&env, 1);
    let a = create(
        &mut tx,
        &one,
        PRODUCT,
        &[("name", Value::str("A")), ("cost", Value::Decimal(a_one))],
    )
    .await;
    let b = create(
        &mut tx,
        &one,
        PRODUCT,
        &[("name", Value::str("B")), ("cost", Value::Decimal(b_one))],
    )
    .await;
    let c = named(&mut tx, &one, PRODUCT, "C").await;
    let ids = [a.ids()[0], b.ids()[0], c.ids()[0]];
    product(&env, 2, ids[0])
        .write_values(&mut tx, &cost(a_two))
        .await
        .unwrap();
    set_cost_default(&mut tx, &one, global, None).await;
    set_cost_default(&mut tx, &one, two_default, Some(2)).await;

    // The effective cost of [A, B, C] in each company, from the inputs and the fallback rule:
    // the record's own value, else the company's default, else the global default.
    let effective: [[Decimal; 3]; 3] = [
        [a_one, b_one, global],
        [a_two, two_default, two_default],
        [global; 3],
    ];

    for phase in ["pending", "flushed"] {
        if phase == "flushed" {
            tx.flush().await.unwrap();
        }
        for (cid, want) in COMPANIES.into_iter().zip(effective) {
            let scoped = company(&env, cid);
            for (id, want) in ids.iter().zip(want) {
                assert_eq!(
                    product(&env, cid, *id).cell(&mut tx, "cost").await.unwrap(),
                    Cell::Decimal(want),
                    "{phase}: record {id} in company {cid}"
                );
            }
            let typed = scoped
                .model::<fixture::Product>()
                .unwrap()
                .browse([ids[0]])
                .unwrap();
            assert_eq!(
                typed.get(&mut tx, fixture::Product::COST).await.unwrap(),
                want[0],
                "{phase}: typed read in company {cid}"
            );
        }
    }
    // Company 3 has no entry: only the two written companies are stored, as exact text.
    assert_eq!(
        raw_cost(&mut tx, ids[0]).await,
        Some(json!({"1": stored(a_one), "2": stored(a_two)}))
    );

    let scope = Domain::condition(
        "id",
        Operator::In,
        erp_domain::Value::list(ids.iter().copied()),
    )
    .unwrap();
    for (cid, want) in COMPANIES.into_iter().zip(effective) {
        let model = company(&env, cid).records(PRODUCT).unwrap();

        let above =
            Domain::condition("cost", Operator::Gt, threshold.to_string().as_str()).unwrap();
        let found = model
            .search_with(
                &mut tx,
                &(scope.clone() & above),
                &SearchOptions::new().order("id"),
            )
            .await
            .unwrap();
        let expected: Vec<i64> = ids
            .iter()
            .zip(want)
            .filter(|(_, value)| *value > threshold)
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(found.ids(), expected, "cost > threshold in company {cid}");

        let ordered = model
            .search_with(&mut tx, &scope, &SearchOptions::new().order("cost, id"))
            .await
            .unwrap();
        let mut expected: Vec<(Decimal, i64)> = want.into_iter().zip(ids).collect();
        expected.sort();
        let expected: Vec<i64> = expected.into_iter().map(|(_, id)| id).collect();
        assert_eq!(ordered.ids(), expected, "order by cost in company {cid}");

        let total = model
            ._read_group(
                &mut tx,
                ReadGroup::new(scope.clone()).aggregates(["cost:sum"]),
            )
            .await
            .unwrap();
        assert_eq!(
            total.rows()[0][0].to_value(RecordsAs::Id),
            Value::Decimal(sum(&want)),
            "sum of costs in company {cid}"
        );
    }

    // Writing a company's fallback value removes that company's override, nothing else.
    let a_in_two = product(&env, 2, ids[0]);
    a_in_two
        .write_values(&mut tx, &cost(two_default))
        .await
        .unwrap();
    tx.flush().await.unwrap();
    assert_eq!(
        raw_cost(&mut tx, ids[0]).await,
        Some(json!({"1": stored(a_one)}))
    );
    assert_eq!(
        a_in_two.cell(&mut tx, "cost").await.unwrap(),
        Cell::Decimal(two_default)
    );
    assert_eq!(
        product(&env, 1, ids[0])
            .cell(&mut tx, "cost")
            .await
            .unwrap(),
        Cell::Decimal(a_one)
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

/// The `(term, count)` groups of the partners `scope` in one company, ordered by term.
async fn term_groups(tx: &mut Transaction, env: &Env, scope: &Domain) -> Vec<(Value, Value)> {
    let groups = env
        .records(PARTNER)
        .unwrap()
        ._read_group(
            tx,
            ReadGroup::new(scope.clone())
                .groupby(["term_id"])
                .aggregates(["__count"]),
        )
        .await
        .unwrap();
    let mut got: Vec<(Value, Value)> = groups
        .rows()
        .iter()
        .map(|row| {
            (
                row[0].to_value(RecordsAs::Id),
                row[1].to_value(RecordsAs::Id),
            )
        })
        .collect();
    got.sort_by_key(|(key, _)| format!("{key:?}"));
    got
}
fn groups(expected: &[(Option<i64>, i64)]) -> Vec<(Value, Value)> {
    let mut want: Vec<(Value, Value)> = expected
        .iter()
        .map(|(term, count)| (term.map_or(Value::False, Value::Int), Value::Int(*count)))
        .collect();
    want.sort_by_key(|(key, _)| format!("{key:?}"));
    want
}

#[tokio::test]
async fn partner_terms_differ_per_company_and_follow_deletion_rules() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = tx.env();
    let one = company(&env, 1);
    let net10 = named(&mut tx, &one, TERM, "Net 10").await.ids()[0];
    let net30 = named(&mut tx, &one, TERM, "Net 30").await.ids()[0];
    let net60 = named(&mut tx, &one, TERM, "Net 60").await.ids()[0];
    let locked = named(&mut tx, &one, TERM, "Locked").await.ids()[0];
    let p = named(&mut tx, &one, PARTNER, "P").await.ids()[0];
    let q = named(&mut tx, &one, PARTNER, "Q").await.ids()[0];
    let partner = |cid: i64, id: i64| {
        company(&env, cid)
            .records(PARTNER)
            .unwrap()
            .browse([id])
            .unwrap()
    };
    for (cid, term) in [(1, net10), (2, net30), (3, net60)] {
        partner(cid, p)
            .write_values(&mut tx, &vals(&[("term_id", Value::Int(term))]))
            .await
            .unwrap();
    }
    partner(1, q)
        .write_values(&mut tx, &vals(&[("term_id", Value::Int(net30))]))
        .await
        .unwrap();
    // The locked term is referenced for company 2 only.
    partner(2, p)
        .write_values(&mut tx, &vals(&[("locked_term_id", Value::Int(locked))]))
        .await
        .unwrap();

    for (cid, term) in [(1, net10), (2, net30), (3, net60)] {
        assert_eq!(
            partner(cid, p).cell(&mut tx, "term_id").await.unwrap(),
            Cell::Id(term),
            "P in company {cid}"
        );
    }
    let scope = Domain::condition("id", Operator::In, erp_domain::Value::list([p, q])).unwrap();
    let net30_by_name = Domain::condition("term_id.name", Operator::Eq, "Net 30").unwrap();
    for (cid, expected) in [(1, vec![q]), (2, vec![p]), (3, vec![])] {
        let found = company(&env, cid)
            .records(PARTNER)
            .unwrap()
            .search(&mut tx, &(scope.clone() & net30_by_name.clone()))
            .await
            .unwrap();
        assert_eq!(found.ids(), expected, "term_id.name in company {cid}");
    }
    for (cid, expected) in [
        (1, [(Some(net10), 1), (Some(net30), 1)]),
        (2, [(Some(net30), 1), (None, 1)]),
        (3, [(Some(net60), 1), (None, 1)]),
    ] {
        assert_eq!(
            term_groups(&mut tx, &company(&env, cid), &scope).await,
            groups(&expected),
            "groups by term_id in company {cid}"
        );
    }
    tx.commit().await.unwrap();

    // A user without elevation, working in company 1.
    let mut tx = h.begin_as(ALICE).await.unwrap();
    let user = tx.env();
    let alice = company(&user, 1);
    assert!(!alice.su());
    let terms = alice.records(TERM).unwrap();
    // The reference that refuses the deletion belongs to company 2, not to the user's company.
    error(
        &terms
            .browse([locked])
            .unwrap()
            .unlink(&mut tx)
            .await
            .unwrap_err(),
        ErrorKind::UserError,
        RESTRICT_REFUSAL,
    );
    assert_eq!(
        terms
            .search(
                &mut tx,
                &Domain::condition("id", Operator::Eq, locked).unwrap()
            )
            .await
            .unwrap()
            .ids(),
        [locked]
    );
    assert_eq!(
        raw(&mut tx, "acceptance_partner", "locked_term_id", p).await,
        Some(json!({"2": locked}))
    );

    // Deleting a set-null target clears its entries only: P keeps companies 2 and 3.
    terms
        .browse([net10])
        .unwrap()
        .unlink(&mut tx)
        .await
        .unwrap();
    assert_eq!(
        raw(&mut tx, "acceptance_partner", "term_id", p).await,
        Some(json!({"1": null, "2": net30, "3": net60}))
    );
    assert_eq!(
        raw(&mut tx, "acceptance_partner", "term_id", q).await,
        Some(json!({"1": net30}))
    );
    let own = |cid: i64| {
        company(&user, cid)
            .records(PARTNER)
            .unwrap()
            .browse([p])
            .unwrap()
    };
    assert_eq!(own(1).cell(&mut tx, "term_id").await.unwrap(), Cell::Null);
    assert_eq!(
        own(2).cell(&mut tx, "term_id").await.unwrap(),
        Cell::Id(net30)
    );

    // A target removed outside the ORM leaves the stored reference; it reads empty.
    tx.flush().await.unwrap();
    tx.execute("DELETE FROM acceptance_term WHERE id = $1", &[&net60])
        .await
        .unwrap();
    tx.invalidate_all().await.unwrap();
    assert_eq!(
        raw(&mut tx, "acceptance_partner", "term_id", p).await,
        Some(json!({"1": null, "2": net30, "3": net60}))
    );
    assert_eq!(own(3).cell(&mut tx, "term_id").await.unwrap(), Cell::Null);
    assert_eq!(
        own(3).value(&mut tx, "term_id").await.unwrap(),
        Value::False
    );
    assert_eq!(
        own(2).cell(&mut tx, "term_id").await.unwrap(),
        Cell::Id(net30)
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

fn alice_in(companies: &[i64]) -> erp_orm::Principal {
    support::rpc::principal_with(ALICE, companies, 1)
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_user_reads_only_allowed_companies_and_rpc_values_are_scalars() {
    let h = support::rpc::harness_with_policy(
        &[&erp_addon_base::ADDON, &fixture::ADDON],
        erp_testkit::RulesPolicy::new(support::rpc::FixturePolicy).unwrap(),
        erp_rpc::RpcConfig::default(),
        false,
    )
    .await;
    let costs = [
        (1, dec("9007199254740993.125")),
        (2, dec("9007199254740994.25")),
        (3, dec("9007199254740995.375")),
    ];
    let mut tx = h.harness().begin().await.unwrap();
    seed_companies(&mut tx).await;
    let env = tx.env();
    let one = company(&env, 1);
    let item = named(&mut tx, &one, PRODUCT, "Item").await.ids()[0];
    let holder = named(&mut tx, &one, PARTNER, "Holder").await.ids()[0];
    let mut terms = Vec::new();
    for (cid, value) in costs {
        product(&env, cid, item)
            .write_values(&mut tx, &cost(value))
            .await
            .unwrap();
        let term = named(&mut tx, &one, TERM, &format!("Term {cid}"))
            .await
            .ids()[0];
        company(&env, cid)
            .records(PARTNER)
            .unwrap()
            .browse([holder])
            .unwrap()
            .write_values(&mut tx, &vals(&[("term_id", Value::Int(term))]))
            .await
            .unwrap();
        terms.push((cid, term));
    }
    tx.commit().await.unwrap();

    // Alice may use companies 1 and 2 (`principal_with` resolves exactly those two).
    let mut reader = h
        .harness()
        .orm()
        .begin_opts(BeginOptions::for_principal(alice_in(&[1, 2])))
        .await
        .unwrap();
    let user = reader.env();
    assert!(!user.su());
    for ((cid, value), (_, term)) in costs.into_iter().zip(terms.iter().copied()).take(2) {
        assert_eq!(
            product(&user, cid, item)
                .cell(&mut reader, "cost")
                .await
                .unwrap(),
            Cell::Decimal(value),
            "cost in company {cid}"
        );
        let partner = company(&user, cid)
            .records(PARTNER)
            .unwrap()
            .browse([holder])
            .unwrap();
        assert_eq!(
            partner.cell(&mut reader, "term_id").await.unwrap(),
            Cell::Id(term),
            "term in company {cid}"
        );
    }
    error(
        &product(&user, 3, item)
            .cell(&mut reader, "cost")
            .await
            .unwrap_err(),
        ErrorKind::Access,
        UNAUTHORIZED_COMPANY,
    );
    drop(reader);

    // The same values over JSON-2: one scalar per field, the company's own.
    for ((cid, value), (_, term)) in costs.into_iter().zip(terms.iter().copied()).take(2) {
        let caller = || erp_rpc::Caller::Principal(alice_in(&[cid]));
        let body = json!({"ids": [item], "specification": {"cost": {}}}).to_string();
        let response =
            support::rpc::call(&h, "/json/2/acceptance.product/web_read", &body, caller()).await;
        let read = support::rpc::ok(&response);
        assert!(
            read[0]["cost"].is_number(),
            "company {cid}: cost is a scalar, got {}",
            read[0]["cost"]
        );
        // A JSON number loses the digits when parsed as f64: the exact text is checked on the
        // bytes of the answer.
        let text = std::str::from_utf8(response.body()).unwrap();
        let exact = format!("\"cost\":{}", stored(value));
        assert!(
            text.contains(&exact),
            "company {cid}: {exact} not in {text}"
        );

        let body = json!({"domain": [["id", "=", item]], "fields": ["cost"]}).to_string();
        let response = support::rpc::call(
            &h,
            "/json/2/acceptance.product/search_read",
            &body,
            caller(),
        )
        .await;
        let found = support::rpc::ok(&response);
        assert!(found[0]["cost"].is_number(), "company {cid}: search_read");
        let text = std::str::from_utf8(response.body()).unwrap();
        assert!(
            text.contains(&exact),
            "company {cid}: {exact} not in {text}"
        );

        let body = json!({"ids": [holder], "fields": ["term_id"], "load": ""}).to_string();
        let response =
            support::rpc::call(&h, "/json/2/acceptance.partner/read", &body, caller()).await;
        assert_eq!(
            support::rpc::ok(&response),
            json!([{"id": holder, "term_id": term}]),
            "company {cid}: term over JSON-2"
        );
    }
    let body = json!({
        "ids": [item],
        "specification": {"cost": {}},
        "context": {"allowed_company_ids": [3]}
    })
    .to_string();
    let refused = support::rpc::call(
        &h,
        "/json/2/acceptance.product/web_read",
        &body,
        erp_rpc::Caller::Principal(alice_in(&[1])),
    )
    .await;
    support::rpc::assert_error(
        &refused,
        403,
        "erp.exceptions.AccessError",
        UNAUTHORIZED_COMPANY,
    );
    h.cleanup().await.unwrap();
}

fn peer(h: &Harness) -> Orm {
    h.orm().rebuild(h.registry().clone()).build().unwrap()
}
/// Create products with no cost and commit.
async fn committed_products(h: &Harness, names: &[&str]) -> Vec<i64> {
    let mut setup = h.begin().await.unwrap();
    seed(&mut setup).await;
    let env = company(&setup.env(), 1);
    let mut ids = Vec::new();
    for name in names {
        ids.push(named(&mut setup, &env, PRODUCT, name).await.ids()[0]);
    }
    setup.commit().await.unwrap();
    ids
}
fn serialization_failure(stale: &Error) {
    assert_eq!(stale.kind(), ErrorKind::Concurrency);
    assert_eq!(stale.sqlstate(), Some("40001"));
    assert_eq!(stale.message(), SERIALIZATION_FAILURE);
}
/// One complete writer transaction: read the cost (which fixes the snapshot), then write it.
/// The commit is the caller's.
async fn read_then_write(tx: &mut Transaction, cid: i64, id: i64, value: Decimal) {
    let record = product(&tx.env(), cid, id);
    record.cell(tx, "cost").await.unwrap();
    record.write_values(tx, &cost(value)).await.unwrap();
}

// behaviour (concurrent writers get the ordinary serialization failure and retry their
// complete transaction); no spec case covers it, so the tests below carry no SPEC tag.
#[tokio::test]
async fn writers_of_different_rows_in_different_companies_do_not_conflict() {
    let h = harness().await;
    let peer = peer(&h);
    let ids = committed_products(&h, &["Left", "Right"]).await;
    let (left, right) = (dec("9007199254740993.125"), dec("9007199254740994.25"));
    let mut one = h.begin().await.unwrap();
    let mut two = peer.begin(SUPERUSER_ID).await.unwrap();
    // Both snapshots exist before either write; both writes are sent before either commit.
    let a = product(&one.env(), 1, ids[0]);
    let b = product(&two.env(), 2, ids[1]);
    a.cell(&mut one, "cost").await.unwrap();
    b.cell(&mut two, "cost").await.unwrap();
    a.write_values(&mut one, &cost(left)).await.unwrap();
    b.write_values(&mut two, &cost(right)).await.unwrap();
    one.flush().await.unwrap();
    two.flush().await.unwrap();
    one.commit().await.unwrap();
    two.commit().await.unwrap();

    let mut check = h.begin().await.unwrap();
    assert_eq!(
        raw_cost(&mut check, ids[0]).await,
        Some(json!({"1": stored(left)}))
    );
    assert_eq!(
        raw_cost(&mut check, ids[1]).await,
        Some(json!({"2": stored(right)}))
    );
    drop((check, one, two, peer));
    h.cleanup().await.unwrap();
}

// no spec case (see above).
#[tokio::test]
async fn one_row_written_for_two_companies_fails_once_and_the_retry_keeps_both() {
    let h = harness().await;
    let peer = peer(&h);
    let id = committed_products(&h, &["Shared"]).await[0];
    let (first, second) = (dec("9007199254740993.125"), dec("9007199254740994.25"));
    let mut one = h.begin().await.unwrap();
    let mut two = peer.begin(SUPERUSER_ID).await.unwrap();
    // 1. the second writer reads: its snapshot predates the first writer's commit
    let stale = product(&two.env(), 2, id);
    assert_eq!(raw_cost(&mut two, id).await, None);
    stale.cell(&mut two, "cost").await.unwrap();
    // 2. the first writer writes company 1 and commits
    read_then_write(&mut one, 1, id, first).await;
    one.commit().await.unwrap();
    // 3. the second writer writes company 2 on its old snapshot: the commit is refused
    stale.write_values(&mut two, &cost(second)).await.unwrap();
    serialization_failure(&two.commit().await.unwrap_err());
    two.rollback().await.unwrap();
    // 4. the complete transaction again: read, write, commit
    read_then_write(&mut two, 2, id, second).await;
    two.commit().await.unwrap();

    let mut check = h.begin().await.unwrap();
    assert_eq!(
        raw_cost(&mut check, id).await,
        Some(json!({"1": stored(first), "2": stored(second)}))
    );
    for (cid, value) in [(1, first), (2, second)] {
        assert_eq!(
            product(&check.env(), cid, id)
                .cell(&mut check, "cost")
                .await
                .unwrap(),
            Cell::Decimal(value)
        );
    }
    drop((check, one, two, peer));
    h.cleanup().await.unwrap();
}

// no spec case (see above).
#[tokio::test]
async fn one_row_and_company_written_twice_keeps_the_retried_writers_value() {
    let h = harness().await;
    let peer = peer(&h);
    let id = committed_products(&h, &["Contended"]).await[0];
    let (first, retried) = (dec("9007199254740993.125"), dec("9007199254740994.25"));
    let mut one = h.begin().await.unwrap();
    let mut two = peer.begin(SUPERUSER_ID).await.unwrap();
    // 1. the second writer reads company 1 before the first writer commits
    let stale = product(&two.env(), 1, id);
    stale.cell(&mut two, "cost").await.unwrap();
    // 2. the first writer writes company 1 and commits
    read_then_write(&mut one, 1, id, first).await;
    one.commit().await.unwrap();
    // 3. the second writer writes the same company on its old snapshot: refused
    stale.write_values(&mut two, &cost(retried)).await.unwrap();
    serialization_failure(&two.commit().await.unwrap_err());
    two.rollback().await.unwrap();
    // 4. the retry sees the first writer's value and replaces it
    let fresh = product(&two.env(), 1, id);
    assert_eq!(
        fresh.cell(&mut two, "cost").await.unwrap(),
        Cell::Decimal(first)
    );
    fresh.write_values(&mut two, &cost(retried)).await.unwrap();
    two.commit().await.unwrap();

    let mut check = h.begin().await.unwrap();
    assert_eq!(
        raw_cost(&mut check, id).await,
        Some(json!({"1": stored(retried)}))
    );
    drop((check, one, two, peer));
    h.cleanup().await.unwrap();
}

/// The attempts one writer may use, the failed ones included. With three writers on one row at
/// least one commits in every round, so three would do; the bound is the retry policy's ceiling.
const MAX_ATTEMPTS: u32 = 5;

// no spec case (see above). The retry is `Orm::run_with_options`: it runs
// the whole unit again on a serialization failure and returns any other error at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_concurrent_writers_of_one_row_all_land_within_the_retry_bound() {
    let h = harness().await;
    let id = committed_products(&h, &["Busy"]).await[0];
    let values = [
        (1, dec("9007199254740993.125")),
        (2, dec("9007199254740994.25")),
        (3, dec("9007199254740995.375")),
    ];
    let orms = [peer(&h), peer(&h), peer(&h)];
    // Every writer holds its first snapshot before any of them writes, so the first round is
    // contended whatever the scheduling; later attempts run freely.
    let start = tokio::sync::Barrier::new(values.len());
    let attempts = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
    let writer = async |index: usize| {
        let (cid, value) = values[index];
        let options = RunOptions::new(
            BeginOptions::new(SUPERUSER_ID),
            RetryPolicy::new(
                MAX_ATTEMPTS,
                Duration::from_millis(1),
                Duration::from_millis(20),
            )
            .unwrap(),
        );
        orms[index]
            .run_with_options(options, async |tx| {
                let attempt = attempts[index].fetch_add(1, Ordering::SeqCst);
                let record = product(&tx.env(), cid, id);
                record.cell(tx, "cost").await?;
                if attempt == 0 {
                    start.wait().await;
                }
                record.write_values(tx, &cost(value)).await?;
                Ok(())
            })
            .await
    };
    let outcomes = tokio::join!(writer(0), writer(1), writer(2));
    for (index, outcome) in [outcomes.0, outcomes.1, outcomes.2].into_iter().enumerate() {
        let used = attempts[index].load(Ordering::SeqCst);
        // any error other than a retried serialization failure, or an exhausted budget
        let failure = outcome.err().map(|failure| {
            format!(
                "{:?}: {}",
                failure.error().kind(),
                failure.error().message()
            )
        });
        assert_eq!(
            failure, None,
            "writer of company {} after {used} attempt(s)",
            values[index].0
        );
        println!(
            "company {} writer: {used} attempt(s) of at most {MAX_ATTEMPTS}",
            values[index].0
        );
        assert!((1..=MAX_ATTEMPTS).contains(&used), "{used} attempts");
    }

    let mut check = h.begin().await.unwrap();
    let expected: serde_json::Map<String, serde_json::Value> = values
        .iter()
        .map(|(cid, value)| (cid.to_string(), json!(stored(*value))))
        .collect();
    assert_eq!(
        raw_cost(&mut check, id).await,
        Some(serde_json::Value::Object(expected))
    );
    drop(check);
    drop(orms);
    h.cleanup().await.unwrap();
}

// no spec case (see above): rollback of a flushed company write.
#[tokio::test]
async fn a_flushed_write_rolled_back_with_its_transaction_changes_nothing() {
    let h = harness().await;
    let id = committed_products(&h, &["Kept"]).await[0];
    let (kept, discarded) = (dec("9007199254740993.125"), dec("9007199254740994.25"));
    let mut tx = h.begin().await.unwrap();
    read_then_write(&mut tx, 1, id, kept).await;
    tx.commit().await.unwrap();

    let record = product(&tx.env(), 1, id);
    record
        .write_values(&mut tx, &cost(discarded))
        .await
        .unwrap();
    tx.flush().await.unwrap();
    // the write reached the database inside the transaction
    assert_eq!(
        raw_cost(&mut tx, id).await,
        Some(json!({"1": stored(discarded)}))
    );
    tx.rollback().await.unwrap();
    drop(tx);

    let mut later = h.begin().await.unwrap();
    assert_eq!(
        raw_cost(&mut later, id).await,
        Some(json!({"1": stored(kept)}))
    );
    assert_eq!(
        product(&later.env(), 1, id)
            .cell(&mut later, "cost")
            .await
            .unwrap(),
        Cell::Decimal(kept)
    );
    drop(later);
    h.cleanup().await.unwrap();
}

// no spec case (see above): a savepoint that flushed inside and is rolled back.
#[tokio::test]
async fn a_savepoint_rolled_back_after_its_flush_restores_the_row_and_the_cached_read() {
    let h = harness().await;
    let id = committed_products(&h, &["Savepoint"]).await[0];
    let (kept, discarded, other) = (
        dec("9007199254740993.125"),
        dec("9007199254740994.25"),
        dec("9007199254740995.375"),
    );
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    let one = product(&env, 1, id);
    let two = product(&env, 2, id);
    one.write_values(&mut tx, &cost(kept)).await.unwrap();
    tx.flush().await.unwrap();
    // both reads are cached before the savepoint
    let before_one = one.cell(&mut tx, "cost").await.unwrap();
    let before_two = two.cell(&mut tx, "cost").await.unwrap();
    assert_eq!(before_one, Cell::Decimal(kept));
    let before_row = raw_cost(&mut tx, id).await;
    assert_eq!(before_row, Some(json!({"1": stored(kept)})));

    let outcome: erp_fields::Result<()> = tx
        .savepoint(async |tx| {
            one.write_values(tx, &cost(discarded)).await?;
            two.write_values(tx, &cost(other)).await?;
            tx.flush().await?;
            assert_eq!(
                raw_cost(tx, id).await,
                Some(json!({"1": stored(discarded), "2": stored(other)}))
            );
            assert_eq!(one.cell(tx, "cost").await?, Cell::Decimal(discarded));
            assert_eq!(two.cell(tx, "cost").await?, Cell::Decimal(other));
            Err(Error::validation("roll back the savepoint"))
        })
        .await;
    error(
        &outcome.unwrap_err(),
        ErrorKind::Validation,
        "roll back the savepoint",
    );
    assert_eq!(raw_cost(&mut tx, id).await, before_row);
    assert_eq!(one.cell(&mut tx, "cost").await.unwrap(), before_one);
    assert_eq!(two.cell(&mut tx, "cost").await.unwrap(), before_two);
    // the transaction is still usable and commits the value from before the savepoint
    tx.commit().await.unwrap();
    assert_eq!(raw_cost(&mut tx, id).await, before_row);
    drop(tx);
    h.cleanup().await.unwrap();
}

// no spec case (see above): a pending mutation keeps the company and the writer of
// the environment that staged it. The fixture models record a writer (`write_uid`, log access
// is on by default), so both are asserted.
#[tokio::test]
async fn a_mutation_flushed_through_another_environment_keeps_its_company_and_writer() {
    let h = harness().await;
    let id = committed_products(&h, &["Attributed"]).await[0];
    let value = dec("9007199254740993.125");
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    let writer_of = async |tx: &mut Transaction| -> Option<i64> {
        tx.query(
            "SELECT write_uid::int8 FROM acceptance_product WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap()[0]
            .get(0)
    };
    // created by the harness principal, not by Alice
    assert_ne!(writer_of(&mut tx).await, Some(ALICE));

    // staged by Alice in company 1, not flushed
    product(&env.with_user(ALICE), 1, id)
        .write_values(&mut tx, &cost(value))
        .await
        .unwrap();
    assert_eq!(raw_cost(&mut tx, id).await, None);
    // flushed by a search of Bob in company 2
    let bob_in_two = company(&env.with_user(BOB), 2);
    let found = bob_in_two
        .records(PRODUCT)
        .unwrap()
        .search(
            &mut tx,
            &(Domain::condition("id", Operator::Eq, id).unwrap()
                & Domain::condition("cost", Operator::Eq, value.to_string().as_str()).unwrap()),
        )
        .await
        .unwrap();
    // company 2 has no such cost: the value did not move to the flushing environment's company
    assert!(found.is_empty());
    // Bob's search is what flushed: the row is written before any flush without an environment
    // (the raw read does not flush, as the `None` above shows).
    assert_eq!(
        raw_cost(&mut tx, id).await,
        Some(json!({"1": stored(value)}))
    );
    assert_eq!(writer_of(&mut tx).await, Some(ALICE));
    tx.flush().await.unwrap();
    assert_eq!(
        raw_cost(&mut tx, id).await,
        Some(json!({"1": stored(value)}))
    );
    assert_eq!(writer_of(&mut tx).await, Some(ALICE));
    assert_eq!(
        product(&env, 1, id).cell(&mut tx, "cost").await.unwrap(),
        Cell::Decimal(value)
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

const CAROL: i64 = support::rpc::ADMIN;
const CASE_MODEL: &str = "acceptance.company";
const CASE_SCALARS: [&str; 3] = ["foo", "date", "moment"];
/// What one user reads of a record: `foo`, `date`, `moment`, and the `tag_id` cell.
type CaseRead = ([Value; 3], Cell);

/// A transaction of a user without elevation who may use the three companies and whose current
/// company is `current`.
async fn begin_in(h: &Harness, (uid, current): (i64, i64)) -> Transaction {
    let id = |company: i64| erp_orm::CompanyId::new(company).unwrap();
    let resolution = erp_orm::Resolution::in_id_order(
        erp_orm::GroupSet::default(),
        COMPANIES.map(id),
        std::num::NonZeroU64::MIN,
    )
    .unwrap();
    let principal = erp_orm::PrincipalBuilder::user(erp_orm::UserId::new(uid).unwrap())
        .resolution(resolution)
        .companies(vec![id(current)])
        .build()
        .unwrap();
    h.orm()
        .begin_opts(BeginOptions::for_principal(principal))
        .await
        .unwrap()
}
fn case_vals((scalars, tag): &CaseRead) -> std::collections::BTreeMap<String, Value> {
    let mut items: Vec<(&str, Value)> = CASE_SCALARS
        .into_iter()
        .zip(scalars.iter().cloned())
        .collect();
    items.push((
        "tag_id",
        match tag {
            Cell::Id(id) => Value::Int(*id),
            _ => Value::False,
        },
    ));
    vals(&items)
}
/// The record as `user` reads it, in a transaction of that user alone.
async fn read_case(h: &Harness, user: (i64, i64), id: i64) -> CaseRead {
    let mut tx = begin_in(h, user).await;
    let env = tx.env();
    assert!(!env.su());
    let record = env.records(CASE_MODEL).unwrap().browse([id]).unwrap();
    let mut scalars = [Value::False, Value::False, Value::False];
    for (slot, field) in scalars.iter_mut().zip(CASE_SCALARS) {
        *slot = record.value(&mut tx, field).await.unwrap();
    }
    let tag = record.cell(&mut tx, "tag_id").await.unwrap();
    // The typed accessors (the surface of the spec case) give the same values.
    let typed = env
        .model::<fixture::Company>()
        .unwrap()
        .browse([id])
        .unwrap();
    let label = typed.get(&mut tx, fixture::Company::FOO).await.unwrap();
    let date = typed.get(&mut tx, fixture::Company::DATE).await.unwrap();
    let moment = typed.get(&mut tx, fixture::Company::MOMENT).await.unwrap();
    let link = typed.get(&mut tx, fixture::Company::TAG_ID).await.unwrap();
    assert_eq!(
        [
            label.map_or(Value::False, |text| Value::str(&text)),
            date.map_or(Value::False, Value::Date),
            moment.map_or(Value::False, Value::DateTime),
        ],
        scalars
    );
    let linked = match tag {
        Cell::Id(target) => Some(erp_fields::RecId::from_real(target).unwrap()),
        _ => None,
    };
    assert_eq!(link, linked);
    (scalars, tag)
}
/// `user` writes `values` and commits.
async fn write_case(h: &Harness, user: (i64, i64), id: i64, values: &CaseRead) {
    let mut tx = begin_in(h, user).await;
    tx.env()
        .records(CASE_MODEL)
        .unwrap()
        .browse([id])
        .unwrap()
        .write_values(&mut tx, &case_vals(values))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn company_values_fall_back_per_company_until_written() {
    let h = harness().await;
    // The spec's companies 0, 1 and 2 are companies 1, 2 and 3 here; one user works in each.
    let (user0, user1, user2) = ((ALICE, 1), (BOB, 2), (CAROL, 3));
    let day = |year, month, date| erp_fields::Date::from_ymd(year, month, date).unwrap();
    let at = |date, hour, minute, second| {
        erp_fields::DateTime::from_parts(date, hour, minute, second, 0).unwrap()
    };

    let mut setup = h.begin().await.unwrap();
    seed(&mut setup).await;
    setup
        .execute(
            "INSERT INTO res_users (id, login, name, active, share) VALUES ($1, 'carol', 'Carol', true, false) ON CONFLICT DO NOTHING",
            &[&CAROL],
        )
        .await
        .unwrap();
    let env = company(&setup.env(), 1);

    // Last clause first, while no default exists: a null company-dependent value reads the same
    // as the null value of the plain field of its kind, cached and reloaded, in every company.
    let null = create(&mut setup, &env, CASE_MODEL, &[]).await;
    for phase in ["cached", "reloaded"] {
        if phase == "reloaded" {
            setup.flush().await.unwrap();
            setup.invalidate_all().await.unwrap();
        }
        for cid in COMPANIES {
            let record = null.with_env(company(&env, cid));
            for field in ["foo", "date", "moment", "tag_id"] {
                let plain = format!("plain_{field}");
                assert_eq!(
                    record.cell(&mut setup, field).await.unwrap(),
                    record.cell(&mut setup, &plain).await.unwrap(),
                    "{phase}: cells of {field} and {plain} in company {cid}"
                );
                assert_eq!(
                    record.value(&mut setup, field).await.unwrap(),
                    record.value(&mut setup, &plain).await.unwrap(),
                    "{phase}: values of {field} and {plain} in company {cid}"
                );
            }
        }
    }

    let tag0 = named(&mut setup, &env, TERM, "Tag 0").await.ids()[0];
    let tag1 = named(&mut setup, &env, TERM, "Tag 1").await.ids()[0];
    let tag2 = named(&mut setup, &env, TERM, "Tag 2").await.ids()[0];
    for (field, value, scope) in [
        ("foo", Value::str("default"), None),
        ("foo", Value::str("default1"), Some(user1.1)),
        ("tag_id", Value::Int(tag0), None),
    ] {
        ir_default::set(
            &mut setup,
            &env,
            CASE_MODEL,
            field,
            &value,
            None,
            scope.map(|id| erp_fields::RecId::from_real(id).unwrap()),
            None,
        )
        .await
        .unwrap();
    }
    setup.commit().await.unwrap();
    drop(setup);

    // Company 0 creates the record with its own values.
    let main: CaseRead = (
        [
            Value::str("main"),
            Value::Date(day(2001, 2, 3)),
            Value::DateTime(at(day(2001, 2, 3), 4, 5, 6)),
        ],
        Cell::Id(tag1),
    );
    let mut tx = begin_in(&h, user0).await;
    let id = tx
        .env()
        .records(CASE_MODEL)
        .unwrap()
        .create_values(&mut tx, &[case_vals(&main)])
        .await
        .unwrap()
        .ids()[0];
    tx.commit().await.unwrap();
    drop(tx);

    // Company 0 reads its values; companies 1 and 2 read their defaults: the company default
    // and the global one, empty dates, the default tag.
    let default1: CaseRead = (
        [Value::str("default1"), Value::False, Value::False],
        Cell::Id(tag0),
    );
    let default2: CaseRead = (
        [Value::str("default"), Value::False, Value::False],
        Cell::Id(tag0),
    );
    assert_eq!(read_case(&h, user0, id).await, main);
    assert_eq!(read_case(&h, user1, id).await, default1);
    assert_eq!(read_case(&h, user2, id).await, default2);

    // Company 1 writes its own values: only its reads change.
    let alpha: CaseRead = (
        [
            Value::str("alpha"),
            Value::Date(day(2004, 5, 6)),
            Value::DateTime(at(day(2004, 5, 6), 12, 30, 45)),
        ],
        Cell::Id(tag2),
    );
    write_case(&h, user1, id, &alpha).await;
    assert_eq!(read_case(&h, user0, id).await, main);
    assert_eq!(read_case(&h, user1, id).await, alpha);
    assert_eq!(read_case(&h, user2, id).await, default2);

    // Company 1 writes empty values: it reads empty, never its default; the others are unchanged.
    let empty: CaseRead = ([Value::False, Value::False, Value::False], Cell::Null);
    write_case(&h, user1, id, &empty).await;
    assert_eq!(read_case(&h, user1, id).await, empty);
    assert_eq!(read_case(&h, user0, id).await, main);
    assert_eq!(read_case(&h, user2, id).await, default2);
    h.cleanup().await.unwrap();
}

/// What a captured statement does to the database.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Statement {
    Read,
    /// An `INSERT INTO` this table (also inside a `WITH`).
    Insert(String),
    /// Any other write: `UPDATE`, `DELETE`, `MERGE`.
    OtherWrite,
}

/// Classify a statement by its keywords; `None` when it is neither a recognizable read nor a
/// write. Keywords are matched as whole words, so a write inside a `WITH` is found.
fn classify(sql: &str) -> Option<Statement> {
    let words: Vec<String> = sql
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';'))
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    if !matches!(
        words.first()?.as_str(),
        "select" | "with" | "insert" | "update" | "delete" | "merge"
    ) {
        return None;
    }
    let follows = |index: usize, next: &str| words.get(index + 1).is_some_and(|word| word == next);
    let mut inserts = Vec::new();
    for (index, word) in words.iter().enumerate() {
        match word.as_str() {
            // `FOR UPDATE` and `FOR NO KEY UPDATE` are row locks of a read
            "update" if index > 0 && matches!(words[index - 1].as_str(), "for" | "key") => {}
            "update" => return Some(Statement::OtherWrite),
            "delete" if follows(index, "from") => return Some(Statement::OtherWrite),
            "merge" if follows(index, "into") => return Some(Statement::OtherWrite),
            "insert" if follows(index, "into") => {
                inserts.push(words.get(index + 2)?.replace('"', ""));
            }
            _ => {}
        }
    }
    match inserts.len() {
        0 => Some(Statement::Read),
        1 => inserts.pop().map(Statement::Insert),
        // more than one INSERT in one statement is a second write
        _ => Some(Statement::OtherWrite),
    }
}

/// A create scenario: its name, the environment it runs in and the values it passes.
type Scenario<'a> = (&'a str, Env, Vec<(&'a str, Value)>);

/// One create of a bacon plus the flush: the statement count and the statement texts.
async fn counted_create(
    tx: &mut Transaction,
    env: &Env,
    items: &[(&str, Value)],
) -> (u64, Vec<String>) {
    let start = tx.query_count();
    tx.capture_statements(true);
    create(tx, env, "acceptance.bacon", items).await;
    tx.flush().await.unwrap();
    let statements = tx.captured().to_vec();
    tx.capture_statements(false);
    (tx.query_count() - start, statements)
}

// The count is a ceiling of the case, not a performance pin deferred.
#[tokio::test]
async fn create_with_a_company_dependent_many2one_writes_with_one_insert() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let env = company(&tx.env(), 1);
    let eggs = named(&mut tx, &env, TERM, "Eggs").await.ids()[0];
    let other = named(&mut tx, &env, TERM, "Other eggs").await.ids()[0];
    for phase in ["no default", "default set to a record"] {
        if phase == "default set to a record" {
            ir_default::set(
                &mut tx,
                &env,
                "acceptance.bacon",
                "eggs_id",
                &Value::Int(eggs),
                None,
                None,
                None,
            )
            .await
            .unwrap();
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
            ("value false", env.clone(), vec![("eggs_id", Value::False)]),
            (
                "value id",
                env.clone(),
                vec![("eggs_id", Value::Int(other))],
            ),
        ];
        for (label, scoped, items) in scenarios {
            let (count, statements) = counted_create(&mut tx, &scoped, &items).await;
            println!(" {phase} / {label}: {count} statement(s)");
            let mut writes = Vec::new();
            for statement in &statements {
                println!(
                    "    {}",
                    statement.split_whitespace().collect::<Vec<_>>().join(" ")
                );
                let kind = classify(statement);
                assert!(
                    kind.is_some(),
                    "{phase} / {label}: neither a read nor a write: {statement}"
                );
                writes.extend(kind.filter(|kind| *kind != Statement::Read));
            }
            // every counted statement was captured, so no write can hide outside the texts
            assert_eq!(statements.len() as u64, count, "{phase} / {label}");
            assert_eq!(count, 1, "{phase} / {label}: one statement per create");
            assert_eq!(
                writes,
                [Statement::Insert("acceptance_bacon".to_owned())],
                "{phase} / {label}: one INSERT and no separate value write"
            );
        }
    }
    drop(tx);
    h.cleanup().await.unwrap();
}
