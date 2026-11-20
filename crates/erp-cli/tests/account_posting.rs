// SPDX-License-Identifier: MIT OR Apache-2.0
//! Posting, reset to draft and cancel of journal entries and invoices, on a full install: a
//! successful post numbers the entries, and numbering reads the declared settings (fiscal calendar,
//! lock wait), which need the installed groups. The collected refusals, re-dating into the first
//! open date (fiscal-year and tax locks), the reversal-only policy, what a posted entry and its
//! items may still change, item changes racing a posting, the numbers of invoices and credit notes,
//! a draft named by hand, and an invoice whose line was changed without its header.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use erp_addon_account::fiscal::calendar::{FiscalYearEnd, next_day, previous_day};
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
    let (db, orm, _) = install_system().await;
    (db, orm)
}

/// [`install`] with the module system, for an upgrade.
async fn install_system() -> (TestDb, Orm, ModuleSystem) {
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
    (db, outcome.orm, system)
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

/// A plain entry's total is the sum of its debit items, kept when its items change, and an upgrade
/// from 1.2 gives the entries posted before it theirs.
#[test]
fn a_plain_entry_shows_the_total_of_its_debits() {
    big_stack(entry_total_body);
}

async fn entry_total_body() {
    let (db, orm, system) = install_system().await;
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
                entry(
                    d(2024, 1, 11),
                    vec![
                        line(l.bank, 7, 0),
                        line(l.income, 0, 4),
                        line(l.income, 0, 3),
                    ],
                ),
            ],
        )
        .await
        .unwrap();
    let ids = created.ids();
    let (first, second) = (
        created.browse([ids[0]]).unwrap(),
        created.browse([ids[1]]).unwrap(),
    );
    assert_eq!(cell(&first, &mut tx, "amount_total").await, dec(5));
    assert_eq!(cell(&second, &mut tx, "amount_total").await, dec(7));
    // new items change it
    first
        .write(
            &mut tx,
            RawVals::new().set_commands("line_ids", vec![line(l.bank, 2, 0), line(l.income, 0, 2)]),
        )
        .await
        .unwrap();
    assert_eq!(cell(&first, &mut tx, "amount_total").await, dec(7));
    created
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap();
    assert_eq!(cell(&first, &mut tx, "amount_total").await, dec(7));
    // a database of 1.2 kept these at zero
    tx.execute(
        "UPDATE account_move SET amount_total = 0 WHERE id = ANY($1)",
        &[&ids],
    )
    .await
    .unwrap();
    // the Journal Entries action as a release before 1.3 stored it: limited to miscellaneous entries
    let stale = tx
        .execute(
            "UPDATE ir_act_window a SET domain = '[(''move_type'', ''='', ''entry'')]' \
             FROM ir_model_data d \
             WHERE d.module = 'account' AND d.name = 'action_move_journal_line' AND a.id = d.res_id",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(stale, 1, "the Journal Entries action is in the database");
    tx.execute(
        "UPDATE ir_module_module SET latest_version = '1.2.0' WHERE name = 'account'",
        &[],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    let env = orm.env(SUPERUSER_ID, Context::new());
    let upgraded =
        Box::pin(system.button_immediate_upgrade(&env, &[ModuleName::new("account").unwrap()]))
            .await
            .unwrap();
    drop(orm);
    let mut tx = upgraded.orm.begin(SUPERUSER_ID).await.unwrap();
    let totals: Vec<String> = tx
        .query(
            "SELECT amount_total::text FROM account_move WHERE id = ANY($1) ORDER BY id",
            &[&ids],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    let totals: Vec<Decimal> = totals.iter().map(|t| t.parse().unwrap()).collect();
    assert_eq!(totals, [Decimal::from(7), Decimal::from(7)]);
    let version: String = tx
        .query(
            "SELECT latest_version FROM ir_module_module WHERE name = 'account'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let manifest = erp_modules::Manifest::from_toml(
        "account",
        &std::fs::read_to_string(addons_dir().join("account/manifest.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(version, manifest.version().to_string());
    let domain: Option<String> = tx
        .query(
            "SELECT a.domain FROM ir_act_window a JOIN ir_model_data d ON a.id = d.res_id \
             WHERE d.module = 'account' AND d.name = 'action_move_journal_line'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(
        domain.as_deref(),
        Some("[]"),
        "the upgrade clears the domain an earlier release stored: the list shows every entry"
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(upgraded);
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
    // a date whose period does not exist yet posts: posting creates the period it needs
    let outside = moves(&tx, company)
        .create(&mut tx, vec![balanced(&l, d(2024, 6, 1), 4)])
        .await
        .unwrap();
    try_call(&outside, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&outside, &mut tx, "state").await, text("posted"));
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

/// A new entry's form gets the company currency from `default_get`, before anything is saved.
#[test]
fn a_new_entry_form_shows_the_company_currency() {
    big_stack(default_currency_body);
}

async fn default_currency_body() {
    let (_db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let expected: i64 = tx
        .query(
            "SELECT currency_id::int8 FROM res_company WHERE id = $1",
            &[&company],
        )
        .await
        .unwrap()[0]
        .get(0);
    let defaults = moves(&tx, company)
        .default_get(&mut tx, &["currency_id"])
        .await
        .unwrap();
    assert_eq!(defaults.get("currency_id"), Some(&Value::Int(expected)));
    tx.rollback().await.unwrap();
}

/// A tax of `rate` percent for `use_` ('sale' or 'purchase'), written directly as in `sales`.
async fn plain_tax(tx: &mut Transaction, company: i64, name: &str, rate: i64, use_: &str) -> i64 {
    let account = account(
        tx,
        company,
        &format!("26{rate}{}", use_.len()),
        "liability_current",
    )
    .await;
    let country: i64 = tx
        .query("SELECT id::int8 FROM res_country ORDER BY id LIMIT 1", &[])
        .await
        .unwrap()[0]
        .get(0);
    let group: i64 = tx
        .query(
            "SELECT id::int8 FROM account_tax_group WHERE company_id = $1 ORDER BY id LIMIT 1",
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
             VALUES (jsonb_build_object('en_US', $4::text), $5::text, 'percent', true, $1, $1, 1, \
             $6, false, true, $2, $3, 0, false) RETURNING id::int8",
            &[&company, &group, &country, &name, &use_, &Decimal::from(rate)],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.query(
        "INSERT INTO account_tax_repartition_line (factor_percent, repartition_type, document_type, \
         account_id, tax_id, company_id, sequence, use_in_tax_closing) VALUES \
         (100, 'base', 'invoice', NULL, $1, $2, 1, false), (100, 'tax', 'invoice', $3, $1, $2, 2, true), \
         (100, 'base', 'refund', NULL, $1, $2, 1, false), (100, 'tax', 'refund', $3, $1, $2, 2, true)",
        &[&tax, &company, &account],
    )
    .await
    .unwrap();
    tax
}

/// A line of `product` (none: a free-text line) whose tax set is left out (`None`) or sent as given.
fn line_of(product: Option<i64>, taxes: Option<&[i64]>) -> Command {
    let mut vals = RawVals::new()
        .set_cell("name", text("Line"))
        .set_cell("quantity", num("1"))
        .set_cell("price_unit", num("100"));
    if let Some(product) = product {
        vals = vals.set_cell("product_id", Cell::Id(product));
    }
    if let Some(taxes) = taxes {
        vals = vals.set_commands(
            "tax_ids",
            [Command::set(
                taxes.iter().map(|t| RecId::from_real(*t).unwrap()),
            )],
        );
    }
    Command::create(vals)
}

/// does not choose any; no fiscal position mapping)
#[test]
fn a_product_line_that_chooses_no_taxes_takes_the_products() {
    big_stack(product_line_taxes_body);
}

async fn product_line_taxes_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let s = sales(&mut tx, company).await;
    let expense = account(&mut tx, company, "600000", "expense").await;
    account(&mut tx, company, "211000", "liability_payable").await;
    journal(&mut tx, company, "BILL", "purchase", Some(expense)).await;
    let vendor_tax = plain_tax(&mut tx, company, "Purchase 8%", 8, "purchase").await;
    let other_sale = plain_tax(&mut tx, company, "Other sale 5%", 5, "sale").await;
    let other_purchase = plain_tax(&mut tx, company, "Other purchase 5%", 5, "purchase").await;
    let product = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("product.product")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Taxed product"))
                    .set_commands(
                        "taxes_id",
                        [Command::set([RecId::from_real(s.tax).unwrap()])],
                    )
                    .set_commands(
                        "supplier_taxes_id",
                        [Command::set([RecId::from_real(vendor_tax).unwrap()])],
                    ),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    let product_b = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("product.product")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Other taxed product"))
                    .set_commands(
                        "taxes_id",
                        [Command::set([RecId::from_real(other_sale).unwrap()])],
                    )
                    .set_commands(
                        "supplier_taxes_id",
                        [Command::set([RecId::from_real(other_purchase).unwrap()])],
                    ),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    // (type, an explicit tax of that side, the product's rate on that side, the other product's)
    for (move_type, explicit, product_rate, other_rate) in [
        ("out_invoice", other_sale, 15, 5),
        ("out_refund", other_sale, 15, 5),
        ("in_invoice", other_purchase, 8, 5),
        ("in_refund", other_purchase, 8, 5),
    ] {
        let cases: [(Command, i64); 4] = [
            // no tax_ids at all: the product's taxes of that side
            (line_of(Some(product), None), product_rate),
            // a given set is kept as given, the empty one included
            (line_of(Some(product), Some(&[])), 0),
            (line_of(Some(product), Some(&[explicit])), 5),
            // no product: no taxes
            (line_of(None, None), 0),
        ];
        for (line, percent) in cases {
            let vals = RawVals::new()
                .set_cell("move_type", text(move_type))
                .set_cell("partner_id", Cell::Id(s.customer))
                .set_cell("invoice_date", Cell::Date(Date::today()))
                .set_commands("invoice_line_ids", vec![line]);
            let entry = moves(&tx, company)
                .create(&mut tx, vec![vals])
                .await
                .unwrap();
            assert_eq!(
                cell(&entry, &mut tx, "amount_untaxed").await,
                num("100"),
                "{move_type}"
            );
            let tax = Cell::Decimal(Decimal::from(percent));
            assert_eq!(
                cell(&entry, &mut tx, "amount_tax").await,
                tax,
                "{move_type} {percent}"
            );
            // a duplicate keeps the lines' taxes as they were, also a line made taxless
            let total = cell(&entry, &mut tx, "amount_total").await;
            let copied = entry.copy(&mut tx, None).await.unwrap();
            assert_eq!(
                cell(&copied, &mut tx, "amount_tax").await,
                tax,
                "copy {move_type}"
            );
            assert_eq!(
                cell(&copied, &mut tx, "amount_total").await,
                total,
                "copy {move_type}"
            );
        }
        // changing the product of an existing line takes the new product's taxes
        let vals = RawVals::new()
            .set_cell("move_type", text(move_type))
            .set_cell("partner_id", Cell::Id(s.customer))
            .set_cell("invoice_date", Cell::Date(Date::today()))
            .set_commands("invoice_line_ids", vec![line_of(Some(product), None)]);
        let entry = moves(&tx, company)
            .create(&mut tx, vec![vals])
            .await
            .unwrap();
        let line = entry
            .related(&mut tx, "invoice_line_ids")
            .await
            .unwrap()
            .rec_ids()[0];
        entry
            .write(
                &mut tx,
                RawVals::new().set_commands(
                    "invoice_line_ids",
                    [Command::update(
                        line,
                        RawVals::new().set_cell("product_id", Cell::Id(product_b)),
                    )],
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            cell(&entry, &mut tx, "amount_tax").await,
            Cell::Decimal(Decimal::from(other_rate)),
            "product change {move_type}"
        );
    }
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

async fn period_count(tx: &mut Transaction, company: i64, month: Date) -> i64 {
    let (year, number, _) = month.ymd();
    let from = d(year, number, 1);
    tx.query(
        "SELECT count(*)::int8 FROM account_fiscal_period \
         WHERE company_id = $1 AND kind = 'regular' AND date_from = $2",
        &[&company, &from],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// On a database without any fiscal period, posting an invoice of this month and one of last month
/// creates the periods they need, once each; a date beyond the horizon setting is refused in plain
/// words naming the setting. The test never calls `ensure_periods`.
#[test]
fn posting_creates_the_periods_it_needs() {
    big_stack(posting_creates_periods_body);
}

async fn posting_creates_periods_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let s = sales(&mut tx, company).await;
    let today = Date::today();
    let (year, month, _) = today.ymd();
    let this_month = d(year, month, 1);
    let last_month = if month == 1 {
        d(year - 1, 12, 1)
    } else {
        d(year, month - 1, 1)
    };
    assert_eq!(period_count(&mut tx, company, this_month).await, 0);
    assert_eq!(period_count(&mut tx, company, last_month).await, 0);
    let model = moves(&tx, company);
    let now = model
        .create(&mut tx, vec![invoice(&s, today)])
        .await
        .unwrap();
    let before = model
        .create(&mut tx, vec![invoice(&s, last_month)])
        .await
        .unwrap();
    try_call(&now, &mut tx, "action_post").await.unwrap();
    try_call(&before, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&now, &mut tx, "state").await, text("posted"));
    assert_eq!(cell(&before, &mut tx, "state").await, text("posted"));
    assert_eq!(period_count(&mut tx, company, this_month).await, 1);
    assert_eq!(period_count(&mut tx, company, last_month).await, 1);
    // a second entry of the same month finds its period and adds none
    let again = model
        .create(&mut tx, vec![invoice(&s, today)])
        .await
        .unwrap();
    try_call(&again, &mut tx, "action_post").await.unwrap();
    assert_eq!(period_count(&mut tx, company, this_month).await, 1);
    // an entry two years back creates its own month only, not the months between
    let old = model
        .create(&mut tx, vec![invoice(&s, d(year - 2, month, 15))])
        .await
        .unwrap();
    try_call(&old, &mut tx, "action_post").await.unwrap();
    let total: i64 = tx
        .query(
            "SELECT count(*)::int8 FROM account_fiscal_period WHERE company_id = $1",
            &[&company],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(
        total, 3,
        "this month, last month and the month two years back"
    );
    // far beyond the horizon: refused, naming the setting and not a function
    let far = model
        .create(&mut tx, vec![invoice(&s, d(year + 5, 1, 15))])
        .await
        .unwrap();
    let error = try_call(&far, &mut tx, "action_post").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{}", error.message());
    assert!(
        error.message().contains("fiscal_period_horizon_months"),
        "{}",
        error.message()
    );
    assert!(
        !error.message().contains("ensure_periods"),
        "{}",
        error.message()
    );
    assert_eq!(cell(&far, &mut tx, "state").await, text("draft"));
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// Two transactions posting entries of the same new month at the same time both succeed (the
/// loser of the creation race is retried by the unit of work) and the period exists once.
#[test]
fn concurrent_posts_of_a_new_month_both_succeed() {
    big_stack(concurrent_posts_body);
}

async fn concurrent_posts_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let s = sales(&mut tx, company).await;
    let income = account(&mut tx, company, "400100", "income").await;
    let second = journal(&mut tx, company, "INV2", "sale", Some(income)).await;
    let today = Date::today();
    let model = moves(&tx, company);
    let a = model
        .create(&mut tx, vec![invoice(&s, today)])
        .await
        .unwrap();
    let b = model
        .create(
            &mut tx,
            vec![invoice(&s, today).set_cell("journal_id", Cell::Id(second))],
        )
        .await
        .unwrap();
    let (a, b) = (a.ids()[0], b.ids()[0]);
    tx.commit().await.unwrap();
    let post = |id: i64| {
        let orm = orm.clone();
        async move {
            orm.run(SUPERUSER_ID, Context::new(), async |tx| {
                let records = tx
                    .env()
                    .with_company(company)?
                    .records("account.move")?
                    .browse([id])?;
                records.call::<(), ()>(tx, "action_post", ()).await
            })
            .await
        }
    };
    let (first, other) = tokio::join!(post(a), post(b));
    first.unwrap();
    other.unwrap();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let (year, month, _) = today.ymd();
    assert_eq!(period_count(&mut tx, company, d(year, month, 1)).await, 1);
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
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
    document(s, "out_invoice").set_cell("invoice_date", Cell::Date(on))
}

/// A customer document of `move_type` with one 100.00 line with the 15 % tax and no invoice date.
fn document(s: &Sales, move_type: &str) -> RawVals {
    RawVals::new()
        .set_cell("move_type", text(move_type))
        .set_cell("partner_id", Cell::Id(s.customer))
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

/// The amounts of the items of `entry` per display type, in the company currency.
async fn balances(tx: &mut Transaction, entry: i64) -> Vec<(String, Decimal)> {
    tx.flush().await.unwrap();
    tx.query(
        "SELECT display_type::text, balance::text FROM account_move_line \
         WHERE move_id = $1 ORDER BY display_type",
        &[&entry],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| {
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1).parse::<Decimal>().unwrap(),
        )
    })
    .collect()
}

/// take it too until the journal keeps a credit note chain, then `R` before the code and a counter
/// of their own)
#[test]
fn invoices_and_credit_notes_are_numbered_in_their_chains() {
    big_stack(document_numbers_body);
}

async fn document_numbers_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let s = sales(&mut tx, company).await;
    let credit_note =
        |on: Date| document(&s, "out_refund").set_cell("invoice_date", Cell::Date(on));
    let first = moves(&tx, company)
        .create(&mut tx, vec![invoice(&s, d(2024, 2, 10))])
        .await
        .unwrap();
    try_call(&first, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&first, &mut tx, "state").await, text("posted"));
    assert_eq!(
        cell(&first, &mut tx, "name").await,
        text("INV/2024/02/0001")
    );
    assert_eq!(
        cell(&first, &mut tx, "date").await,
        Cell::Date(d(2024, 2, 10))
    );
    assert_eq!(cell(&first, &mut tx, "amount_total").await, num("115"));
    let states = lines_of(&first, &mut tx)
        .await
        .cells(&mut tx, "parent_state")
        .await
        .unwrap();
    assert_eq!(states.len(), 3, "product, tax and receivable items");
    assert!(states.iter().all(|s| *s == text("posted")), "{states:?}");
    // the journal keeps no credit note chain: the credit note takes the next invoice number
    let shared = moves(&tx, company)
        .create(&mut tx, vec![credit_note(d(2024, 2, 12))])
        .await
        .unwrap();
    try_call(&shared, &mut tx, "action_post").await.unwrap();
    assert_eq!(
        cell(&shared, &mut tx, "name").await,
        text("INV/2024/02/0002")
    );
    // with its own chain a credit note starts at one behind the R, and invoices go on counting
    let journal: i64 = tx
        .query(
            "SELECT id::int8 FROM account_journal WHERE company_id = $1 AND code = 'INV'",
            &[&company],
        )
        .await
        .unwrap()[0]
        .get(0);
    let journal = tx
        .env()
        .records("account.journal")
        .unwrap()
        .browse([journal])
        .unwrap();
    try_write(
        &journal,
        &mut tx,
        RawVals::new().set_cell("refund_sequence", Cell::Bool(true)),
    )
    .await
    .unwrap();
    let own = moves(&tx, company)
        .create(&mut tx, vec![credit_note(d(2024, 2, 14))])
        .await
        .unwrap();
    try_call(&own, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&own, &mut tx, "name").await, text("RINV/2024/02/0001"));
    let next = moves(&tx, company)
        .create(&mut tx, vec![invoice(&s, d(2024, 2, 20))])
        .await
        .unwrap();
    try_call(&next, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&next, &mut tx, "name").await, text("INV/2024/02/0003"));
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// it is posted through `action_post`, and its accounting date, due date and number follow)
#[test]
fn an_undated_customer_invoice_is_posted_on_today() {
    big_stack(undated_invoice_body);
}

async fn undated_invoice_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let today = tx.env().context_today().unwrap();
    tx.rollback().await.unwrap();
    // the periods around today, so a run across midnight still finds its period
    ensure_periods_with(
        &orm,
        BeginOptions::new(SUPERUSER_ID),
        &FiscalCalendar::new(FiscalYearEnd::new(31, 12).unwrap(), 12).unwrap(),
        company,
        previous_day(today),
        next_day(today),
    )
    .await
    .unwrap();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let s = sales(&mut tx, company).await;
    let undated = moves(&tx, company)
        .create(&mut tx, vec![document(&s, "out_invoice")])
        .await
        .unwrap();
    try_call(&undated, &mut tx, "action_post").await.unwrap();
    let today = tx.env().context_today().unwrap();
    assert_eq!(cell(&undated, &mut tx, "state").await, text("posted"));
    for field in ["invoice_date", "date", "invoice_date_due"] {
        assert_eq!(
            cell(&undated, &mut tx, field).await,
            Cell::Date(today),
            "{field}"
        );
    }
    let (year, month, _) = today.ymd();
    assert_eq!(
        cell(&undated, &mut tx, "name").await,
        text(&format!("INV/{year}/{month:02}/0001"))
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// A price written on an invoice line directly runs no sync; posting rebuilds the tax line, the
/// receivable line and the totals from the product lines before it checks and posts the invoice.
#[test]
fn posting_rebuilds_an_invoice_whose_line_changed_alone() {
    big_stack(posting_rebuilds_body);
}

async fn posting_rebuilds_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let s = sales(&mut tx, company).await;
    let created = moves(&tx, company)
        .create(&mut tx, vec![invoice(&s, d(2024, 2, 10))])
        .await
        .unwrap();
    let product_line = created
        .related(&mut tx, "invoice_line_ids")
        .await
        .unwrap()
        .ids()[0];
    let item = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .browse([product_line])
        .unwrap();
    try_write(
        &item,
        &mut tx,
        RawVals::new().set_cell("price_unit", num("200")),
    )
    .await
    .unwrap();
    try_call(&created, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&created, &mut tx, "state").await, text("posted"));
    assert_eq!(cell(&created, &mut tx, "amount_untaxed").await, num("200"));
    assert_eq!(cell(&created, &mut tx, "amount_tax").await, num("30"));
    assert_eq!(cell(&created, &mut tx, "amount_total").await, num("230"));
    assert_eq!(
        balances(&mut tx, created.ids()[0]).await,
        [
            ("payment_term".to_owned(), Decimal::from(230)),
            ("product".to_owned(), Decimal::from(-200)),
            ("tax".to_owned(), Decimal::from(-30)),
        ]
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// A draft named by hand is not posted under that name; once the name is cleared it takes the
/// journal's number. A batch in which one entry is refused after the collected checks passed (its
/// date is beyond the fiscal-period horizon) posts nothing.
#[test]
fn named_drafts_unbalanced_invoice_lines_and_a_refused_batch() {
    big_stack(named_draft_body);
}

async fn named_draft_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    let named = moves(&tx, company)
        .create(
            &mut tx,
            vec![balanced(&l, d(2024, 1, 10), 5).set_cell("name", text("MISC/2024/01/0001"))],
        )
        .await
        .unwrap();
    let error = try_call(&named, &mut tx, "action_post").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    assert!(
        error.message().contains("written by hand")
            && error.message().contains("MISC/2024/01/0001"),
        "{}",
        error.message()
    );
    assert_eq!(cell(&named, &mut tx, "state").await, text("draft"));
    try_write(&named, &mut tx, RawVals::new().set_cell("name", Cell::Null))
        .await
        .unwrap();
    try_call(&named, &mut tx, "action_post").await.unwrap();
    assert_eq!(
        cell(&named, &mut tx, "name").await,
        text("MISC/2024/01/0001")
    );

    // an invoice-line command on a plain entry adds a journal item: the entry must still balance
    let plain = moves(&tx, company)
        .create(&mut tx, vec![balanced(&l, d(2024, 1, 12), 7)])
        .await
        .unwrap();
    let error = try_write(
        &plain,
        &mut tx,
        RawVals::new().set_commands("invoice_line_ids", vec![line(l.bank, 2, 0)]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    assert!(
        error.message().contains("Debits and credits must be equal"),
        "{}",
        error.message()
    );
    assert_eq!(balances(&mut tx, plain.ids()[0]).await.len(), 2);

    // the second entry of the batch is dated beyond the fiscal-period horizon (a month whose period
    // is merely missing is created by the post, so only a real refusal leaves the batch draft)
    let (this_year, _, _) = Date::today().ymd();
    let batch = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                balanced(&l, d(2024, 2, 10), 3),
                balanced(&l, d(this_year + 5, 6, 10), 4),
            ],
        )
        .await
        .unwrap();
    let error = try_call(&batch, &mut tx, "action_post").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{}", error.message());
    assert!(
        error.message().contains("fiscal_period_horizon_months"),
        "{}",
        error.message()
    );
    assert_eq!(
        batch.cells(&mut tx, "state").await.unwrap(),
        [text("draft"), text("draft")]
    );
    assert_eq!(
        batch.cells(&mut tx, "name").await.unwrap(),
        [Cell::Null, Cell::Null]
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// A draft is named by its kind, a posted entry by its number, and a reference follows in
/// brackets: the breadcrumb and the form heading of a draft are no longer empty.
#[test]
fn an_entry_is_named_by_its_kind_until_it_has_a_number() {
    big_stack(entry_names_body);
}

async fn entry_names_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let l = ledger(&mut tx, company).await;
    let s = sales(&mut tx, company).await;
    let created = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                balanced(&l, d(2024, 1, 10), 5).set_cell("ref", text("Rent  of\tJanuary")),
                invoice(&s, d(2024, 2, 10)),
            ],
        )
        .await
        .unwrap();
    let ids = created.ids();
    let entry = created.browse([ids[0]]).unwrap();
    let invoice = created.browse([ids[1]]).unwrap();
    assert_eq!(
        cell(&entry, &mut tx, "display_name").await,
        text("Draft Entry (Rent of January)")
    );
    assert_eq!(
        cell(&invoice, &mut tx, "display_name").await,
        text("Draft Invoice")
    );
    try_call(&created, &mut tx, "action_post").await.unwrap();
    for (record, suffix) in [(&entry, " (Rent of January)"), (&invoice, "")] {
        let Cell::Str(number) = cell(record, &mut tx, "name").await else {
            panic!("a posted entry has a number")
        };
        assert_eq!(
            cell(record, &mut tx, "display_name").await,
            text(&format!("{number}{suffix}"))
        );
    }
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// An ORM on the same database that asks the installed access policy, so a test can act as a
/// user who is not the superuser.
async fn policy_orm(db: &TestDb, installed: &Orm) -> Orm {
    let source = erp_security::source::PolicySource::build(
        Arc::clone(installed.registry()),
        db.pool().clone(),
    )
    .await
    .unwrap();
    Orm::builder(Arc::clone(installed.registry()), db.pool().clone())
        .generation_attachment(erp_orm::GenerationAttachment::new(
            Arc::clone(installed.registry()),
            installed
                .env(SUPERUSER_ID, Context::new())
                .generation_attachment::<erp_views::PublishedViewTypes>()
                .unwrap(),
        ))
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .resolver(erp_security::resolver::Resolver::for_registry(installed.registry()).unwrap())
        .policy(Arc::new(source) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap()
}

async fn xml_id(tx: &mut Transaction, module: &str, name: &str) -> i64 {
    tx.query(
        "SELECT res_id::int8 FROM ir_model_data WHERE module = $1 AND name = $2",
        &[&module, &name],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// An internal user of `company` only, with `groups`.
async fn user(tx: &mut Transaction, login: &str, company: i64, groups: &[i64]) -> i64 {
    let internal = xml_id(tx, "base", "group_user").await;
    let vals = RawVals::new()
        .set_cell("name", text(login))
        .set_cell("login", text(login))
        .set_cell("password", text("posting-fixture-password"))
        .set_cell("company_id", Cell::Id(company))
        .set_commands(
            "company_ids",
            vec![Command::set([RecId::from_real(company).unwrap()])],
        )
        .set_commands(
            "group_ids",
            vec![Command::set(
                groups
                    .iter()
                    .chain([&internal])
                    .map(|id| RecId::from_real(*id).unwrap()),
            )],
        );
    tx.env()
        .records("res.users")
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

/// The refusal a caller without rights on an entry gets: the access error, never the text that
/// names the entry's state and number.
fn kept_private(error: &Error, number: &str) {
    assert_eq!(error.kind(), ErrorKind::Access, "{}", error.message());
    assert!(
        !error.message().contains(number) && !error.message().contains("posted entry"),
        "{}",
        error.message()
    );
}

/// The guards of posted entries read an entry by the id the caller gave. They ask the caller's
/// rights on that entry first: an accountant of another company, or a user without accounting
/// rights, gets the access refusal and learns neither the state nor the number of the entry.
#[test]
fn the_entry_guards_ask_the_caller_rights_before_they_read_an_entry() {
    big_stack(entry_guards_access_body);
}

async fn entry_guards_access_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let main = main_company(&mut tx).await;
    let second = other_company(&mut tx, main).await;
    tx.commit().await.unwrap();
    periods(&orm, main).await;
    periods(&orm, second).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let own = ledger(&mut tx, main).await;
    let foreign = ledger(&mut tx, second).await;
    let posted = moves(&tx, second)
        .create(&mut tx, vec![balanced(&foreign, d(2024, 1, 3), 10)])
        .await
        .unwrap();
    try_call(&posted, &mut tx, "action_post").await.unwrap();
    let Cell::Str(number) = cell(&posted, &mut tx, "name").await else {
        panic!("a posted entry has a number");
    };
    let draft = moves(&tx, second)
        .create(&mut tx, vec![balanced(&foreign, d(2024, 1, 4), 10)])
        .await
        .unwrap();
    let own_posted = moves(&tx, main)
        .create(&mut tx, vec![balanced(&own, d(2024, 1, 3), 10)])
        .await
        .unwrap();
    try_call(&own_posted, &mut tx, "action_post").await.unwrap();
    let Cell::Str(own_number) = cell(&own_posted, &mut tx, "name").await else {
        panic!("a posted entry has a number");
    };
    // the database keeps an active settings administrator: the rule runs on every user create
    let system = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "posting-admin", main, &[system]).await;
    let accounting = xml_id(&mut tx, "account", "group_account_user").await;
    let accountant = user(&mut tx, "posting-accountant", main, &[accounting]).await;
    let outsider = user(&mut tx, "posting-outsider", main, &[]).await;
    tx.commit().await.unwrap();
    let orm = policy_orm(&db, &orm).await;

    // an accountant of the main company, on the entries of the second company
    let mut tx = orm
        .begin(erp_orm::UserId::new(accountant).unwrap())
        .await
        .unwrap();
    let items = tx.env().records("account.move.line").unwrap();
    for entry in [posted.ids()[0], draft.ids()[0]] {
        let vals = line_vals(own.bank, 5, 0).set_cell("move_id", Cell::Id(entry));
        let model = items.clone();
        let error = tx
            .savepoint(async move |tx| model.create(tx, vec![vals]).await.map(|_| ()))
            .await
            .unwrap_err();
        kept_private(&error, &number);
    }
    let theirs = tx
        .env()
        .records("account.move")
        .unwrap()
        .browse([posted.ids()[0]])
        .unwrap();
    let error = try_write(
        &theirs,
        &mut tx,
        RawVals::new().set_cell("name", text("MINE/0001")),
    )
    .await
    .unwrap_err();
    kept_private(&error, &number);
    let record = theirs.clone();
    let error = tx
        .savepoint(async move |tx| record.unlink(tx).await)
        .await
        .unwrap_err();
    kept_private(&error, &number);
    tx.rollback().await.unwrap();
    drop(tx);

    // a user without accounting rights, on a posted entry of the user's own company
    let mut tx = orm
        .begin(erp_orm::UserId::new(outsider).unwrap())
        .await
        .unwrap();
    let items = tx.env().records("account.move.line").unwrap();
    let vals = line_vals(own.bank, 5, 0).set_cell("move_id", Cell::Id(own_posted.ids()[0]));
    let error = tx
        .savepoint(async move |tx| items.create(tx, vec![vals]).await.map(|_| ()))
        .await
        .unwrap_err();
    kept_private(&error, &own_number);
    let mine = tx
        .env()
        .records("account.move")
        .unwrap()
        .browse([own_posted.ids()[0]])
        .unwrap();
    let record = mine.clone();
    let error = tx
        .savepoint(async move |tx| record.unlink(tx).await)
        .await
        .unwrap_err();
    kept_private(&error, &own_number);
    tx.rollback().await.unwrap();
    drop(tx);

    // the accountant still gets the refusal that names the entry on the company's own entries
    let mut tx = orm
        .begin(erp_orm::UserId::new(accountant).unwrap())
        .await
        .unwrap();
    let mine = tx
        .env()
        .records("account.move")
        .unwrap()
        .browse([own_posted.ids()[0]])
        .unwrap();
    let error = try_write(
        &mine,
        &mut tx,
        RawVals::new().set_cell("name", text("MINE/0001")),
    )
    .await
    .unwrap_err();
    refused(&error, Some("name"));
    assert!(
        error.message().contains(&*own_number),
        "{}",
        error.message()
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// The totals, the amount due and the payment status of an invoice come from its lines only: a
/// caller value is refused by name on a draft too (create, write and the context default), so a
/// draft cannot be posted with a paid status or another total.
/// Posting runs the line sync once more, so an item changed directly on the item model is in
/// the posted totals.
#[test]
fn a_draft_invoice_refuses_caller_totals_and_payment_status() {
    big_stack(draft_totals_body);
}

async fn draft_totals_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.rollback().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let s = sales(&mut tx, company).await;
    let model = moves(&tx, company);
    let draft = model
        .create(&mut tx, vec![invoice(&s, d(2024, 2, 10))])
        .await
        .unwrap();
    assert_eq!(cell(&draft, &mut tx, "state").await, text("draft"));
    let figures = [
        ("amount_untaxed", num("1")),
        ("amount_tax", num("1")),
        ("amount_total", num("1")),
        ("amount_residual", num("0")),
        ("payment_state", text("paid")),
    ];
    for (field, value) in &figures {
        let before = cell(&draft, &mut tx, field).await;
        // on a write
        let error = try_write(
            &draft,
            &mut tx,
            RawVals::new().set_cell(*field, value.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        assert!(
            error.message().contains(field) && error.message().contains("computed from its lines"),
            "{field}: {}",
            error.message()
        );
        assert_eq!(cell(&draft, &mut tx, field).await, before, "{field}");
        // with the record
        let vals = invoice(&s, d(2024, 2, 11)).set_cell(*field, value.clone());
        let creator = model.clone();
        let error = tx
            .savepoint(async move |tx| creator.create(tx, vec![vals]).await.map(|_| ()))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        assert!(
            error.message().contains(field) && error.message().contains("computed from its lines"),
            "{field}: {}",
            error.message()
        );
    }
    // through the context
    let key = "default_payment_state";
    let creator = model.with_context([(key, Value::Str("paid".into()))]);
    let vals = invoice(&s, d(2024, 2, 11));
    let error = tx
        .savepoint(async move |tx| creator.create(tx, vec![vals]).await.map(|_| ()))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    assert!(error.message().contains(key), "{}", error.message());
    // posted, the invoice carries the figures of its lines
    try_call(&draft, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&draft, &mut tx, "amount_total").await, num("115"));
    assert_eq!(cell(&draft, &mut tx, "amount_residual").await, num("115"));
    assert_eq!(
        cell(&draft, &mut tx, "payment_state").await,
        text("not_paid")
    );
    // an item written directly does not run the line sync: posting runs it, so the posted
    // invoice carries the figures of the lines it has
    let second = model
        .create(&mut tx, vec![invoice(&s, d(2024, 2, 12))])
        .await
        .unwrap();
    let product_line = second.related(&mut tx, "invoice_line_ids").await.unwrap();
    try_write(
        &product_line,
        &mut tx,
        RawVals::new().set_cell("price_unit", num("200")),
    )
    .await
    .unwrap();
    assert_eq!(cell(&second, &mut tx, "amount_total").await, num("115"));
    try_call(&second, &mut tx, "action_post").await.unwrap();
    assert_eq!(cell(&second, &mut tx, "state").await, text("posted"));
    assert_eq!(cell(&second, &mut tx, "amount_untaxed").await, num("200"));
    assert_eq!(cell(&second, &mut tx, "amount_tax").await, num("30"));
    assert_eq!(cell(&second, &mut tx, "amount_total").await, num("230"));
    assert_eq!(cell(&second, &mut tx, "amount_residual").await, num("230"));
    assert_eq!(
        cell(&product_line, &mut tx, "price_subtotal").await,
        num("200")
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}
