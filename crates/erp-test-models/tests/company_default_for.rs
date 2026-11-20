// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company fields that mirror a company-dependent default (runtime half; ADR
//! 0163 supplement of 2026-11-15): read, write, declared default, scope and access. The SQL half
//! (search, order, grouping) is the follow-up task; here those refuse by name.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use erp_orm::{
    BeginOptions, CompanyId, Env, GroupSet, PrincipalBuilder, ReadGroup, Records, Resolution,
    SearchOptions, Transaction, UserId, ir_default,
};
use erp_sdk::{Cell, Domain, ErrorKind, Operation, Operator, RawVals, Value};
use erp_test_models::test_company_default_for as fx;
use erp_testkit::{DomainPolicy, Harness, Otherwise};
use std::num::NonZeroU64;

const TARGET: &str = "test_orm.company_default_for";

async fn harness() -> Harness {
    Harness::builder([&erp_addon_base::ADDON, &fx::ADDON])
        .build()
        .await
        .unwrap()
}

async fn currency(tx: &mut Transaction) -> i64 {
    let rows = tx
        .query("SELECT id::int8 FROM res_currency ORDER BY id LIMIT 1", &[])
        .await
        .unwrap();
    match rows.first() {
        Some(row) => row.get(0),
        None => erp_testkit::fixtures::seed_currency_in(tx).await.unwrap(),
    }
}

/// A company created through the ORM (the base `create`).
async fn company(tx: &mut Transaction, name: &str) -> i64 {
    let currency = currency(tx).await;
    tx.env()
        .records("res.company")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("currency_id", Cell::Id(currency)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// A company row that exists before the module is installed: inserted by SQL, no default.
async fn old_company(tx: &mut Transaction, name: &str, active: bool) -> i64 {
    let currency = currency(tx).await;
    tx.query(
        "INSERT INTO res_company (tz, name, currency_id, active) VALUES ('UTC', $1, $2, $3) RETURNING id::int8",
        &[&name, &currency, &active],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// What the module install runs for `res.company`: its `init` hook.
async fn install(tx: &mut Transaction) {
    let companies = tx.env().records("res.company").unwrap();
    let _: () = companies.call(tx, "init", ()).await.unwrap();
}

fn co(env: &Env, id: i64) -> Records {
    env.records("res.company").unwrap().browse([id]).unwrap()
}

fn in_company(env: &Env, id: i64) -> Env {
    env.with_context([("allowed_company_ids", Value::list([Value::Int(id)]))])
}

/// The value of a company field, read through the ORM.
async fn setting(tx: &mut Transaction, company: i64, field: &str) -> Value {
    co(&tx.env(), company).value(tx, field).await.unwrap()
}

/// The value of a field of target record `record`, read in `company`.
async fn on_record(tx: &mut Transaction, record: i64, company: i64, field: &str) -> Value {
    in_company(&tx.env(), company)
        .records(TARGET)
        .unwrap()
        .browse([record])
        .unwrap()
        .value(tx, field)
        .await
        .unwrap()
}

async fn target_record(tx: &mut Transaction, company: i64) -> i64 {
    in_company(&tx.env(), company)
        .records(TARGET)
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("name", Cell::Str("record".into()))],
        )
        .await
        .unwrap()
        .ids()[0]
}

async fn partner(tx: &mut Transaction, name: &str) -> i64 {
    tx.env()
        .records("res.partner")
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("name", Cell::Str(name.into()))],
        )
        .await
        .unwrap()
        .ids()[0]
}

async fn write(tx: &mut Transaction, company: i64, vals: RawVals) {
    co(&tx.env(), company).write(tx, vals).await.unwrap();
}

/// The `ir.default` rows of a target field: `(user, company, condition, json)` in id order.
type DefaultRow = (Option<i64>, Option<i64>, Option<String>, String);
async fn default_rows(tx: &mut Transaction, field: &str) -> Vec<DefaultRow> {
    tx.flush().await.unwrap();
    tx.query(
        "SELECT d.user_id::int8, d.company_id::int8, d.condition, d.json_value FROM ir_default d \
         JOIN ir_model_fields f ON f.id = d.field_id WHERE f.model = $1 AND f.name = $2 \
         ORDER BY d.id",
        &[&TARGET, &field],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
    .collect()
}

async fn set_default(tx: &mut Transaction, field: &str, value: Value, company: Option<i64>) {
    let env = tx.env();
    ir_default::set(
        tx,
        &env,
        TARGET,
        field,
        &value,
        None,
        company.map(|id| erp_fields::RecId::from_real(id).unwrap()),
        None,
    )
    .await
    .unwrap();
}

async fn delete_defaults(tx: &mut Transaction, field: &str, company: i64) {
    tx.execute(
        "DELETE FROM ir_default d USING ir_model_fields f WHERE f.id = d.field_id AND \
         f.model = $1 AND f.name = $2 AND d.company_id = $3",
        &[&TARGET, &field, &company],
    )
    .await
    .unwrap();
}

async fn finish(mut tx: Transaction, h: Harness) {
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn declared_default_reaches_old_and_new_companies() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let old = old_company(&mut tx, "Before install", true).await;
    // nothing was written for it yet: the kind's empty value
    assert_eq!(default_rows(&mut tx, "credit_limit").await, vec![]);
    let empty = setting(&mut tx, old, "credit_limit").await;
    install(&mut tx).await;
    let new = company(&mut tx, "After install").await;
    let record = target_record(&mut tx, old).await;
    let old_setting = setting(&mut tx, old, "credit_limit").await;
    assert_ne!(old_setting, empty, "the install wrote the declared default");
    assert_eq!(old_setting, Value::Float(1000.0));
    assert_eq!(setting(&mut tx, new, "credit_limit").await, old_setting);
    for id in [old, new] {
        assert_eq!(
            on_record(&mut tx, record, id, "credit_limit").await,
            old_setting
        );
    }
    finish(tx, h).await;
}

#[tokio::test]
async fn a_float_write_changes_one_company() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let a = company(&mut tx, "A").await;
    let record = target_record(&mut tx, zero).await;
    write(
        &mut tx,
        zero,
        RawVals::new()
            .set_cell("credit_limit", Cell::Float(2000.0))
            .set_cell("default_label", Cell::Str("gold".into()))
            .set_cell("name", Cell::Str("Zero renamed".into())),
    )
    .await;
    assert_eq!(
        on_record(&mut tx, record, zero, "credit_limit").await,
        Value::Float(2000.0)
    );
    assert_eq!(
        on_record(&mut tx, record, a, "credit_limit").await,
        Value::Float(1000.0)
    );
    assert_eq!(
        setting(&mut tx, zero, "credit_limit").await,
        Value::Float(2000.0)
    );
    assert_eq!(
        setting(&mut tx, a, "credit_limit").await,
        Value::Float(1000.0)
    );
    assert_eq!(
        setting(&mut tx, zero, "default_label").await,
        Value::str("gold")
    );
    assert_eq!(
        on_record(&mut tx, record, zero, "label").await,
        Value::str("gold")
    );
    assert_eq!(
        setting(&mut tx, zero, "name").await,
        Value::str("Zero renamed")
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn a_many2one_default_is_set_and_cleared() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let record = target_record(&mut tx, zero).await;
    let p = partner(&mut tx, "Default partner").await;
    let empty = setting(&mut tx, zero, "default_partner_id").await;
    assert_eq!(empty, Value::False);
    assert_eq!(on_record(&mut tx, record, zero, "partner_id").await, empty);
    write(
        &mut tx,
        zero,
        RawVals::new().set_cell("default_partner_id", Cell::Id(p)),
    )
    .await;
    let set = setting(&mut tx, zero, "default_partner_id").await;
    assert_ne!(set, empty);
    assert_eq!(
        co(&tx.env(), zero)
            .cell(&mut tx, "default_partner_id")
            .await
            .unwrap(),
        Cell::Id(p)
    );
    assert_eq!(on_record(&mut tx, record, zero, "partner_id").await, set);
    write(
        &mut tx,
        zero,
        RawVals::new().set_cell("default_partner_id", Cell::Null),
    )
    .await;
    assert_eq!(setting(&mut tx, zero, "default_partner_id").await, empty);
    assert_eq!(on_record(&mut tx, record, zero, "partner_id").await, empty);
    assert_eq!(
        default_rows(&mut tx, "partner_id").await,
        vec![(None, Some(zero), None, "false".to_owned())]
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn a_global_default_does_not_change_a_company_with_its_own() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let record = target_record(&mut tx, zero).await;
    let before = setting(&mut tx, zero, "credit_limit").await;
    set_default(&mut tx, "credit_limit", Value::Float(2000.0), None).await;
    assert_eq!(setting(&mut tx, zero, "credit_limit").await, before);
    assert_eq!(before, Value::Float(1000.0));
    assert_eq!(
        on_record(&mut tx, record, zero, "credit_limit").await,
        before
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn json_false_defaults_read_the_empty_value() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let record = target_record(&mut tx, zero).await;
    set_default(&mut tx, "credit_limit", Value::False, Some(zero)).await;
    set_default(&mut tx, "partner_id", Value::False, Some(zero)).await;
    assert_eq!(
        setting(&mut tx, zero, "credit_limit").await,
        Value::Float(0.0)
    );
    assert_eq!(
        setting(&mut tx, zero, "default_partner_id").await,
        Value::False
    );
    assert_eq!(
        on_record(&mut tx, record, zero, "credit_limit").await,
        Value::Float(0.0)
    );
    assert_eq!(
        on_record(&mut tx, record, zero, "partner_id").await,
        Value::False
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn deleted_defaults_read_empty_and_a_global_default_shows() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let record = target_record(&mut tx, zero).await;
    assert_eq!(
        setting(&mut tx, zero, "credit_limit").await,
        Value::Float(1000.0)
    );
    delete_defaults(&mut tx, "credit_limit", zero).await;
    delete_defaults(&mut tx, "partner_id", zero).await;
    assert_eq!(
        setting(&mut tx, zero, "credit_limit").await,
        Value::Float(0.0)
    );
    assert_eq!(
        setting(&mut tx, zero, "default_partner_id").await,
        Value::False
    );
    set_default(&mut tx, "credit_limit", Value::Float(2000.0), None).await;
    assert_eq!(
        setting(&mut tx, zero, "credit_limit").await,
        Value::Float(2000.0)
    );
    assert_eq!(
        on_record(&mut tx, record, zero, "credit_limit").await,
        Value::Float(2000.0)
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn a_dangling_many2one_default_reads_empty() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let record = target_record(&mut tx, zero).await;
    const MISSING: i64 = 97_979_797;
    // written directly: ir_default::set checks the type only
    set_default(&mut tx, "partner_id", Value::Int(MISSING), Some(zero)).await;
    assert_eq!(
        default_rows(&mut tx, "partner_id").await,
        vec![(None, Some(zero), None, MISSING.to_string())]
    );
    assert_eq!(
        setting(&mut tx, zero, "default_partner_id").await,
        Value::False
    );
    assert_eq!(
        on_record(&mut tx, record, zero, "partner_id").await,
        Value::False
    );
    // through the company field: what the ordinary many2one write validation answers
    delete_defaults(&mut tx, "partner_id", zero).await;
    let answer = co(&tx.env(), zero)
        .write(
            &mut tx,
            RawVals::new().set_cell("default_partner_id", Cell::Id(MISSING)),
        )
        .await;
    match answer {
        Ok(()) => {
            eprintln!("OBSERVED non-existing id write: accepted");
            // stored as given, and read as empty like any dangling default
            assert_eq!(
                default_rows(&mut tx, "partner_id").await,
                vec![(None, Some(zero), None, MISSING.to_string())]
            );
            assert_eq!(
                setting(&mut tx, zero, "default_partner_id").await,
                Value::False
            );
            tx.rollback().await.unwrap();
        }
        Err(error) => {
            eprintln!(
                "OBSERVED non-existing id write: refused {:?} {}",
                error.kind(),
                error.message()
            );
            assert!(
                error.message().contains(&MISSING.to_string()),
                "the refusal names the id: {}",
                error.message()
            );
            drop(tx);
        }
    }
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_write_touches_only_the_written_company() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let a = company(&mut tx, "A").await;
    let before = default_rows(&mut tx, "credit_limit").await;
    assert_eq!(
        before,
        vec![
            (None, Some(zero), None, "1000.0".to_owned()),
            (None, Some(a), None, "1000.0".to_owned()),
        ]
    );
    write(
        &mut tx,
        zero,
        RawVals::new().set_cell("credit_limit", Cell::Float(2000.0)),
    )
    .await;
    assert_eq!(
        default_rows(&mut tx, "credit_limit").await,
        vec![
            (None, Some(zero), None, "2000.0".to_owned()),
            (None, Some(a), None, "1000.0".to_owned()),
        ]
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn a_user_without_company_write_access_is_refused() {
    let policy = DomainPolicy::builder(Otherwise::Allow)
        .deny("res.company", Operation::Write)
        .rule(
            "res.company",
            Operation::Read,
            Domain::condition("name", Operator::Ne, "Hidden company").unwrap(),
        )
        .build()
        .unwrap();
    let h = Harness::builder([&erp_addon_base::ADDON, &fx::ADDON])
        .policy(policy)
        .build()
        .await
        .unwrap();
    let mut setup = h.begin().await.unwrap();
    let visible = company(&mut setup, "Visible company").await;
    let hidden = company(&mut setup, "Hidden company").await;
    setup
        .execute(
            "INSERT INTO res_users (id,name,login,active,share,company_id) VALUES \
             (2,'Reader','reader',true,false,$1)",
            &[&visible],
        )
        .await
        .unwrap();
    let before = default_rows(&mut setup, "credit_limit").await;
    setup.commit().await.unwrap();
    drop(setup);
    let ids = [visible, hidden].map(|id| CompanyId::new(id).unwrap());
    let principal = PrincipalBuilder::user(UserId::new(2).unwrap())
        .resolution(Resolution::in_id_order(GroupSet::default(), ids, NonZeroU64::MIN).unwrap())
        .companies(vec![ids[0]])
        .build()
        .unwrap();
    let mut tx = h
        .orm()
        .begin_opts(BeginOptions::for_principal(principal))
        .await
        .unwrap();
    assert_eq!(
        co(&tx.env(), visible)
            .value(&mut tx, "credit_limit")
            .await
            .unwrap(),
        Value::Float(1000.0)
    );
    let unreadable = co(&tx.env(), hidden)
        .value(&mut tx, "credit_limit")
        .await
        .unwrap_err();
    assert_eq!(unreadable.kind(), ErrorKind::Access);
    eprintln!("OBSERVED hidden read: {}", unreadable.message());
    let refused = co(&tx.env(), visible)
        .write(
            &mut tx,
            RawVals::new().set_cell("credit_limit", Cell::Float(5.0)),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Access);
    eprintln!("OBSERVED refused write: {}", refused.message());
    assert!(
        refused.message().contains("res.company") || refused.message().contains("Company"),
        "the refusal names the model: {}",
        refused.message()
    );
    drop(tx);
    let mut check = h.begin().await.unwrap();
    assert_eq!(default_rows(&mut check, "credit_limit").await, before);
    check.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_change_of_ir_default_is_seen_by_the_next_read() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    assert_eq!(
        setting(&mut tx, zero, "credit_limit").await,
        Value::Float(1000.0)
    );
    let row: i64 = tx
        .query(
            "SELECT d.id::int8 FROM ir_default d JOIN ir_model_fields f ON f.id = d.field_id \
             WHERE f.model = $1 AND f.name = 'credit_limit' AND d.company_id = $2",
            &[&TARGET, &zero],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.env()
        .records("ir.default")
        .unwrap()
        .browse([row])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("json_value", Cell::Str("3000.0".into())),
        )
        .await
        .unwrap();
    // staged, not flushed: the read flushes ir.default and projects the new value
    assert_eq!(
        setting(&mut tx, zero, "credit_limit").await,
        Value::Float(3000.0)
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn declared_defaults_are_idempotent_and_reach_archived_companies() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let own = company(&mut tx, "Own value").await;
    write(
        &mut tx,
        own,
        RawVals::new().set_cell("credit_limit", Cell::Float(0.0)),
    )
    .await;
    let archived = old_company(&mut tx, "Archived", false).await;
    let companies = tx
        .env()
        .records("res.company")
        .unwrap()
        .browse([own, archived])
        .unwrap();
    erp_sdk::apply_declared_defaults(&mut tx, &companies)
        .await
        .unwrap();
    let first = default_rows(&mut tx, "credit_limit").await;
    assert_eq!(
        first,
        vec![
            (None, Some(own), None, "0.0".to_owned()),
            (None, Some(archived), None, "1000.0".to_owned()),
        ]
    );
    erp_sdk::apply_declared_defaults(&mut tx, &companies)
        .await
        .unwrap();
    assert_eq!(default_rows(&mut tx, "credit_limit").await, first);
    // fields without a declared default seed nothing
    assert_eq!(default_rows(&mut tx, "partner_id").await, vec![]);
    assert_eq!(
        setting(&mut tx, archived, "credit_limit").await,
        Value::Float(1000.0)
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn search_order_and_grouping_refuse_by_name() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    company(&mut tx, "Zero").await;
    let companies = tx.env().records("res.company").unwrap();
    let message = |operation: &str| {
        format!(
            "res.company.credit_limit mirrors the company default of \
             test_orm.company_default_for.credit_limit and is not stored: {operation} on it is \
             not supported until a later release"
        )
    };
    let search = companies
        .search(
            &mut tx,
            &Domain::condition("credit_limit", Operator::Gt, 5.0).unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(search.kind(), ErrorKind::Validation);
    assert_eq!(search.message(), message("searching"));
    let order = companies
        .search_with(
            &mut tx,
            &Domain::TRUE,
            &SearchOptions::new().order("credit_limit"),
        )
        .await
        .unwrap_err();
    assert_eq!(order.kind(), ErrorKind::Validation);
    assert_eq!(order.message(), message("sorting"));
    let group = companies
        ._read_group(
            &mut tx,
            ReadGroup::new(Domain::TRUE).groupby(["credit_limit"]),
        )
        .await
        .unwrap_err();
    assert_eq!(group.kind(), ErrorKind::Validation);
    assert_eq!(group.message(), message("grouping"));
    let aggregate = companies
        ._read_group(
            &mut tx,
            ReadGroup::new(Domain::TRUE).aggregates(["credit_limit:sum"]),
        )
        .await
        .unwrap_err();
    assert_eq!(aggregate.kind(), ErrorKind::Validation);
    assert_eq!(aggregate.message(), message("grouping"));
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn further_kinds_round_trip() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let a = company(&mut tx, "A").await;
    let record = target_record(&mut tx, zero).await;
    for (setting_name, target_name, cell) in [
        ("default_choice", "choice", Cell::Str("b".into())),
        ("default_flag", "flag", Cell::Bool(true)),
        ("default_count", "count", Cell::Int(7)),
        ("default_label", "label", Cell::Str("silver".into())),
    ] {
        let empty = setting(&mut tx, zero, setting_name).await;
        assert_eq!(
            on_record(&mut tx, record, zero, target_name).await,
            empty,
            "{setting_name}: empty before the write"
        );
        write(
            &mut tx,
            zero,
            RawVals::new().set_cell(setting_name, cell.clone()),
        )
        .await;
        assert_eq!(
            co(&tx.env(), zero)
                .cell(&mut tx, setting_name)
                .await
                .unwrap(),
            cell,
            "{setting_name}: the company reads what was written"
        );
        let written = setting(&mut tx, zero, setting_name).await;
        assert_ne!(written, empty, "{setting_name}");
        assert_eq!(
            on_record(&mut tx, record, zero, target_name).await,
            written,
            "{setting_name}: the record reads the company default"
        );
        assert_eq!(
            setting(&mut tx, a, setting_name).await,
            empty,
            "{setting_name}: another company is untouched"
        );
        assert_eq!(
            on_record(&mut tx, record, a, target_name).await,
            empty,
            "{setting_name}: a record in another company is untouched"
        );
        // cleared again: an explicit empty default
        write(
            &mut tx,
            zero,
            RawVals::new().set_cell(setting_name, Cell::Null),
        )
        .await;
        assert_eq!(
            setting(&mut tx, zero, setting_name).await,
            empty,
            "{setting_name}: cleared"
        );
    }
    finish(tx, h).await;
}

#[tokio::test]
async fn the_default_is_stored_before_method_inverses() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    write(
        &mut tx,
        zero,
        RawVals::new()
            .set_cell("probe", Cell::Str("go".into()))
            .set_cell("credit_limit", Cell::Float(2500.0)),
    )
    .await;
    assert_eq!(
        setting(&mut tx, zero, "probe_seen").await,
        setting(&mut tx, zero, "credit_limit").await
    );
    assert_eq!(
        setting(&mut tx, zero, "probe_seen").await,
        Value::Float(2500.0)
    );
    let currency = currency(&mut tx).await;
    let created = tx
        .env()
        .records("res.company")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("Created with a limit".into()))
                    .set_cell("currency_id", Cell::Id(currency))
                    .set_cell("probe", Cell::Str("go".into()))
                    .set_cell("credit_limit", Cell::Float(3500.0)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    assert_eq!(
        setting(&mut tx, created, "credit_limit").await,
        Value::Float(3500.0)
    );
    assert_eq!(
        setting(&mut tx, created, "probe_seen").await,
        Value::Float(3500.0)
    );
    finish(tx, h).await;
}

#[tokio::test]
async fn a_related_field_reads_the_company_default() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let zero = company(&mut tx, "Zero").await;
    let record = in_company(&tx.env(), zero)
        .records(TARGET)
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("with company".into()))
                    .set_cell("company_id", Cell::Id(zero)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    assert_eq!(
        on_record(&mut tx, record, zero, "company_credit_limit").await,
        setting(&mut tx, zero, "credit_limit").await
    );
    write(
        &mut tx,
        zero,
        RawVals::new().set_cell("credit_limit", Cell::Float(2000.0)),
    )
    .await;
    assert_eq!(
        on_record(&mut tx, record, zero, "company_credit_limit").await,
        Value::Float(2000.0)
    );
    finish(tx, h).await;
}
