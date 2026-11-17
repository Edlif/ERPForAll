// SPDX-License-Identifier: MIT OR Apache-2.0
//! Payments on a full install: the method lines of a new bank journal, a draft without entry,
//! posting (entry, number, flags), matching with invoices (status of both), the refusals by name,
//! the lifecycle of drafts, and access (an accountant who is not an administrator, a read-only
//! user, an accountant of another company).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

#[path = "support/accounting.rs"]
mod accounting;

use accounting::*;
use erp_sdk::prelude::*;
use erp_sdk::{ErrorKind, SUPERUSER_ID};

/// A bank journal created through the ORM (so it gets its method lines) on its own cash account.
async fn bank_journal(tx: &mut Transaction, company: i64, code: &str) -> (i64, i64) {
    let cash = account(tx, company, &format!("10{code}"), "asset_cash").await;
    let id = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("account.journal")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text(code))
                    .set_cell("code", text(code))
                    .set_cell("type", text("bank"))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("default_account_id", Cell::Id(cash)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    (id, cash)
}

/// The method line of `journal` for `direction`.
async fn method_line(tx: &mut Transaction, journal: i64, direction: &str) -> i64 {
    let rows = tx
        .query(
            "SELECT id::int8 FROM account_payment_method_line \
             WHERE journal_id = $1 AND payment_type = $2 ORDER BY id",
            &[&journal, &direction],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "one {direction} line on journal {journal}");
    rows[0].get(0)
}

async fn set_line_account(tx: &mut Transaction, line: i64, account: i64) {
    tx.env()
        .records("account.payment.method.line")
        .unwrap()
        .browse([line])
        .unwrap()
        .write(
            tx,
            RawVals::new().set_cell("payment_account_id", Cell::Id(account)),
        )
        .await
        .unwrap();
}

fn payments(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.payment")
        .unwrap()
}

async fn payment(
    tx: &mut Transaction,
    company: i64,
    journal: i64,
    partner: i64,
    amount: &str,
    extra: RawVals,
) -> Records {
    let mut vals = extra
        .set_cell("journal_id", Cell::Id(journal))
        .set_cell("partner_id", Cell::Id(partner))
        .set_cell("amount", Cell::Decimal(num(amount)));
    if !vals.contains("date") {
        vals.insert("date".into(), Cell::Date(d(2024, 2, 1)));
    }
    payments(tx, company).create(tx, vec![vals]).await.unwrap()
}

async fn entry_of(tx: &mut Transaction, payment: &Records) -> Records {
    let entry = payment.related(tx, "move_id").await.unwrap();
    assert_eq!(entry.len(), 1, "a posted payment has its entry");
    entry
}

/// The journal entries that belong to `payment`.
async fn entries_of(tx: &mut Transaction, payment: &Records) -> i64 {
    tx.query(
        "SELECT count(*)::int8 FROM account_move WHERE origin_payment_id = ANY($1)",
        &[&payment.ids()],
    )
    .await
    .unwrap()[0]
        .get(0)
}

#[test]
fn a_customer_payment_posts_its_entry_and_pays_the_invoice() {
    big_stack(customer_body);
}

async fn customer_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let (bank, _) = bank_journal(&mut tx, company, "BNK9").await;
    let inbound = method_line(&mut tx, bank, "inbound").await;
    method_line(&mut tx, bank, "outbound").await;
    let receipts = account(&mut tx, company, "101310", "asset_current").await;
    set_line_account(&mut tx, inbound, receipts).await;

    let first = payment(
        &mut tx,
        company,
        bank,
        b.customer,
        "300",
        RawVals::new().set_cell("memo", text("First part")),
    )
    .await;
    assert_eq!(string(&first, &mut tx, "state").await, "draft");
    assert_eq!(
        string(&first, &mut tx, "display_name").await,
        "Draft Payment"
    );
    assert_eq!(
        first.cell(&mut tx, "payment_method_line_id").await.unwrap(),
        Cell::Id(inbound)
    );
    let rows: i64 = tx
        .query(
            "SELECT count(*)::int8 FROM account_move WHERE origin_payment_id = ANY($1)",
            &[&first.ids()],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(rows, 0, "a draft payment has no entry");

    try_call(&first, &mut tx, "action_post").await.unwrap();
    assert_eq!(string(&first, &mut tx, "state").await, "paid");
    assert_eq!(
        first.cell(&mut tx, "is_reconciled").await.unwrap(),
        Cell::Bool(false)
    );
    assert_eq!(
        first.cell(&mut tx, "is_matched").await.unwrap(),
        Cell::Bool(false)
    );
    let entry = entry_of(&mut tx, &first).await;
    assert_eq!(string(&entry, &mut tx, "state").await, "posted");
    let number = string(&first, &mut tx, "name").await;
    assert!(number.starts_with("PBNK9/2024/"), "{number}");
    assert_eq!(string(&entry, &mut tx, "name").await, number);
    assert_eq!(string(&entry, &mut tx, "ref").await, "First part");
    // a confirmed payment is never a draft again by a write, and Confirm twice books one entry
    let error = tx
        .savepoint(async |tx| {
            first
                .write(tx, RawVals::new().set_cell("state", text("draft")))
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    assert!(
        error.message().contains("The field state of a payment"),
        "{}",
        error.message()
    );
    try_call(&first, &mut tx, "action_post").await.unwrap();
    assert_eq!(entries_of(&mut tx, &first).await, 1);
    assert_eq!(string(&first, &mut tx, "name").await, number);
    // a draft that has an entry (written behind the payment's back) is not confirmed again
    tx.execute(
        "UPDATE account_payment SET state = 'draft' WHERE id = ANY($1)",
        &[&first.ids()],
    )
    .await
    .unwrap();
    tx.invalidate_model("account.payment", None).await.unwrap();
    let error = try_call(&first, &mut tx, "action_post").await.unwrap_err();
    assert!(
        error
            .message()
            .contains("confirming it again is not possible"),
        "{}",
        error.message()
    );
    assert_eq!(entries_of(&mut tx, &first).await, 1);
    tx.execute(
        "UPDATE account_payment SET state = 'paid' WHERE id = ANY($1)",
        &[&first.ids()],
    )
    .await
    .unwrap();
    tx.invalidate_model("account.payment", None).await.unwrap();
    let liquidity = items_on(&mut tx, &[&entry], receipts).await;
    let counterpart = items_on(&mut tx, &[&entry], b.receivable).await;
    assert_eq!(decimal(&liquidity, &mut tx, "debit").await, num("300"));
    assert_eq!(decimal(&counterpart, &mut tx, "credit").await, num("300"));
    assert_eq!(
        decimal(&counterpart, &mut tx, "amount_currency").await,
        num("-300")
    );
    assert_eq!(
        string(&counterpart, &mut tx, "name").await,
        "Manual Payment: First part"
    );

    let invoice = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "1000",
        d(2024, 1, 15),
    )
    .await;
    let due = items_on(&mut tx, &[&invoice], b.receivable).await;
    try_call(&due.union(&counterpart).unwrap(), &mut tx, "reconcile")
        .await
        .unwrap();
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("700")
    );
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "partial");
    assert_eq!(
        first.cell(&mut tx, "is_reconciled").await.unwrap(),
        Cell::Bool(true)
    );
    assert_eq!(
        first.cell(&mut tx, "is_matched").await.unwrap(),
        Cell::Bool(false)
    );
    assert_eq!(string(&first, &mut tx, "state").await, "paid");

    let second = payment(&mut tx, company, bank, b.customer, "700", RawVals::new()).await;
    try_call(&second, &mut tx, "action_post").await.unwrap();
    let second_entry = entry_of(&mut tx, &second).await;
    let rest = items_on(&mut tx, &[&second_entry], b.receivable).await;
    try_call(&due.union(&rest).unwrap(), &mut tx, "reconcile")
        .await
        .unwrap();
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("0")
    );
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "paid");
    for paid in [&first, &second] {
        assert_eq!(string(paid, &mut tx, "state").await, "reconciled");
    }
    // the refused confirmations used no number: the second payment takes the next one
    let (prefix, digits) = number.rsplit_once('/').unwrap();
    let next = format!(
        "{prefix}/{:0width$}",
        digits.parse::<u64>().unwrap() + 1,
        width = digits.len()
    );
    assert_eq!(string(&second, &mut tx, "name").await, next);

    // the memo of a confirmed payment is its entry's reference; what feeds the entry is refused
    first
        .write(
            &mut tx,
            RawVals::new().set_cell("memo", text("Updated memo")),
        )
        .await
        .unwrap();
    assert_eq!(string(&entry, &mut tx, "ref").await, "Updated memo");
    let error = tx
        .savepoint(async |tx| {
            first
                .write(
                    tx,
                    RawVals::new().set_cell("amount", Cell::Decimal(num("1"))),
                )
                .await
        })
        .await
        .unwrap_err();
    assert!(
        error.message().contains("is confirmed"),
        "{}",
        error.message()
    );
    for action in ["action_cancel", "action_draft"] {
        let error = try_call(&first, &mut tx, action).await.unwrap_err();
        assert!(
            error.message().contains("is confirmed"),
            "{action}: {}",
            error.message()
        );
    }
    let error = tx
        .savepoint(async |tx| first.unlink(tx).await)
        .await
        .unwrap_err();
    assert!(
        error.message().contains("is confirmed"),
        "{}",
        error.message()
    );
    // unmatching the second payment brings the invoice back to partial and both payments to paid
    try_call(&rest, &mut tx, "remove_move_reconcile")
        .await
        .unwrap();
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "partial");
    assert_eq!(string(&first, &mut tx, "state").await, "paid");
    assert_eq!(
        second.cell(&mut tx, "is_reconciled").await.unwrap(),
        Cell::Bool(false)
    );
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn what_cannot_be_posted_is_refused_by_name() {
    big_stack(refusals_body);
}

async fn refusals_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let (bank, cash_account) = bank_journal(&mut tx, company, "BNK8").await;
    let (other_bank, _) = bank_journal(&mut tx, company, "BNK7").await;
    // a journal that becomes a bank journal gets its method lines, as a new one does
    let general = journal(&mut tx, company, "GEN9", "general", None).await;
    let till = account(&mut tx, company, "101398", "asset_cash").await;
    tx.env()
        .records("account.journal")
        .unwrap()
        .browse([general])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new()
                .set_cell("type", text("bank"))
                .set_cell("default_account_id", Cell::Id(till)),
        )
        .await
        .unwrap();
    method_line(&mut tx, general, "inbound").await;
    method_line(&mut tx, general, "outbound").await;
    let refused = |error: Error, words: &str| {
        assert!(
            error.message().contains(words),
            "{words}: {}",
            error.message()
        );
    };

    // a negative amount fails its database check
    let negative = tx
        .savepoint(async |tx| {
            payments(tx, company)
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("journal_id", Cell::Id(bank))
                            .set_cell("amount", Cell::Decimal(num("-1"))),
                    ],
                )
                .await
                .map(|_| ())?;
            tx.flush().await
        })
        .await
        .unwrap_err();
    refused(negative, "A payment amount is never below zero.");

    // the service-owned fields are refused, given or as context defaults
    let owned = tx
        .savepoint(async |tx| {
            payments(tx, company)
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("journal_id", Cell::Id(bank))
                            .set_cell("state", text("paid")),
                    ],
                )
                .await
                .map(|_| ())
        })
        .await
        .unwrap_err();
    assert_eq!(owned.kind(), ErrorKind::UserError, "{}", owned.message());
    let defaulted = tx
        .savepoint(async |tx| {
            let records = payments(tx, company).with_context([("default_move_id", Value::Int(1))]);
            records
                .create(
                    tx,
                    vec![RawVals::new().set_cell("journal_id", Cell::Id(bank))],
                )
                .await
                .map(|_| ())
        })
        .await
        .unwrap_err();
    refused(defaulted, "default_move_id");
    let linked = tx
        .savepoint(async |tx| {
            moves(tx, company)
                .create(
                    tx,
                    vec![RawVals::new().set_cell("origin_payment_id", Cell::Id(1))],
                )
                .await
                .map(|_| ())
        })
        .await
        .unwrap_err();
    refused(linked, "payment of a journal entry");

    // no outstanding account anywhere, then the company's transfer account
    let draft = payment(&mut tx, company, bank, b.customer, "50", RawVals::new()).await;
    // the refusal names the journal as the user reads it, never its stored translations
    let error = try_call(&draft, &mut tx, "action_post").await.unwrap_err();
    assert!(
        error
            .message()
            .starts_with("No outstanding account could be found to make the payment of ")
            && error
                .message()
                .contains(" on BNK8: set one on the journal's payment method")
            && !error.message().contains('{'),
        "{}",
        error.message()
    );
    let transfer = account(&mut tx, company, "101390", "asset_current").await;
    tx.env()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("transfer_account_id", Cell::Id(transfer)),
        )
        .await
        .unwrap();
    try_call(&draft, &mut tx, "action_post").await.unwrap();
    assert_eq!(
        draft.cell(&mut tx, "outstanding_account_id").await.unwrap(),
        Cell::Id(transfer)
    );
    // validated by hand: not a bank or cash account, paid
    try_call(&draft, &mut tx, "action_validate").await.unwrap();
    assert_eq!(string(&draft, &mut tx, "state").await, "reconciled");

    // a method line of another journal
    let foreign = method_line(&mut tx, other_bank, "inbound").await;
    let wrong = payment(
        &mut tx,
        company,
        bank,
        b.customer,
        "20",
        RawVals::new().set_cell("payment_method_line_id", Cell::Id(foreign)),
    )
    .await;
    refused(
        try_call(&wrong, &mut tx, "action_post").await.unwrap_err(),
        "is not one of the journal's inbound methods",
    );
    // a draft is cancelled, cannot be confirmed, returns to draft and is confirmed
    try_call(&wrong, &mut tx, "action_cancel").await.unwrap();
    assert_eq!(string(&wrong, &mut tx, "state").await, "canceled");
    refused(
        try_call(&wrong, &mut tx, "action_post").await.unwrap_err(),
        "reset it to draft",
    );
    try_call(&wrong, &mut tx, "action_draft").await.unwrap();
    assert_eq!(string(&wrong, &mut tx, "state").await, "draft");
    refused(
        try_call(&wrong, &mut tx, "action_validate")
            .await
            .unwrap_err(),
        "must be paid",
    );
    wrong.unlink(&mut tx).await.unwrap();

    // a vendor payment on the journal's own bank account: stays paid and is matched
    let outbound = method_line(&mut tx, bank, "outbound").await;
    set_line_account(&mut tx, outbound, cash_account).await;
    let vendor = payment(
        &mut tx,
        company,
        bank,
        b.vendor,
        "80",
        RawVals::new()
            .set_cell("payment_type", text("outbound"))
            .set_cell("partner_type", text("supplier")),
    )
    .await;
    assert_eq!(decimal(&vendor, &mut tx, "amount_signed").await, num("-80"));
    try_call(&vendor, &mut tx, "action_post").await.unwrap();
    assert_eq!(string(&vendor, &mut tx, "state").await, "paid");
    assert_eq!(
        vendor.cell(&mut tx, "is_matched").await.unwrap(),
        Cell::Bool(true)
    );
    let entry = entry_of(&mut tx, &vendor).await;
    let payable = items_on(&mut tx, &[&entry], b.payable).await;
    assert_eq!(decimal(&payable, &mut tx, "debit").await, num("80"));
    let liquidity = items_on(&mut tx, &[&entry], cash_account).await;
    assert_eq!(decimal(&liquidity, &mut tx, "credit").await, num("80"));
    refused(
        try_call(&vendor, &mut tx, "action_validate")
            .await
            .unwrap_err(),
        "bank or cash account",
    );
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

/// An accountant who is not an administrator records and confirms a payment; a read-only user and
/// an accountant of another company are refused before anything is read.
#[test]
fn payments_ask_the_accounting_rights_and_stay_in_their_company() {
    big_stack(access_body);
}

async fn access_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let second = other_company(&mut tx, company).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    periods(&orm, second).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let (bank, _) = bank_journal(&mut tx, company, "BNK6").await;
    let inbound = method_line(&mut tx, bank, "inbound").await;
    let receipts = account(&mut tx, company, "101320", "asset_current").await;
    set_line_account(&mut tx, inbound, receipts).await;
    let existing = payment(&mut tx, company, bank, b.customer, "10", RawVals::new()).await;
    // the user rules ask for an administrator before other users are created
    let system = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "payment-admin", company, &[system]).await;
    let accounting = xml_id(&mut tx, "account", "group_account_user").await;
    let readonly = xml_id(&mut tx, "account", "group_account_readonly").await;
    let accountant = user(&mut tx, "payment-accountant", company, &[accounting]).await;
    let reader = user(&mut tx, "payment-reader", company, &[readonly]).await;
    let outsider = user(&mut tx, "payment-outsider", second, &[accounting]).await;
    tx.commit().await.unwrap();
    let policy = policy_orm(&db, &orm).await;

    let mut tx = policy
        .begin(erp_orm::UserId::new(accountant).unwrap())
        .await
        .unwrap();
    let mine = payment(&mut tx, company, bank, b.customer, "40", RawVals::new()).await;
    try_call(&mine, &mut tx, "action_post").await.unwrap();
    assert_eq!(string(&mine, &mut tx, "state").await, "paid");
    tx.rollback().await.unwrap();

    for who in [reader, outsider] {
        let mut tx = policy
            .begin(erp_orm::UserId::new(who).unwrap())
            .await
            .unwrap();
        let theirs = tx
            .env()
            .records("account.payment")
            .unwrap()
            .browse(existing.ids())
            .unwrap();
        let error = try_call(&theirs, &mut tx, "action_post").await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access, "{}", error.message());
        tx.rollback().await.unwrap();
    }
    let mut tx = policy
        .begin(erp_orm::UserId::new(reader).unwrap())
        .await
        .unwrap();
    let created = tx
        .savepoint(async |tx| {
            payments(tx, company)
                .create(
                    tx,
                    vec![RawVals::new().set_cell("journal_id", Cell::Id(bank))],
                )
                .await
                .map(|_| ())
        })
        .await
        .unwrap_err();
    assert_eq!(created.kind(), ErrorKind::Access, "{}", created.message());
    tx.rollback().await.unwrap();
    drop(policy);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// Two units confirming payments of one journal at once: the later fails and, run again, takes
/// the next number; the numbers never repeat.
#[test]
fn two_confirmations_in_one_journal_never_share_a_number() {
    big_stack(race_body);
}

async fn race_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let (bank, _) = bank_journal(&mut tx, company, "BNK5").await;
    let inbound = method_line(&mut tx, bank, "inbound").await;
    let receipts = account(&mut tx, company, "101330", "asset_current").await;
    set_line_account(&mut tx, inbound, receipts).await;
    // the journal's number chain of the month exists before the race
    let opened = payment(&mut tx, company, bank, b.customer, "5", RawVals::new()).await;
    try_call(&opened, &mut tx, "action_post").await.unwrap();
    let one = payment(&mut tx, company, bank, b.customer, "10", RawVals::new())
        .await
        .ids();
    let two = payment(&mut tx, company, bank, b.customer, "20", RawVals::new())
        .await
        .ids();
    tx.commit().await.unwrap();

    let confirm = async |tx: &mut Transaction, ids: &[i64]| {
        let records = payments(tx, company).browse(ids.to_vec()).unwrap();
        try_call(&records, tx, "action_post").await
    };
    let mut late = orm.begin(SUPERUSER_ID).await.unwrap();
    late.query("SELECT 1", &[]).await.unwrap();
    let mut early = orm.begin(SUPERUSER_ID).await.unwrap();
    confirm(&mut early, &one).await.unwrap();
    early.commit().await.unwrap();
    let error = confirm(&mut late, &two).await.unwrap_err();
    assert!(
        matches!(
            error.kind(),
            ErrorKind::Concurrency | ErrorKind::Conflict | ErrorKind::Database
        ),
        "{:?}: {}",
        error.kind(),
        error.message()
    );
    late.rollback().await.unwrap();
    let mut again = orm.begin(SUPERUSER_ID).await.unwrap();
    confirm(&mut again, &two).await.unwrap();
    let numbers: Vec<String> = again
        .query(
            "SELECT name::text FROM account_payment WHERE id = ANY($1) ORDER BY id",
            &[&[one[0], two[0]].to_vec()],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(numbers.len(), 2);
    assert_ne!(numbers[0], numbers[1], "{numbers:?}");
    again.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}
