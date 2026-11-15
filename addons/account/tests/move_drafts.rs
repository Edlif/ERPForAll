// SPDX-License-Identifier: MIT OR Apache-2.0
//! Draft journal entries and items (first part): defaults, amounts, the balance check,
//! the line rules and the refusal of every way to post. Posting comes with.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
use erp_addon_account::moves::{AccountMove, AccountMoveLine, POST_TASK};
use erp_sdk::prelude::*;
use erp_sdk::{Command, ErrorKind};
use erp_testkit::Harness;

async fn harness() -> Harness {
    Harness::builder([
        &erp_addon_base::ADDON,
        &erp_addon_uom::ADDON,
        &erp_addon_product::ADDON,
        &erp_addon_analytic::ADDON,
        &erp_addon_account::ADDON,
    ])
    .build()
    .await
    .unwrap()
}

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

fn dec(value: i64) -> Cell {
    Cell::Decimal(Decimal::from(value))
}

/// A root company with the first currency (the fixture's when none exists yet).
async fn company(tx: &mut Transaction, name: &str) -> i64 {
    let rows = tx
        .query("SELECT id::int8 FROM res_currency ORDER BY id LIMIT 1", &[])
        .await
        .unwrap();
    let currency = match rows.first() {
        Some(row) => row.get(0),
        None => erp_testkit::fixtures::seed_currency_in(tx).await.unwrap(),
    };
    let vals = RawVals::new()
        .set_cell("name", text(name))
        .set_cell("currency_id", Cell::Id(currency));
    tx.env()
        .records("res.company")
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

/// A miscellaneous journal of `company`, written directly: the journal rules read declared
/// settings that a harness without data files does not install.
async fn journal(tx: &mut Transaction, company: i64, code: &str, sequence: i32) -> i64 {
    tx.query(
        "INSERT INTO account_journal (name, code, active, type, company_id, sequence, \
         refund_sequence, payment_sequence, sequence_reset, sequence_padding, color, show_on_dashboard) \
         VALUES (jsonb_build_object('en_US', $2::text), $2::text, true, 'general', $1, $3, \
         false, false, 'month', 4, 0, true) RETURNING id::int8",
        &[&company, &code, &sequence],
    )
    .await
    .unwrap()[0]
        .get(0)
}

async fn account(tx: &mut Transaction, company: i64, code: &str) -> i64 {
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
                    .set_cell("account_type", text("asset_current")),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

fn moves(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move")
        .unwrap()
}

fn line(account: i64, debit: i64, credit: i64) -> Command {
    let mut vals = RawVals::new().set_cell("account_id", Cell::Id(account));
    if debit != 0 {
        vals.insert("debit".into(), dec(debit));
    }
    if credit != 0 {
        vals.insert("credit".into(), dec(credit));
    }
    Command::create(vals)
}

fn entry(lines: Vec<Command>) -> RawVals {
    RawVals::new().set_commands("line_ids", lines)
}

async fn try_create(model: &Records, tx: &mut Transaction, vals: RawVals) -> Result<Records> {
    let model = model.clone();
    tx.savepoint(async move |tx| model.create(tx, vec![vals]).await)
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

async fn decimal(record: &Records, tx: &mut Transaction, field: &str) -> Decimal {
    match record.cell(tx, field).await.unwrap() {
        Cell::Decimal(value) => value,
        Cell::Null => Decimal::ZERO,
        other => panic!("{field} is {other:?}"),
    }
}

/// A company with two accounts and a miscellaneous journal.
struct Ledger {
    company: i64,
    journal: i64,
    bank: i64,
    income: i64,
}

async fn ledger(tx: &mut Transaction, name: &str) -> Ledger {
    let company = company(tx, name).await;
    let journal = journal(tx, company, "MISC", 10).await;
    let bank = account(tx, company, "1000").await;
    let income = account(tx, company, "4000").await;
    Ledger {
        company,
        journal,
        bank,
        income,
    }
}

async fn lines_of(record: &Records, tx: &mut Transaction) -> Records {
    record.related(tx, "line_ids").await.unwrap()
}

#[tokio::test]
async fn draft_models_keep_only_the_ledger_columns() {
    let h = harness().await;
    for name in ["account.move", "account.move.line"] {
        assert!(h.registry().model(name).is_some(), "missing model {name}");
    }
    let mut tx = h.begin().await.unwrap();
    let columns = tx
        .query(
            "SELECT attname::text, attgenerated::text FROM pg_attribute \
             WHERE attrelid = 'account_move_line'::regclass AND attnum > 0 AND NOT attisdropped",
            &[],
        )
        .await
        .unwrap();
    for name in [
        AccountMoveLine::DEBIT.name(),
        AccountMoveLine::CREDIT.name(),
    ] {
        let row = columns
            .iter()
            .find(|r| r.get::<_, String>(0) == name)
            .unwrap_or_else(|| panic!("no column {name}"));
        assert_eq!(row.get::<_, String>(1), "v", "{name} is a virtual column");
    }
    for name in ["move_name", "ref", "company_currency_id"] {
        assert!(
            !columns.iter().any(|r| r.get::<_, String>(0) == name),
            "{name} is read through the entry, not stored on the line"
        );
    }
    for table in ["account_move", "account_move_line"] {
        let count: i64 = tx
            .query(&format!("SELECT count(*) FROM {table}"), &[])
            .await
            .unwrap()[0]
            .get(0);
        assert_eq!(count, 0, "{table} ships no records");
    }
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_draft_takes_its_journal_company_currency_and_date() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Defaults").await;
    let second = journal(&mut tx, l.company, "MISC2", 20).await;
    let model = moves(&tx, l.company);
    let created = model
        .create(&mut tx, vec![entry(vec![line(l.bank, 0, 0)])])
        .await
        .unwrap();
    assert_eq!(
        created.cell(&mut tx, "journal_id").await.unwrap(),
        Cell::Id(l.journal),
        "the first miscellaneous journal by sequence, not {second}"
    );
    assert_eq!(
        created.cell(&mut tx, "company_id").await.unwrap(),
        Cell::Id(l.company)
    );
    let company_currency = tx
        .env()
        .records("res.company")
        .unwrap()
        .browse([l.company])
        .unwrap()
        .cell(&mut tx, "currency_id")
        .await
        .unwrap();
    assert_eq!(
        created.cell(&mut tx, "currency_id").await.unwrap(),
        company_currency
    );
    assert_eq!(
        created.cell(&mut tx, "date").await.unwrap(),
        Cell::Date(tx.env().context_today().unwrap())
    );
    assert_eq!(created.cell(&mut tx, "state").await.unwrap(), text("draft"));
    let lines = lines_of(&created, &mut tx).await;
    assert_eq!(
        lines.cell(&mut tx, "currency_id").await.unwrap(),
        company_currency
    );
    assert_eq!(
        lines.cell(&mut tx, "display_type").await.unwrap(),
        text("product")
    );
    assert_eq!(
        lines.cell(&mut tx, "journal_id").await.unwrap(),
        Cell::Id(l.journal)
    );

    // without a miscellaneous journal the refusal names the company's missing journal
    let other = company(&mut tx, "No journal").await;
    let error = try_create(&moves(&tx, other), &mut tx, RawVals::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert!(
        error
            .message()
            .contains("No miscellaneous journal is available"),
        "{}",
        error.message()
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn debit_and_credit_become_the_balance() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Amounts").await;
    let created = moves(&tx, l.company)
        .create(
            &mut tx,
            vec![entry(vec![line(l.bank, 150, 0), line(l.income, 0, 150)])],
        )
        .await
        .unwrap();
    let lines = lines_of(&created, &mut tx).await;
    assert_eq!(lines.len(), 2);
    let first = lines.browse([lines.ids()[0]]).unwrap();
    let second = lines.browse([lines.ids()[1]]).unwrap();
    assert_eq!(
        decimal(&first, &mut tx, "balance").await,
        Decimal::from(150)
    );
    assert_eq!(
        decimal(&second, &mut tx, "balance").await,
        Decimal::from(-150)
    );
    assert_eq!(decimal(&first, &mut tx, "debit").await, Decimal::from(150));
    assert_eq!(decimal(&first, &mut tx, "credit").await, Decimal::ZERO);
    assert_eq!(
        decimal(&second, &mut tx, "credit").await,
        Decimal::from(150)
    );
    assert_eq!(
        decimal(&second, &mut tx, "amount_currency").await,
        Decimal::from(-150),
        "in the company currency the amount in currency is the balance"
    );

    // a conflicting amount in currency on a company-currency line is refused
    let error = try_write(
        &first,
        &mut tx,
        RawVals::new()
            .set_cell("balance", dec(10))
            .set_cell("amount_currency", dec(11)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert!(
        error.message().contains("company currency"),
        "{}",
        error.message()
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn writing_one_side_rewrites_the_balance() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Rewrite").await;
    let unchecked =
        moves(&tx, l.company).with_context([("check_move_validity", Value::Bool(false))]);
    let created = unchecked
        .create(&mut tx, vec![entry(vec![line(l.bank, 0, 20)])])
        .await
        .unwrap();
    let lines = lines_of(&created, &mut tx)
        .await
        .with_context([("check_move_validity", Value::Bool(false))]);
    assert_eq!(
        decimal(&lines, &mut tx, "balance").await,
        Decimal::from(-20)
    );
    try_write(&lines, &mut tx, RawVals::new().set_cell("debit", dec(10)))
        .await
        .unwrap();
    assert_eq!(decimal(&lines, &mut tx, "balance").await, Decimal::from(10));
    assert_eq!(
        decimal(&lines, &mut tx, "amount_currency").await,
        Decimal::from(10)
    );
    // a zero on one side keeps the other side's stored amount
    try_write(&lines, &mut tx, RawVals::new().set_cell("credit", dec(0)))
        .await
        .unwrap();
    assert_eq!(decimal(&lines, &mut tx, "balance").await, Decimal::from(10));
    // both sides at once on one accountable line are refused
    let error = try_write(
        &lines,
        &mut tx,
        RawVals::new()
            .set_cell("debit", dec(100))
            .set_cell("credit", dec(50)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert!(
        error.message().contains("both a debit and a credit"),
        "{}",
        error.message()
    );
    // a balance that contradicts the given debit is refused
    let error = try_write(
        &lines,
        &mut tx,
        RawVals::new()
            .set_cell("debit", dec(5))
            .set_cell("balance", dec(6)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(decimal(&lines, &mut tx, "balance").await, Decimal::from(10));
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn an_unbalanced_draft_is_refused_unless_the_check_is_skipped() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Balance").await;
    let model = moves(&tx, l.company);
    let error = try_create(
        &model,
        &mut tx,
        entry(vec![line(l.bank, 100, 0), line(l.income, 0, 80)]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert!(
        error.message().contains("must be equal") && error.message().contains("by 20"),
        "{}",
        error.message()
    );
    let count: i64 = tx
        .query("SELECT count(*) FROM account_move", &[])
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(count, 0, "the refused entry was rolled back");

    // the form saves an incomplete draft with the check skipped
    let unchecked = model.with_context([("check_move_validity", Value::Bool(false))]);
    let draft = try_create(&unchecked, &mut tx, entry(vec![line(l.bank, 100, 0)]))
        .await
        .unwrap();
    // completing it later through the entry passes the check
    try_write(
        &draft.with_env(model.env().clone()),
        &mut tx,
        entry(vec![line(l.income, 0, 100)]),
    )
    .await
    .unwrap();
    // a direct line write that unbalances it is refused
    let lines = lines_of(&draft.with_env(model.env().clone()), &mut tx).await;
    let first = lines.browse([lines.ids()[0]]).unwrap();
    let error = try_write(&first, &mut tx, RawVals::new().set_cell("debit", dec(90)))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    // a malformed context value is refused instead of read as a default
    let malformed = model.with_context([("check_move_validity", Value::Str("no".into()))]);
    let error = try_create(&malformed, &mut tx, RawVals::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming);
    assert!(
        error.message().contains("check_move_validity"),
        "{}",
        error.message()
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn accountable_and_text_lines_keep_their_rules() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Lines").await;
    let unchecked =
        moves(&tx, l.company).with_context([("check_move_validity", Value::Bool(false))]);
    let error = try_create(
        &unchecked,
        &mut tx,
        entry(vec![Command::create(
            RawVals::new().set_cell("debit", dec(5)),
        )]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert!(
        error.message().contains("needs an account"),
        "{}",
        error.message()
    );
    let error = try_create(
        &unchecked,
        &mut tx,
        entry(vec![Command::create(
            RawVals::new()
                .set_cell("display_type", text("line_section"))
                .set_cell("account_id", Cell::Id(l.bank)),
        )]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert!(
        error.message().contains("section or note line"),
        "{}",
        error.message()
    );
    // a section without amount or account is fine
    try_create(
        &unchecked,
        &mut tx,
        entry(vec![Command::create(
            RawVals::new()
                .set_cell("display_type", text("line_section"))
                .set_cell("name", text("Heading")),
        )]),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn accounts_and_journals_of_another_company_are_refused() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Home").await;
    let foreign = ledger(&mut tx, "Elsewhere").await;
    let model = moves(&tx, l.company).with_context([("check_move_validity", Value::Bool(false))]);
    let error = try_create(&model, &mut tx, entry(vec![line(foreign.bank, 5, 0)]))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    let error = try_create(
        &model,
        &mut tx,
        RawVals::new()
            .set_cell("journal_id", Cell::Id(foreign.journal))
            .set_cell("company_id", Cell::Id(l.company)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn nothing_posts_an_entry_before_m6_post() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "No posting").await;
    let model = moves(&tx, l.company);
    let created = model
        .create(
            &mut tx,
            vec![entry(vec![line(l.bank, 7, 0), line(l.income, 0, 7)])],
        )
        .await
        .unwrap();
    let error = created
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert!(error.message().contains(POST_TASK), "{}", error.message());
    for vals in [
        RawVals::new().set_cell(AccountMove::STATE.name(), text("posted")),
        RawVals::new().set_cell(AccountMove::STATE.name(), text("cancel")),
    ] {
        let error = try_write(&created.sudo(true), &mut tx, vals)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UserError);
        assert!(error.message().contains(POST_TASK), "{}", error.message());
    }
    let error = try_create(
        &model.sudo(true),
        &mut tx,
        RawVals::new().set_cell("state", text("posted")),
    )
    .await
    .unwrap_err();
    assert!(error.message().contains(POST_TASK), "{}", error.message());
    let lines = lines_of(&created, &mut tx).await;
    let first = lines.browse([lines.ids()[0]]).unwrap();
    let error = try_write(
        &first.sudo(true),
        &mut tx,
        RawVals::new().set_cell("parent_state", text("posted")),
    )
    .await
    .unwrap_err();
    assert!(error.message().contains(POST_TASK), "{}", error.message());
    // raw SQL meets the draft-only constraints
    for sql in [
        "UPDATE account_move SET state = 'posted'",
        "UPDATE account_move_line SET parent_state = 'posted'",
    ] {
        let error = tx
            .savepoint(async |tx| tx.query(sql, &[]).await.map(|_| ()))
            .await
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some("23514"),
            "{sql}: {}",
            error.message()
        );
    }
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_draft_and_its_lines_are_deleted_together() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Delete").await;
    let other = journal(&mut tx, l.company, "AJ", 30).await;
    let created = moves(&tx, l.company)
        .create(
            &mut tx,
            vec![
                entry(vec![line(l.bank, 3, 0), line(l.income, 0, 3)])
                    .set_cell("name", text("MANUAL/1")),
            ],
        )
        .await
        .unwrap();
    try_write(
        &created,
        &mut tx,
        RawVals::new().set_cell("journal_id", Cell::Id(other)),
    )
    .await
    .unwrap();
    assert_eq!(created.cell(&mut tx, "name").await.unwrap(), Cell::Null);
    let lines = lines_of(&created, &mut tx).await;
    assert_eq!(
        lines.cells(&mut tx, "journal_id").await.unwrap(),
        [Cell::Id(other), Cell::Id(other)]
    );
    created.unlink(&mut tx).await.unwrap();
    tx.flush().await.unwrap();
    let count: i64 = tx
        .query("SELECT count(*) FROM account_move_line", &[])
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(count, 0);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_line_never_changes_entry() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Steal").await;
    let model = moves(&tx, l.company).with_context([("check_move_validity", Value::Bool(false))]);
    let first = model
        .create(&mut tx, vec![entry(vec![line(l.bank, 1, 0)])])
        .await
        .unwrap();
    let second = model
        .create(&mut tx, vec![entry(vec![line(l.bank, 0, 1)])])
        .await
        .unwrap();
    let stolen = lines_of(&first, &mut tx).await;
    let error = try_write(
        &stolen,
        &mut tx,
        RawVals::new().set_cell("move_id", Cell::Id(second.ids()[0])),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert!(
        error.message().contains("stays in the entry"),
        "{}",
        error.message()
    );
    let error = try_write(
        &second,
        &mut tx,
        RawVals::new().set_commands(
            "line_ids",
            vec![Command::link(RecId::from_real(stolen.ids()[0]).unwrap())],
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        stolen.cell(&mut tx, "move_id").await.unwrap(),
        Cell::Id(first.ids()[0])
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn entries_and_items_have_their_default_order() {
    let h = harness().await;
    let registry = h.registry();
    assert_eq!(
        registry.model("account.move").unwrap().order,
        "date desc, name desc, id desc"
    );
    assert_eq!(
        registry.model("account.move.line").unwrap().order,
        "date desc, move_id desc, id"
    );
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn storno_lines_wait_for_the_company_setting() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Storno").await;
    let model = moves(&tx, l.company).with_context([("check_move_validity", Value::Bool(false))]);
    let error = try_create(
        &model,
        &mut tx,
        entry(vec![Command::create(
            RawVals::new()
                .set_cell("account_id", Cell::Id(l.bank))
                .set_cell("is_storno", Cell::Bool(true)),
        )]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert!(
        error.message().contains("storno setting"),
        "{}",
        error.message()
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// moves a line with an amount to another currency is refused instead of keeping the old amounts)
#[tokio::test]
async fn currency_only_write_to_a_foreign_currency_is_refused() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let l = ledger(&mut tx, "Currency move").await;
    let foreign: i64 = tx
        .query(
            "INSERT INTO res_currency (name, symbol, rounding, active) \
             VALUES ('XTS', 'X', 0.01, true) RETURNING id::int8",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let created = moves(&tx, l.company)
        .create(
            &mut tx,
            vec![entry(vec![line(l.bank, 100, 0), line(l.income, 0, 100)])],
        )
        .await
        .unwrap();
    let lines = lines_of(&created, &mut tx).await;
    let first = lines.browse([lines.ids()[0]]).unwrap();
    let error = try_write(
        &first,
        &mut tx,
        RawVals::new().set_cell("currency_id", Cell::Id(foreign)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error}");
    assert!(
        error.message().contains("not delivered yet"),
        "{}",
        error.message()
    );
    assert_eq!(
        decimal(&first, &mut tx, "balance").await,
        Decimal::from(100)
    );
    assert_eq!(
        decimal(&first, &mut tx, "amount_currency").await,
        Decimal::from(100),
        "the refused write leaves the amounts as they were"
    );
}
