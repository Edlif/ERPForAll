// SPDX-License-Identifier: MIT OR Apache-2.0
//! Posting, reset to draft and cancel of journal entries and invoices, on a full install: a
//! successful post numbers the entries, and numbering reads the declared settings (fiscal calendar,
//! lock wait), which need the installed groups. The collected refusals, re-dating into the first
//! open date (fiscal-year and tax locks), the reversal-only policy, what a posted entry and its
//! items may still change, and item changes racing a posting.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use erp_addon_account::fiscal::calendar::FiscalYearEnd;
use erp_addon_account::fiscal::ensure_periods_with;
use erp_addon_account::fiscal::settings::FiscalCalendar;
use erp_modules::system::{InitOptions, ModuleSystem};
use erp_modules::{AddonIndex, ModuleName};
use erp_sdk::prelude::*;
use erp_sdk::{BeginOptions, Command, Context, ErrorKind, Orm, SUPERUSER_ID};
use erp_testkit::TestDb;

/// Run a test future on a thread with a 16 MiB stack (the install overflows the default one).
fn big_stack<F: std::future::Future<Output = ()> + 'static>(test: fn() -> F) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(test());
        })
        .unwrap()
        .join()
        .unwrap();
}

fn addons_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("addons")
}

/// A fresh database with `account` installed.
async fn install() -> (TestDb, Orm) {
    let db = TestDb::create().await.unwrap();
    let mut index = AddonIndex::new(true);
    index.add_dev_path(addons_dir());
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
    let system = ModuleSystem::builder(db.pool().clone(), index)
        .view_types(erp_addon_web::view_type_contributions())
        .native_artifact_identities(identities)
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_uom::ADDON,
            &erp_addon_product::ADDON,
            &erp_addon_analytic::ADDON,
            &erp_addon_account::ADDON,
        ])
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .require_core_tables(erp_security::auth::core_tables())
        .build()
        .unwrap();
    let init = Box::pin(system.initialize(InitOptions::new().with_skip_auto_install(true)))
        .await
        .unwrap();
    let env = init.orm.env(SUPERUSER_ID, Context::new());
    let outcome =
        Box::pin(system.button_immediate_install(&env, &[ModuleName::new("account").unwrap()]))
            .await
            .unwrap();
    (db, outcome.orm)
}

/// A second ORM on the same database, as another server node.
fn independent_node(db: &TestDb, orm: &Orm) -> Orm {
    Orm::builder(Arc::clone(orm.registry()), db.pool().clone())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .require_core_tables(erp_security::auth::core_tables())
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap()
}

fn d(y: i32, m: u32, day: u32) -> Date {
    Date::from_ymd(y, m, day).unwrap()
}

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

fn dec(value: i64) -> Cell {
    Cell::Decimal(Decimal::from(value))
}

fn num(value: &str) -> Cell {
    Cell::Decimal(value.parse().unwrap())
}

async fn main_company(tx: &mut Transaction) -> i64 {
    tx.query(
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
        &[],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// Another root company with the main company's currency.
async fn other_company(tx: &mut Transaction, main: i64) -> i64 {
    let currency: i64 = tx
        .query(
            "SELECT currency_id::int8 FROM res_company WHERE id = $1",
            &[&main],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.env()
        .records("res.company")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Other company"))
                    .set_cell("currency_id", Cell::Id(currency)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// Monthly periods of 2024-01 to 2024-03 for `company`, committed.
async fn periods(orm: &Orm, company: i64) {
    ensure_periods_with(
        orm,
        BeginOptions::new(SUPERUSER_ID),
        &FiscalCalendar::new(FiscalYearEnd::new(31, 12).unwrap(), 12).unwrap(),
        company,
        d(2024, 1, 1),
        d(2024, 3, 31),
    )
    .await
    .unwrap();
}

/// A journal with monthly numbers of four digits, written directly so its numbering is fixed.
async fn journal(
    tx: &mut Transaction,
    company: i64,
    code: &str,
    kind: &str,
    default_account: Option<i64>,
) -> i64 {
    tx.query(
        "INSERT INTO account_journal (name, code, active, type, company_id, sequence, \
         refund_sequence, payment_sequence, sequence_reset, sequence_padding, color, \
         show_on_dashboard, default_account_id) \
         VALUES (jsonb_build_object('en_US', $2::text), $2::text, true, $3::text, $1, 10, \
         false, false, 'month', 4, 0, true, $4) RETURNING id::int8",
        &[&company, &code, &kind, &default_account],
    )
    .await
    .unwrap()[0]
        .get(0)
}

async fn account(tx: &mut Transaction, company: i64, code: &str, kind: &str) -> i64 {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.account")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text(code))
                    .set_cell("code", text(code))
                    .set_cell("account_type", text(kind)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

struct Ledger {
    bank: i64,
    income: i64,
}

async fn ledger(tx: &mut Transaction, company: i64) -> Ledger {
    journal(tx, company, "MISC", "general", None).await;
    Ledger {
        bank: account(tx, company, "1000", "asset_current").await,
        income: account(tx, company, "4000", "asset_current").await,
    }
}

fn moves(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move")
        .unwrap()
}

fn line_vals(account: i64, debit: i64, credit: i64) -> RawVals {
    let mut vals = RawVals::new().set_cell("account_id", Cell::Id(account));
    if debit != 0 {
        vals.insert("debit".into(), dec(debit));
    }
    if credit != 0 {
        vals.insert("credit".into(), dec(credit));
    }
    vals
}

fn line(account: i64, debit: i64, credit: i64) -> Command {
    Command::create(line_vals(account, debit, credit))
}

fn entry(date: Date, lines: Vec<Command>) -> RawVals {
    RawVals::new()
        .set_cell("date", Cell::Date(date))
        .set_commands("line_ids", lines)
}

fn balanced(l: &Ledger, date: Date, amount: i64) -> RawVals {
    entry(
        date,
        vec![line(l.bank, amount, 0), line(l.income, 0, amount)],
    )
}

async fn try_call(records: &Records, tx: &mut Transaction, method: &'static str) -> Result<()> {
    let records = records.clone();
    tx.savepoint(async move |tx| records.call::<(), ()>(tx, method, ()).await)
        .await
}

async fn try_write(record: &Records, tx: &mut Transaction, vals: RawVals) -> Result<()> {
    let record = record.clone();
    tx.savepoint(async move |tx| {
        record.write(tx, vals).await?;
        tx.flush().await
    })
    .await
}

async fn lines_of(record: &Records, tx: &mut Transaction) -> Records {
    record.related(tx, "line_ids").await.unwrap()
}

async fn cell(record: &Records, tx: &mut Transaction, field: &str) -> Cell {
    record.cell(tx, field).await.unwrap()
}

/// A refusal of a change on a closed entry, naming `field` when one is given.
fn refused(error: &Error, field: Option<&str>) {
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    assert!(
        error.message().contains("posted entry") || error.message().contains("cancel entry"),
        "{}",
        error.message()
    );
    if let Some(field) = field {
        assert!(
            error.message().contains(field),
            "{field}: {}",
            error.message()
        );
    }
}

#[test]
fn posting_flips_entries_and_items_and_numbers_them_in_date_order() {
    big_stack(posting_flips_body);
}

async fn posting_flips_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    let created = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                balanced(&l, d(2024, 1, 10), 5),
                balanced(&l, d(2024, 1, 5), 3),
            ],
        )
        .await
        .unwrap();
    created
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap();
    let ids = created.ids();
    for (id, number) in [(ids[1], "MISC/2024/01/0001"), (ids[0], "MISC/2024/01/0002")] {
        let record = created.browse([id]).unwrap();
        assert_eq!(cell(&record, &mut tx, "state").await, text("posted"));
        assert_eq!(cell(&record, &mut tx, "name").await, text(number));
    }
    let items = lines_of(&created, &mut tx).await;
    let states = items.cells(&mut tx, "parent_state").await.unwrap();
    assert_eq!(states.len(), 4);
    assert!(states.iter().all(|s| *s == text("posted")), "{states:?}");
    let error = try_call(&created, &mut tx, "action_post")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert!(
        error.message().contains("is not a draft"),
        "{}",
        error.message()
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn posting_collects_every_refusal_and_posts_nothing() {
    big_stack(posting_collects_body);
}

async fn posting_collects_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let other = other_company(&mut tx, company).await;
    let l = ledger(&mut tx, company).await;
    let foreign = account(&mut tx, other, "1900", "asset_current").await;
    let model = moves(&tx, company);
    let unchecked = model.with_context([("check_move_validity", Value::Bool(false))]);
    let empty = model
        .create(&mut tx, vec![entry(d(2024, 1, 3), vec![])])
        .await
        .unwrap();
    let unbalanced = unchecked
        .create(
            &mut tx,
            vec![entry(d(2024, 1, 3), vec![line(l.bank, 9, 0)])],
        )
        .await
        .unwrap();
    let both = model
        .browse(empty.ids().into_iter().chain(unbalanced.ids()))
        .unwrap();
    let error = try_call(&both, &mut tx, "action_post").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    for part in [
        "There is nothing to post",
        "balance the entry before posting",
    ] {
        assert!(error.message().contains(part), "{}", error.message());
    }
    let states = both.cells(&mut tx, "state").await.unwrap();
    assert!(states.iter().all(|s| *s == text("draft")), "{states:?}");

    let good = model
        .create(&mut tx, vec![balanced(&l, d(2024, 1, 4), 2)])
        .await
        .unwrap();
    let good_id = good.ids()[0];
    // the ORM refuses the account at create; raw SQL reaches the draft
    tx.execute(
        "UPDATE account_move_line SET account_id = $2 WHERE move_id = $1 AND account_id = $3",
        &[&good_id, &foreign, &l.bank],
    )
    .await
    .unwrap();
    let error = try_call(&good, &mut tx, "action_post").await.unwrap_err();
    assert!(
        error
            .message()
            .contains("belong to no company of the entry's company tree"),
        "{}",
        error.message()
    );
    tx.execute(
        "UPDATE account_move_line SET account_id = $3 WHERE move_id = $1 AND account_id = $2",
        &[&good_id, &foreign, &l.bank],
    )
    .await
    .unwrap();
    tx.execute(
        "UPDATE res_currency SET active = false WHERE id = \
         (SELECT currency_id FROM account_move WHERE id = $1)",
        &[&good_id],
    )
    .await
    .unwrap();
    let error = try_call(&good, &mut tx, "action_post").await.unwrap_err();
    assert!(
        error.message().contains("is archived; activate it"),
        "{}",
        error.message()
    );
    tx.execute(
        "UPDATE res_currency SET active = true WHERE id = \
         (SELECT currency_id FROM account_move WHERE id = $1)",
        &[&good_id],
    )
    .await
    .unwrap();
    try_call(&good, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&good, &mut tx, "state").await, text("posted"));
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// markers come later)
#[test]
fn posting_into_a_locked_period_redates_the_entry_and_its_items() {
    big_stack(posting_redates_body);
}

async fn posting_redates_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    tx.execute(
        "UPDATE res_company SET fiscalyear_lock_date = $2 WHERE id = $1",
        &[&company, &d(2024, 1, 31)],
    )
    .await
    .unwrap();
    let created = moves(&tx, company)
        .create(&mut tx, vec![balanced(&l, d(2024, 1, 15), 4)])
        .await
        .unwrap();
    try_call(&created, &mut tx, "action_post").await.unwrap();
    // the day after the lock is in a month before today's, so the entry takes that month's end
    assert_eq!(
        cell(&created, &mut tx, "date").await,
        Cell::Date(d(2024, 2, 29))
    );
    assert_eq!(
        cell(&created, &mut tx, "name").await,
        text("MISC/2024/02/0001")
    );
    let dates = lines_of(&created, &mut tx)
        .await
        .cells(&mut tx, "date")
        .await
        .unwrap();
    assert!(
        dates.iter().all(|c| *c == Cell::Date(d(2024, 2, 29))),
        "{dates:?}"
    );
    // a date without a fiscal period is refused naming the period generator
    let outside = moves(&tx, company)
        .create(&mut tx, vec![balanced(&l, d(2024, 6, 1), 4)])
        .await
        .unwrap();
    let error = try_call(&outside, &mut tx, "action_post")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Missing, "{}", error.message());
    assert_eq!(cell(&outside, &mut tx, "state").await, text("draft"));
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn reset_and_cancel_follow_the_reversal_only_policy() {
    big_stack(reset_and_cancel_body);
}

async fn reset_and_cancel_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    let created = moves(&tx, company)
        .create(&mut tx, vec![balanced(&l, d(2024, 2, 2), 6)])
        .await
        .unwrap();
    try_call(&created, &mut tx, "button_cancel").await.unwrap();
    assert_eq!(cell(&created, &mut tx, "state").await, text("cancel"));
    let states = lines_of(&created, &mut tx)
        .await
        .cells(&mut tx, "parent_state")
        .await
        .unwrap();
    assert!(states.iter().all(|s| *s == text("cancel")), "{states:?}");
    // a written state never leaves cancelled: only Reset to Draft does
    let error = try_write(
        &created,
        &mut tx,
        RawVals::new().set_cell("state", text("draft")),
    )
    .await
    .unwrap_err();
    refused(&error, Some("state"));
    assert_eq!(cell(&created, &mut tx, "state").await, text("cancel"));
    let error = try_call(&created, &mut tx, "button_cancel")
        .await
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("Cancelling applies to draft entries only"),
        "{}",
        error.message()
    );
    try_call(&created, &mut tx, "button_draft").await.unwrap();
    assert_eq!(cell(&created, &mut tx, "state").await, text("draft"));
    let error = try_call(&created, &mut tx, "button_draft")
        .await
        .unwrap_err();
    assert!(
        error.message().contains("can go back to draft"),
        "{}",
        error.message()
    );
    try_call(&created, &mut tx, "action_post").await.unwrap();
    for method in ["button_draft", "button_cancel"] {
        let error = try_call(&created, &mut tx, method).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UserError);
        assert!(
            error.message().contains("only allows a reversal"),
            "{method}: {}",
            error.message()
        );
        assert_eq!(cell(&created, &mut tx, "state").await, text("posted"));
    }
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// allow-list trigger comes later)
#[test]
fn a_posted_entry_keeps_its_accounting_fields() {
    big_stack(a_posted_entry_keeps_body);
}

async fn a_posted_entry_keeps_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    let created = moves(&tx, company)
        .create(&mut tx, vec![balanced(&l, d(2024, 3, 3), 8)])
        .await
        .unwrap();
    try_call(&created, &mut tx, "action_post").await.unwrap();
    for (vals, field) in [
        (
            RawVals::new().set_cell("date", Cell::Date(d(2024, 3, 4))),
            "date",
        ),
        (
            RawVals::new().set_commands("line_ids", vec![line(l.bank, 1, 0)]),
            "line_ids",
        ),
        // a written state is refused on a posted entry, with and without the superuser flag
        (RawVals::new().set_cell("state", text("draft")), "state"),
    ] {
        refused(
            &try_write(&created, &mut tx, vals.clone())
                .await
                .unwrap_err(),
            Some(field),
        );
        let sudo = created.sudo(true);
        refused(
            &try_write(&sudo, &mut tx, vals).await.unwrap_err(),
            Some(field),
        );
    }
    assert_eq!(cell(&created, &mut tx, "state").await, text("posted"));
    try_write(
        &created,
        &mut tx,
        RawVals::new().set_cell("ref", text("checked")),
    )
    .await
    .unwrap();
    let items = lines_of(&created, &mut tx).await;
    let first = items.browse([items.ids()[0]]).unwrap();
    for (vals, field) in [
        (RawVals::new().set_cell("balance", dec(9)), "balance"),
        (
            RawVals::new().set_cell("account_id", Cell::Id(l.income)),
            "account_id",
        ),
    ] {
        refused(
            &try_write(&first, &mut tx, vals).await.unwrap_err(),
            Some(field),
        );
    }
    try_write(
        &first,
        &mut tx,
        RawVals::new().set_cell("name", text("label")),
    )
    .await
    .unwrap();
    let item_model = tx.env().records("account.move.line").unwrap();
    let created_id = created.ids()[0];
    let add = tx
        .savepoint(async |tx| {
            item_model
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("move_id", Cell::Id(created_id))
                            .set_cell("account_id", Cell::Id(l.bank))
                            .set_cell("balance", dec(0)),
                    ],
                )
                .await
                .map(|_| ())
        })
        .await;
    refused(&add.unwrap_err(), None);
    let first_again = first.clone();
    refused(
        &tx.savepoint(async move |tx| first_again.unlink(tx).await)
            .await
            .unwrap_err(),
        None,
    );
    let whole = created.clone();
    refused(
        &tx.savepoint(async move |tx| whole.unlink(tx).await)
            .await
            .unwrap_err(),
        None,
    );
    assert_eq!(cell(&first, &mut tx, "balance").await, dec(8));
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// absolute ceiling)
#[test]
fn context_defaults_do_not_go_around_the_create_checks() {
    big_stack(context_defaults_body);
}

async fn context_defaults_body() {
    let (_db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    let model = moves(&tx, company);
    // an entry cannot be born posted through the context
    let born_posted = model.with_context([("default_state", Value::Str("posted".into()))]);
    let error = born_posted
        .create(&mut tx, vec![balanced(&l, d(2024, 1, 3), 10)])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert!(
        error.message().contains("default_state"),
        "{}",
        error.message()
    );
    // an item cannot reach a posted entry through the context
    let posted = model
        .create(&mut tx, vec![balanced(&l, d(2024, 1, 3), 10)])
        .await
        .unwrap();
    try_call(&posted, &mut tx, "action_post").await.unwrap();
    let items = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("account.move.line")
        .unwrap()
        .with_context([("default_move_id", Value::Int(posted.ids()[0]))]);
    let before = lines_of(&posted, &mut tx).await.ids().len();
    let error = items
        .create(&mut tx, vec![line_vals(l.bank, 5, 0)])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert!(
        error.message().contains("default_move_id"),
        "{}",
        error.message()
    );
    assert_eq!(lines_of(&posted, &mut tx).await.ids().len(), before);
    tx.rollback().await.unwrap();
}

#[test]
fn posting_runs_as_many_statements_for_one_entry_as_for_five() {
    big_stack(posting_statements_body);
}

async fn posting_statements_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    let model = moves(&tx, company);
    let create =
        |n: i64| -> Vec<RawVals> { (1..=n).map(|i| balanced(&l, d(2024, 1, 2), i)).collect() };
    // the first posting creates the number chain row
    let warm = model.create(&mut tx, create(1)).await.unwrap();
    try_call(&warm, &mut tx, "action_post").await.unwrap();
    let one = model.create(&mut tx, create(1)).await.unwrap();
    let five = model.create(&mut tx, create(5)).await.unwrap();
    let mut counts = Vec::new();
    for batch in [&one, &five] {
        tx.invalidate_all().await.unwrap();
        let before = tx.query_count();
        try_call(batch, &mut tx, "action_post").await.unwrap();
        counts.push(tx.query_count() - before);
    }
    assert_eq!(
        counts[0], counts[1],
        "statements for 1 and 5 entries: {counts:?}"
    );
    let states = five.cells(&mut tx, "state").await.unwrap();
    assert!(states.iter().all(|s| *s == text("posted")), "{states:?}");
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// Sales books of `company`: a sale journal whose default account is `income`, a receivable, a
/// 15 % sale tax and a customer. Taxes are written directly, as in `addons/account/tests/invoices.rs`.
struct Sales {
    tax: i64,
    customer: i64,
}

async fn sales(tx: &mut Transaction, company: i64) -> Sales {
    let income = account(tx, company, "400000", "income").await;
    account(tx, company, "121000", "asset_receivable").await;
    let tax_account = account(tx, company, "251000", "liability_current").await;
    journal(tx, company, "INV", "sale", Some(income)).await;
    let country: i64 = match tx
        .query("SELECT id::int8 FROM res_country ORDER BY id LIMIT 1", &[])
        .await
        .unwrap()
        .first()
    {
        Some(row) => row.get(0),
        None => tx
            .env()
            .records("res.country")
            .unwrap()
            .create(
                tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Testland"))
                        .set_cell("code", text("TL")),
                ],
            )
            .await
            .unwrap()
            .ids()[0],
    };
    let group: i64 = tx
        .query(
            "INSERT INTO account_tax_group (name, sequence, company_id) \
             VALUES (jsonb_build_object('en_US', 'VAT'), 10, $1) RETURNING id::int8",
            &[&company],
        )
        .await
        .unwrap()[0]
        .get(0);
    let tax: i64 = tx
        .query(
            "INSERT INTO account_tax (name, type_tax_use, amount_type, active, company_id, \
             root_company_id, sequence, amount, include_base_amount, is_base_affected, tax_group_id, \
             country_id, repartition_serial, analytic) \
             VALUES (jsonb_build_object('en_US', 'VAT 15%'), 'sale', 'percent', true, $1, $1, 1, \
             15, false, true, $2, $3, 0, false) RETURNING id::int8",
            &[&company, &group, &country],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.query(
        "INSERT INTO account_tax_repartition_line (factor_percent, repartition_type, document_type, \
         account_id, tax_id, company_id, sequence, use_in_tax_closing) VALUES \
         (100, 'base', 'invoice', NULL, $1, $2, 1, false), (100, 'tax', 'invoice', $3, $1, $2, 2, true), \
         (100, 'base', 'refund', NULL, $1, $2, 1, false), (100, 'tax', 'refund', $3, $1, $2, 2, true)",
        &[&tax, &company, &tax_account],
    )
    .await
    .unwrap();
    let customer = tx
        .env()
        .records("res.partner")
        .unwrap()
        .create(tx, vec![RawVals::new().set_cell("name", text("Customer"))])
        .await
        .unwrap()
        .ids()[0];
    Sales { tax, customer }
}

/// A customer invoice of one 100.00 line with the 15 % tax, dated `on`.
fn invoice(s: &Sales, on: Date) -> RawVals {
    RawVals::new()
        .set_cell("move_type", text("out_invoice"))
        .set_cell("partner_id", Cell::Id(s.customer))
        .set_cell("invoice_date", Cell::Date(on))
        .set_commands(
            "invoice_line_ids",
            vec![Command::create(
                RawVals::new()
                    .set_cell("name", text("Consulting"))
                    .set_cell("quantity", num("1"))
                    .set_cell("price_unit", num("100"))
                    .set_cell("discount", num("0"))
                    .set_commands(
                        "tax_ids",
                        [Command::set([RecId::from_real(s.tax).unwrap()])],
                    ),
            )],
        )
}

/// due date, totals, payment state and invoice lines, refused by name; the reference's list is
/// )
/// invoice line, by the ORM)
#[test]
fn a_posted_invoice_keeps_its_document_fields() {
    big_stack(a_posted_invoice_keeps_body);
}

async fn a_posted_invoice_keeps_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let s = sales(&mut tx, company).await;
    let posted = moves(&tx, company)
        .create(&mut tx, vec![invoice(&s, d(2024, 2, 10))])
        .await
        .unwrap();
    try_call(&posted, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&posted, &mut tx, "state").await, text("posted"));
    // the invoice date would carry the accounting date with it: both are named
    let error = try_write(
        &posted,
        &mut tx,
        RawVals::new().set_cell("invoice_date", Cell::Date(d(2024, 1, 5))),
    )
    .await
    .unwrap_err();
    refused(&error, Some("invoice_date"));
    refused(&error, Some("date,"));
    assert_eq!(
        cell(&posted, &mut tx, "date").await,
        Cell::Date(d(2024, 2, 10))
    );
    let product_line = posted
        .related(&mut tx, "invoice_line_ids")
        .await
        .unwrap()
        .ids()[0];
    for (vals, field) in [
        (
            RawVals::new().set_cell("invoice_date_due", Cell::Date(d(2024, 3, 30))),
            "invoice_date_due",
        ),
        (
            RawVals::new().set_cell("amount_total", num("0")),
            "amount_total",
        ),
        (
            RawVals::new().set_cell("amount_residual", num("0")),
            "amount_residual",
        ),
        (
            RawVals::new().set_cell("payment_state", text("paid")),
            "payment_state",
        ),
        (
            RawVals::new().set_commands(
                "invoice_line_ids",
                vec![Command::update(
                    RecId::from_real(product_line).unwrap(),
                    RawVals::new().set_cell("price_unit", num("1")),
                )],
            ),
            "invoice_line_ids",
        ),
    ] {
        refused(
            &try_write(&posted, &mut tx, vals).await.unwrap_err(),
            Some(field),
        );
    }
    let item = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .browse([product_line])
        .unwrap();
    for (vals, field) in [
        (RawVals::new().set_cell("quantity", num("3")), "quantity"),
        (
            RawVals::new().set_cell("price_unit", num("1")),
            "price_unit",
        ),
        (RawVals::new().set_cell("discount", num("50")), "discount"),
        (
            RawVals::new().set_cell("price_subtotal", num("1")),
            "price_subtotal",
        ),
        (
            RawVals::new().set_cell("price_total", num("1")),
            "price_total",
        ),
        (
            RawVals::new().set_cell("product_id", Cell::Null),
            "product_id",
        ),
    ] {
        refused(
            &try_write(&item, &mut tx, vals).await.unwrap_err(),
            Some(field),
        );
    }
    assert_eq!(cell(&posted, &mut tx, "amount_total").await, num("115"));
    // the reference and the item label stay editable
    try_write(
        &posted,
        &mut tx,
        RawVals::new().set_cell("ref", text("PO 7")),
    )
    .await
    .unwrap();
    try_write(
        &item,
        &mut tx,
        RawVals::new().set_cell("name", text("Advice")),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// first open date; an untaxed entry of the same date keeps it; the checks on later changes come
/// with the reset policy)
#[test]
fn the_tax_lock_date_binds_taxed_entries_at_posting() {
    big_stack(tax_lock_body);
}

async fn tax_lock_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let s = sales(&mut tx, company).await;
    let l = ledger(&mut tx, company).await;
    tx.execute(
        "UPDATE res_company SET tax_lock_date = $2 WHERE id = $1",
        &[&company, &d(2024, 1, 31)],
    )
    .await
    .unwrap();
    let taxed = moves(&tx, company)
        .create(&mut tx, vec![invoice(&s, d(2024, 1, 15))])
        .await
        .unwrap();
    let untaxed = moves(&tx, company)
        .create(&mut tx, vec![balanced(&l, d(2024, 1, 15), 4)])
        .await
        .unwrap();
    try_call(&taxed, &mut tx, "action_post").await.unwrap();
    try_call(&untaxed, &mut tx, "action_post").await.unwrap();
    // a sales document never goes past today, and the day after the lock is in a month before
    // today's: the end of that month
    assert_eq!(
        cell(&taxed, &mut tx, "date").await,
        Cell::Date(d(2024, 2, 29))
    );
    assert_eq!(
        cell(&untaxed, &mut tx, "date").await,
        Cell::Date(d(2024, 1, 15))
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// Committed: the main company's books with periods and one draft entry per `amounts`.
async fn committed_drafts(orm: &Orm, amounts: &[i64]) -> (i64, Ledger, Vec<i64>) {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    let created = moves(&tx, company)
        .create(
            &mut tx,
            amounts
                .iter()
                .map(|a| balanced(&l, d(2024, 1, 20), *a))
                .collect(),
        )
        .await
        .unwrap();
    let ids = created.ids();
    tx.commit().await.unwrap();
    (company, l, ids)
}

async fn snapshot(tx: &mut Transaction, entry: i64) {
    let state: String = tx
        .query(
            "SELECT state::text FROM account_move WHERE id = $1",
            &[&entry],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(state, "draft");
}

fn serialization_failure(result: Result<()>) {
    let error = result.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Concurrency, "{}", error.message());
    assert_eq!(error.sqlstate(), Some("40001"), "{}", error.message());
}

async fn add_items(tx: &mut Transaction, company: i64, l: &Ledger, entry: i64) -> Result<()> {
    let model = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("account.move.line")
        .unwrap();
    model
        .create(
            tx,
            vec![
                line_vals(l.bank, 7, 0).set_cell("move_id", Cell::Id(entry)),
                line_vals(l.income, 0, 7).set_cell("move_id", Cell::Id(entry)),
            ],
        )
        .await
        .map(|_| ())
}

/// the two fails with a serialization error, in either order, so no posted entry holds items its
/// posting did not check)
#[test]
fn items_added_while_an_entry_is_posted_do_not_both_commit() {
    big_stack(concurrent_items_body);
}

async fn concurrent_items_body() {
    let (db, orm) = install().await;
    let node_b = independent_node(&db, &orm);
    let (company, l, ids) = committed_drafts(&orm, &[5, 6]).await;

    // the poster's snapshot is older than the committed items: the poster fails at its lock
    let mut poster = orm.begin(SUPERUSER_ID).await.unwrap();
    snapshot(&mut poster, ids[0]).await;
    let mut writer = node_b.begin(SUPERUSER_ID).await.unwrap();
    add_items(&mut writer, company, &l, ids[0]).await.unwrap();
    writer.commit().await.unwrap();
    drop(writer);
    let entry = moves(&poster, company).browse([ids[0]]).unwrap();
    serialization_failure(entry.call::<(), ()>(&mut poster, "action_post", ()).await);
    poster.rollback().await.unwrap();
    // a finished transaction keeps its pool connection until it is dropped
    drop(poster);

    // the writer's snapshot is older than the committed posting: the writer fails at its lock
    let mut writer = node_b.begin(SUPERUSER_ID).await.unwrap();
    snapshot(&mut writer, ids[1]).await;
    let mut poster = orm.begin(SUPERUSER_ID).await.unwrap();
    let entry = moves(&poster, company).browse([ids[1]]).unwrap();
    entry
        .call::<(), ()>(&mut poster, "action_post", ())
        .await
        .unwrap();
    poster.commit().await.unwrap();
    drop(poster);
    serialization_failure(add_items(&mut writer, company, &l, ids[1]).await);
    writer.rollback().await.unwrap();
    drop(writer);

    let mut check = orm.begin(SUPERUSER_ID).await.unwrap();
    let rows = check
        .query(
            "SELECT count(*)::int8 FROM account_move_line WHERE move_id = $1",
            &[&ids[1]],
        )
        .await
        .unwrap();
    assert_eq!(
        rows[0].get::<_, i64>(0),
        2,
        "the posted entry keeps its two items"
    );
    check.rollback().await.unwrap();
    drop(check);
    drop(node_b);
    drop(orm);
    db.cleanup().await.unwrap();
}
