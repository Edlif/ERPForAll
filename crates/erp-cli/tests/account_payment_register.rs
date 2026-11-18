// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Register Payment dialog on a full install: the Register Payment button of an invoice, what
//! the dialog proposes, a partial payment that leaves the rest due, one grouped payment for several
//! documents of a customer (a credit note netted), one payment per document when not grouped, and
//! the refusals by name.
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
use erp_sdk::serde_json::Value as Json;
use erp_sdk::{Command, ErrorKind, SUPERUSER_ID};

/// A bank journal created through the ORM, with an outstanding account on both method lines.
async fn bank_journal(tx: &mut Transaction, company: i64, code: &str) -> i64 {
    money_journal(tx, company, code, "bank").await
}

/// A bank or cash journal (`kind`), with an outstanding account on both method lines.
async fn money_journal(tx: &mut Transaction, company: i64, code: &str, kind: &str) -> i64 {
    let cash = account(tx, company, &format!("10{code}"), "asset_cash").await;
    let outstanding = account(tx, company, &format!("11{code}"), "asset_current").await;
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
                    .set_cell("type", text(kind))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("default_account_id", Cell::Id(cash)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    let lines = tx
        .env()
        .records("account.journal")
        .unwrap()
        .browse([id])
        .unwrap()
        .related(tx, "payment_method_line_ids")
        .await
        .unwrap();
    assert_eq!(
        lines.len(),
        2,
        "a new money journal has its two manual lines"
    );
    lines
        .write(
            tx,
            RawVals::new().set_cell("payment_account_id", Cell::Id(outstanding)),
        )
        .await
        .unwrap();
    id
}

/// A dialog on `docs`, dated in the books' year.
async fn dialog(tx: &mut Transaction, company: i64, docs: &[&Records], extra: RawVals) -> Records {
    let ids: Vec<RecId> = docs.iter().flat_map(|d| d.ids()).map(rec).collect();
    let mut vals = extra.set_commands("invoice_ids", vec![Command::set(ids)]);
    if !vals.contains("payment_date") {
        vals.insert("payment_date".into(), Cell::Date(d(2024, 3, 1)));
    }
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.payment.register")
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
}

/// The action dictionary a button answers.
fn json_of(value: Value) -> Json {
    match value {
        Value::Json(json) => (*json).clone(),
        other => panic!("an action is a JSON dictionary, not {other:?}"),
    }
}

async fn pay(dialog: &Records, tx: &mut Transaction) -> Result<Json> {
    let dialog = dialog.clone();
    tx.savepoint(async move |tx| {
        dialog
            .call::<(), Value>(tx, "action_create_payments", ())
            .await
    })
    .await
    .map(json_of)
}

fn payments(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.payment")
        .unwrap()
}

async fn all_payments(tx: &mut Transaction, company: i64) -> Records {
    payments(tx, company)
        .search(
            tx,
            &Domain::and([Domain::condition("company_id", Operator::Eq, company).unwrap()]),
        )
        .await
        .unwrap()
}

/// the direction; a preferred method line of the partner comes later)
#[test]
fn registering_pays_in_part_then_groups_the_rest_with_a_credit_note() {
    big_stack(grouped_body);
}

async fn grouped_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let bank = bank_journal(&mut tx, company, "BNK9").await;
    let first = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "1000",
        d(2024, 1, 10),
    )
    .await;

    // the button opens the dialog on the invoice
    let action = json_of(
        first
            .call::<(), Value>(&mut tx, "action_register_payment", ())
            .await
            .unwrap(),
    );
    assert_eq!(action["res_model"], "account.payment.register");
    assert_eq!(action["target"], "new");
    assert_eq!(
        action["context"]["default_invoice_ids"],
        erp_sdk::serde_json::json!([[6, 0, [first.ids()[0]]]])
    );

    let part = dialog(&mut tx, company, &[&first], RawVals::new()).await;
    assert_eq!(decimal(&part, &mut tx, "amount_due").await, num("1000"));
    assert_eq!(decimal(&part, &mut tx, "amount").await, num("1000"));
    assert_eq!(
        part.cell(&mut tx, "journal_id").await.unwrap(),
        Cell::Id(bank)
    );
    assert_eq!(string(&part, &mut tx, "payment_type").await, "inbound");
    let number = string(&first, &mut tx, "name").await;
    assert_eq!(string(&part, &mut tx, "communication").await, number);
    part.write(
        &mut tx,
        RawVals::new().set_cell("amount", Cell::Decimal(num("400"))),
    )
    .await
    .unwrap();
    let done = pay(&part, &mut tx).await.unwrap();
    assert_eq!(done["tag"], "display_notification");
    assert_eq!(string(&first, &mut tx, "payment_state").await, "partial");
    assert_eq!(
        decimal(&first, &mut tx, "amount_residual").await,
        num("600")
    );
    let made = all_payments(&mut tx, company).await;
    assert_eq!(made.len(), 1);
    assert_eq!(decimal(&made, &mut tx, "amount").await, num("400"));
    assert_eq!(string(&made, &mut tx, "memo").await, number);
    assert_eq!(
        made.cell(&mut tx, "journal_id").await.unwrap(),
        Cell::Id(bank)
    );

    // the rest, a second invoice and a credit note: one payment of what remains, netted
    let second = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "500",
        d(2024, 1, 20),
    )
    .await;
    let refund = posted_invoice(&mut tx, &b, "out_refund", b.customer, "200", d(2024, 1, 25)).await;
    let rest = dialog(
        &mut tx,
        company,
        &[&first, &second, &refund],
        RawVals::new(),
    )
    .await;
    assert_eq!(decimal(&rest, &mut tx, "amount_due").await, num("900"));
    pay(&rest, &mut tx).await.unwrap();
    for doc in [&first, &second, &refund] {
        assert_eq!(string(doc, &mut tx, "payment_state").await, "paid");
        assert_eq!(decimal(doc, &mut tx, "amount_residual").await, num("0"));
    }
    let made = all_payments(&mut tx, company).await;
    assert_eq!(made.len(), 2);
    let grouped = made
        .browse([made.ids().into_iter().max().unwrap()])
        .unwrap();
    assert_eq!(decimal(&grouped, &mut tx, "amount").await, num("900"));
    assert_ne!(string(&grouped, &mut tx, "state").await, "draft");
    let mut names = [
        number.clone(),
        string(&second, &mut tx, "name").await,
        string(&refund, &mut tx, "name").await,
    ];
    names.sort();
    assert_eq!(string(&grouped, &mut tx, "memo").await, names.join(", "));
    assert_eq!(
        grouped.cell(&mut tx, "is_reconciled").await.unwrap(),
        Cell::Bool(true)
    );
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn without_grouping_every_bill_gets_its_own_payment() {
    big_stack(separate_body);
}

async fn separate_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    bank_journal(&mut tx, company, "BNK8").await;
    let one = posted_invoice(&mut tx, &b, "in_invoice", b.vendor, "1000", d(2024, 1, 10)).await;
    let two = posted_invoice(&mut tx, &b, "in_invoice", b.vendor, "2000", d(2024, 1, 11)).await;
    let wizard = dialog(
        &mut tx,
        company,
        &[&one, &two],
        RawVals::new().set_cell("group_payment", Cell::Bool(false)),
    )
    .await;
    assert_eq!(string(&wizard, &mut tx, "payment_type").await, "outbound");
    // two payments: a changed amount is refused, not dropped
    wizard
        .write(
            &mut tx,
            RawVals::new().set_cell("amount", Cell::Decimal(num("1200"))),
        )
        .await
        .unwrap();
    let error = pay(&wizard, &mut tx).await.unwrap_err();
    assert!(
        error
            .message()
            .contains("2 payments would be made (one per document)"),
        "{}",
        error.message()
    );
    wizard
        .write(
            &mut tx,
            RawVals::new().set_cell("amount", Cell::Decimal(num("3000"))),
        )
        .await
        .unwrap();
    pay(&wizard, &mut tx).await.unwrap();
    let made = all_payments(&mut tx, company).await;
    assert_eq!(made.len(), 2);
    let mut amounts: Vec<Decimal> = Vec::new();
    for id in made.ids() {
        let p = made.browse([id]).unwrap();
        assert_eq!(string(&p, &mut tx, "payment_type").await, "outbound");
        assert_eq!(string(&p, &mut tx, "partner_type").await, "supplier");
        amounts.push(decimal(&p, &mut tx, "amount").await);
    }
    amounts.sort();
    assert_eq!(amounts, vec![num("1000"), num("2000")]);
    for doc in [&one, &two] {
        assert_eq!(string(doc, &mut tx, "payment_state").await, "paid");
    }
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn what_cannot_be_registered_is_refused_by_name() {
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
    bank_journal(&mut tx, company, "BNK7").await;
    let invoice = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "100",
        d(2024, 1, 10),
    )
    .await;
    let bill = posted_invoice(&mut tx, &b, "in_invoice", b.vendor, "100", d(2024, 1, 10)).await;

    let refused = |error: Error, expected: &str| {
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        assert!(
            error.message().contains(expected),
            "expected {expected:?} in {:?}",
            error.message()
        );
    };

    let mixed = dialog(&mut tx, company, &[&invoice, &bill], RawVals::new()).await;
    refused(
        pay(&mixed, &mut tx).await.unwrap_err(),
        "Customer documents and vendor documents",
    );

    // the field's domain keeps blocked and paid documents out of a new dialog; a dialog opened
    // before the document changed is refused when it pays
    let blocked = dialog(&mut tx, company, &[&invoice], RawVals::new()).await;
    let full = dialog(&mut tx, company, &[&invoice], RawVals::new()).await;
    let again = dialog(&mut tx, company, &[&invoice], RawVals::new()).await;
    tx.execute(
        "UPDATE account_move SET payment_state = 'blocked' WHERE id = $1",
        &[&invoice.ids()[0]],
    )
    .await
    .unwrap();
    tx.invalidate_model("account.move", None).await.unwrap();
    refused(
        pay(&blocked, &mut tx).await.unwrap_err(),
        "blocked for payment",
    );
    tx.execute(
        "UPDATE account_move SET payment_state = 'not_paid' WHERE id = $1",
        &[&invoice.ids()[0]],
    )
    .await
    .unwrap();
    tx.invalidate_model("account.move", None).await.unwrap();
    pay(&full, &mut tx).await.unwrap();
    refused(
        pay(&again, &mut tx).await.unwrap_err(),
        "nothing left to pay",
    );

    let zero = dialog(&mut tx, company, &[&bill], RawVals::new()).await;
    zero.write(
        &mut tx,
        RawVals::new().set_cell("amount", Cell::Decimal(num("0"))),
    )
    .await
    .unwrap();
    refused(pay(&zero, &mut tx).await.unwrap_err(), "more than zero");

    let draft = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("move_type", text("out_invoice"))
                    .set_cell("partner_id", Cell::Id(b.customer)),
            ],
        )
        .await
        .unwrap();
    let error = tx
        .savepoint(async |tx| {
            draft
                .call::<(), Value>(tx, "action_register_payment", ())
                .await
        })
        .await
        .unwrap_err();
    refused(error, "is not confirmed");
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

/// A dialog is its creator's: another accountant, an accountant of another company and a
/// read-only accounting user can neither read, change nor pay it; documents outside the caller's
/// companies cannot be put in a dialog.
#[test]
fn a_dialog_is_its_creators_and_of_its_documents_company() {
    big_stack(access_body);
}

async fn access_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let second = other_company(&mut tx, company).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    bank_journal(&mut tx, company, "BNK5").await;
    let invoice = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "100",
        d(2024, 1, 10),
    )
    .await;
    // the user rules ask for an administrator before other users are created
    let system = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "register-admin", company, &[system]).await;
    let accounting = xml_id(&mut tx, "account", "group_account_user").await;
    let readonly = xml_id(&mut tx, "account", "group_account_readonly").await;
    let owner = user(&mut tx, "register-owner", company, &[accounting]).await;
    let colleague = user(&mut tx, "register-colleague", company, &[accounting]).await;
    let reader = user(&mut tx, "register-reader", company, &[readonly]).await;
    let outsider = user(&mut tx, "register-outsider", second, &[accounting]).await;
    tx.commit().await.unwrap();
    let policy = policy_orm(&db, &orm).await;
    let begin = async |who: i64| {
        policy
            .begin(erp_orm::UserId::new(who).unwrap())
            .await
            .unwrap()
    };

    let mut tx = begin(owner).await;
    let mine = dialog(&mut tx, company, &[&invoice], RawVals::new()).await;
    tx.commit().await.unwrap();

    for who in [colleague, outsider, reader] {
        let mut tx = begin(who).await;
        let theirs = tx
            .env()
            .records("account.payment.register")
            .unwrap()
            .browse(mine.ids())
            .unwrap();
        let denied = |error: Error| {
            assert_eq!(
                error.kind(),
                ErrorKind::Access,
                "{who}: {}",
                error.message()
            );
        };
        let read = tx
            .savepoint(async |tx| theirs.cell(tx, "amount").await.map(|_| ()))
            .await;
        denied(read.unwrap_err());
        let written = tx
            .savepoint(async |tx| {
                theirs
                    .write(
                        tx,
                        RawVals::new().set_cell("amount", Cell::Decimal(num("1"))),
                    )
                    .await
            })
            .await;
        denied(written.unwrap_err());
        denied(pay(&theirs, &mut tx).await.unwrap_err());
        tx.rollback().await.unwrap();
    }

    // documents of a company the caller is not in cannot be put in a dialog
    let mut tx = begin(outsider).await;
    let foreign = tx
        .savepoint(async |tx| {
            let ids = vec![rec(invoice.ids()[0])];
            tx.env()
                .records("account.payment.register")
                .unwrap()
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_commands("invoice_ids", vec![Command::set(ids)])
                            .set_cell("payment_date", Cell::Date(d(2024, 3, 1))),
                    ],
                )
                .await
                .map(|_| ())
        })
        .await
        .unwrap_err();
    assert_eq!(foreign.kind(), ErrorKind::Access, "{}", foreign.message());
    tx.rollback().await.unwrap();

    // the owner pays it
    let mut tx = begin(owner).await;
    let mine = tx
        .env()
        .records("account.payment.register")
        .unwrap()
        .browse(mine.ids())
        .unwrap();
    pay(&mine, &mut tx).await.unwrap();
    let invoice = moves(&tx, company).browse(invoice.ids()).unwrap();
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "paid");
    tx.rollback().await.unwrap();
    drop(policy);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// A second inbound method line on `journal`, on its own outstanding account.
async fn second_inbound_line(tx: &mut Transaction, company: i64, journal: i64) -> i64 {
    let method = tx
        .env()
        .records("account.payment.method")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Cheque"))
                    .set_cell("code", text("cheque_test"))
                    .set_cell("payment_type", text("inbound")),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    let outstanding = account(tx, company, "113000", "asset_current").await;
    tx.env()
        .records("account.payment.method.line")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Cheque"))
                    .set_cell("journal_id", Cell::Id(journal))
                    .set_cell("payment_method_id", Cell::Id(method))
                    .set_cell("payment_account_id", Cell::Id(outstanding)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// A dialog pays what it showed, once, with the method chosen in it: a changed amount over
/// several payments, a document paid in part since it opened, a method of the other direction
/// and a journal of another company are refused by name; a second Pay finds it used.
#[test]
fn a_dialog_pays_what_it_shows_once_with_the_chosen_method() {
    big_stack(once_body);
}

async fn once_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let second = other_company(&mut tx, company).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let bank = bank_journal(&mut tx, company, "BNK4").await;
    let foreign_bank = bank_journal(&mut tx, second, "BNK3").await;
    let cheque = second_inbound_line(&mut tx, company, bank).await;
    let other = partner(&mut tx, "Second customer").await;
    let refused = |error: Error, expected: &str| {
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        assert!(
            error.message().contains(expected),
            "expected {expected:?} in {:?}",
            error.message()
        );
    };
    let set = async |tx: &mut Transaction, wizard: &Records, vals: RawVals| {
        wizard.write(tx, vals).await.unwrap();
    };

    // two partners: two payments, the amount cannot be changed, both take the chosen method
    let a = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "100",
        d(2024, 1, 10),
    )
    .await;
    let c = posted_invoice(&mut tx, &b, "out_invoice", other, "50", d(2024, 1, 10)).await;
    let two = dialog(&mut tx, company, &[&a, &c], RawVals::new()).await;
    assert_eq!(
        two.cell(&mut tx, "single_payment").await.unwrap(),
        Cell::Bool(false)
    );
    set(
        &mut tx,
        &two,
        RawVals::new().set_cell("amount", Cell::Decimal(num("60"))),
    )
    .await;
    refused(
        pay(&two, &mut tx).await.unwrap_err(),
        "The amount was changed, but 2 payments would be made",
    );
    set(
        &mut tx,
        &two,
        RawVals::new()
            .set_cell("amount", Cell::Decimal(num("150")))
            .set_cell("payment_method_line_id", Cell::Id(cheque)),
    )
    .await;
    pay(&two, &mut tx).await.unwrap();
    let made = all_payments(&mut tx, company).await;
    assert_eq!(made.len(), 2);
    for id in made.ids() {
        let p = made.browse([id]).unwrap();
        assert_eq!(
            p.cell(&mut tx, "payment_method_line_id").await.unwrap(),
            Cell::Id(cheque)
        );
    }
    // a second Pay of the same dialog registers nothing
    let replay = pay(&two, &mut tx).await.unwrap_err();
    assert_eq!(replay.kind(), ErrorKind::Missing, "{}", replay.message());
    assert!(
        replay.message().contains("already used"),
        "{}",
        replay.message()
    );
    assert_eq!(all_payments(&mut tx, company).await.len(), 2);

    // two dialogs on one invoice: the second, opened before the first paid part, is refused
    let big = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "300",
        d(2024, 1, 12),
    )
    .await;
    let first = dialog(&mut tx, company, &[&big], RawVals::new()).await;
    let late = dialog(&mut tx, company, &[&big], RawVals::new()).await;
    assert_eq!(
        late.cell(&mut tx, "single_payment").await.unwrap(),
        Cell::Bool(true)
    );
    set(
        &mut tx,
        &first,
        RawVals::new().set_cell("amount", Cell::Decimal(num("100"))),
    )
    .await;
    pay(&first, &mut tx).await.unwrap();
    refused(
        pay(&late, &mut tx).await.unwrap_err(),
        "changed since the dialog was opened",
    );
    assert_eq!(decimal(&big, &mut tx, "amount_residual").await, num("200"));

    // a receiving method for a bill, a journal of another company
    let bill = posted_invoice(&mut tx, &b, "in_invoice", b.vendor, "70", d(2024, 1, 10)).await;
    let wrong_way = dialog(&mut tx, company, &[&bill], RawVals::new()).await;
    // the proposals are computed before the user changes them, as the form does
    assert_eq!(
        string(&wrong_way, &mut tx, "payment_type").await,
        "outbound"
    );
    wrong_way
        .cell(&mut tx, "payment_method_line_id")
        .await
        .unwrap();
    set(
        &mut tx,
        &wrong_way,
        RawVals::new().set_cell("payment_method_line_id", Cell::Id(cheque)),
    )
    .await;
    refused(
        pay(&wrong_way, &mut tx).await.unwrap_err(),
        "The payment method Cheque receives money, but the payment of",
    );
    let elsewhere = dialog(&mut tx, company, &[&bill], RawVals::new()).await;
    assert_eq!(
        elsewhere.cell(&mut tx, "journal_id").await.unwrap(),
        Cell::Id(bank)
    );
    set(
        &mut tx,
        &elsewhere,
        RawVals::new().set_cell("journal_id", Cell::Id(foreign_bank)),
    )
    .await;
    refused(
        pay(&elsewhere, &mut tx).await.unwrap_err(),
        "The journal BNK3 belongs to another company than the documents",
    );
    assert_eq!(all_payments(&mut tx, company).await.len(), 3);
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

/// An invoice whose first instalment was matched by hand with a payment that keeps a rest: the
/// dialog pays the instalment still open and leaves the matched one out.
#[test]
fn a_registration_skips_an_instalment_matched_by_hand() {
    big_stack(instalment_body);
}

async fn instalment_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    bank_journal(&mut tx, company, "BNK2").await;
    // 30 % now, the rest in 60 days
    let term = xml_id(&mut tx, "account", "account_payment_term_advance_60days").await;
    let line = Command::create(
        RawVals::new()
            .set_cell("name", text("Service"))
            .set_cell("quantity", Cell::Decimal(num("1")))
            .set_cell("price_unit", Cell::Decimal(num("1000"))),
    );
    let invoice = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("move_type", text("out_invoice"))
                    .set_cell("partner_id", Cell::Id(b.customer))
                    .set_cell("invoice_date", Cell::Date(d(2024, 3, 1)))
                    .set_cell("date", Cell::Date(d(2024, 3, 1)))
                    .set_cell("invoice_payment_term_id", Cell::Id(term))
                    .set_commands("invoice_line_ids", vec![line]),
            ],
        )
        .await
        .unwrap();
    invoice
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap();
    let first: i64 = tx
        .query(
            "SELECT id::int8 FROM account_move_line WHERE move_id = $1 \
             AND display_type = 'payment_term' ORDER BY date_maturity, id LIMIT 1",
            &[&invoice.ids()[0]],
        )
        .await
        .unwrap()[0]
        .get(0);
    let received = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-500",
        d(2024, 3, 2),
    )
    .await;
    let mut by_hand = items_on(&mut tx, &[&received], b.receivable).await.ids();
    by_hand.push(first);
    let by_hand = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .browse(by_hand)
        .unwrap();
    try_call(&by_hand, &mut tx, "reconcile").await.unwrap();
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("700")
    );

    let wizard = dialog(&mut tx, company, &[&invoice], RawVals::new()).await;
    assert_eq!(decimal(&wizard, &mut tx, "amount_due").await, num("700"));
    pay(&wizard, &mut tx).await.unwrap();
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("0")
    );
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "paid");
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

struct Host;
impl erp_rpc::RpcHost for Host {
    fn language_active(&self, code: &str) -> Result<bool> {
        Ok(code == "en_US")
    }
    fn has_capability(&self, _: &erp_orm::Principal, _: &erp_orm::Capability) -> Result<bool> {
        Ok(false)
    }
}

/// One JSON-2 call as the web client sends it; the status and the parsed body.
async fn json2(
    service: &erp_rpc::RpcService,
    principal: &erp_orm::Principal,
    path: &str,
    args: Json,
) -> (u16, Json) {
    let response = service
        .call(erp_rpc::RpcCall::json2(
            &format!("/json/2/{path}"),
            erp_sdk::serde_json::to_vec(&args).unwrap(),
            erp_rpc::Caller::Principal(principal.clone()),
        ))
        .await;
    let body = erp_sdk::serde_json::from_slice(response.body())
        .unwrap_or_else(|_| Json::String(String::from_utf8_lossy(response.body()).into_owned()));
    (response.status(), body)
}

async fn json2_ok(
    service: &erp_rpc::RpcService,
    principal: &erp_orm::Principal,
    path: &str,
    args: Json,
) -> Json {
    let (status, body) = json2(service, principal, path, args).await;
    assert_eq!(status, 200, "{path}: {body}");
    body
}

/// A many2one as an onchange answers it: `{id, display_name}` or a bare id.
fn id_of(value: &Json) -> i64 {
    value
        .get("id")
        .unwrap_or(value)
        .as_i64()
        .unwrap_or_else(|| panic!("a record id, not {value}"))
}

/// What the dialog must show for some documents.
struct Expected {
    due: f64,
    partner: i64,
    payment_type: &'static str,
    /// What the user enters before Pay; `None`: the dialog is saved as it was shown.
    entered: Option<Entered>,
}

/// The amount, journal (its first method line of the direction is chosen) and memo a user enters.
struct Entered {
    amount: f64,
    journal: i64,
    memo: &'static str,
}

/// The first method line of `journal` for `direction`, as the dialog's compute picks it.
async fn line_of(tx: &mut Transaction, journal: i64, direction: &str) -> i64 {
    tx.query(
        "SELECT id::int8 FROM account_payment_method_line WHERE journal_id = $1 \
         AND payment_type = $2 ORDER BY id LIMIT 1",
        &[&journal, &direction],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// A company's id and currency for `doc`'s company.
async fn company_of(tx: &mut Transaction, doc: i64) -> (i64, i64) {
    let row = &tx
        .query(
            "SELECT c.id::int8, c.currency_id::int8 FROM res_company c JOIN account_move m \
             ON m.company_id = c.id WHERE m.id = $1",
            &[&doc],
        )
        .await
        .unwrap()[0];
    (row.get(0), row.get(1))
}

/// The dialog's first onchange as the client sends it on a new record with `context`.
fn first_onchange(context: &Json) -> Json {
    let relation = erp_sdk::serde_json::json!({"fields": {"display_name": {}}});
    erp_sdk::serde_json::json!({
        "ids": [], "values": {}, "field_names": [], "context": context,
        "fields_spec": {
            "journal_id": relation, "payment_method_line_id": relation, "partner_id": relation,
            "group_payment": {}, "amount_due": {}, "amount": {}, "currency_id": relation,
            "payment_type": {}, "company_id": relation, "payment_date": {}, "communication": {},
            "single_payment": {}, "invoice_ids": relation,
        },
    })
}

/// The Register Payment button pressed on `docs` the way the web client presses it: the button's
/// action, normalized; the first onchange of a new dialog with the action's context and no id; an
/// onchange of the amount; the dialog saved with what the user entered (or what it showed); Pay;
/// the answer normalized. The documents are paid, in part when a smaller amount was entered.
async fn client_pays(
    service: &erp_rpc::RpcService,
    principal: &erp_orm::Principal,
    orm: &erp_sdk::Orm,
    docs: &[i64],
    bank: i64,
    expected: Expected,
) {
    use erp_sdk::serde_json::json;
    let opened = json2_ok(
        service,
        principal,
        "account.move/action_register_payment",
        json!({"ids": docs}),
    )
    .await;
    let normalized = json2_ok(
        service,
        principal,
        "account.payment.register/erp_normalize_action_result",
        json!({"wire_major": 1, "result": opened}),
    )
    .await;
    let window = &normalized["result"];
    assert_eq!(window["kind"], "window", "{normalized}");
    assert_eq!(window["model"], "account.payment.register", "{normalized}");
    assert_eq!(window["res_id"], Json::Null, "a new dialog: {normalized}");
    let context = window["context"].clone();
    assert_eq!(
        context["default_invoice_ids"],
        json!([[6, 0, docs]]),
        "{normalized}"
    );

    let (status, shown) = json2(
        service,
        principal,
        "account.payment.register/onchange",
        first_onchange(&context),
    )
    .await;
    assert_eq!(status, 200, "the dialog's first onchange: {shown}");
    let value = &shown["value"];
    assert_eq!(value["amount_due"].as_f64(), Some(expected.due), "{shown}");
    assert_eq!(value["amount"].as_f64(), Some(expected.due), "{shown}");
    assert_eq!(id_of(&value["partner_id"]), expected.partner, "{shown}");
    assert_eq!(value["payment_type"], expected.payment_type, "{shown}");
    assert_eq!(id_of(&value["journal_id"]), bank, "{shown}");
    assert_eq!(value["single_payment"], true, "{shown}");
    let line = id_of(&value["payment_method_line_id"]);
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let row = tx
        .query(
            "SELECT journal_id::int8, payment_type::text FROM account_payment_method_line \
             WHERE id = $1",
            &[&line],
        )
        .await
        .unwrap();
    assert_eq!(
        row[0].get::<_, i64>(0),
        bank,
        "the method line of the journal"
    );
    assert_eq!(row[0].get::<_, String>(1), expected.payment_type);
    let (company, currency) = company_of(&mut tx, docs[0]).await;
    assert_eq!(id_of(&value["company_id"]), company, "{shown}");
    assert_eq!(id_of(&value["currency_id"]), currency, "{shown}");
    // what the user enters, else what the dialog showed
    let (amount, journal, memo) = match &expected.entered {
        Some(e) => (json!(e.amount), e.journal, json!(e.memo)),
        None => (
            value["amount"].clone(),
            bank,
            value["communication"].clone(),
        ),
    };
    let line = if journal == bank {
        line
    } else {
        line_of(&mut tx, journal, expected.payment_type).await
    };
    tx.rollback().await.unwrap();

    // the client's onchange when the user edits the amount: the whole form, one changed field
    let mut form = value.as_object().unwrap().clone();
    for name in [
        "journal_id",
        "payment_method_line_id",
        "partner_id",
        "currency_id",
        "company_id",
    ] {
        form.insert(name.to_owned(), json!(id_of(&value[name])));
    }
    form.insert("invoice_ids".to_owned(), json!([[6, 0, docs]]));
    form.insert("amount".to_owned(), amount.clone());
    let (status, edited) = json2(
        service,
        principal,
        "account.payment.register/onchange",
        json!({"ids": [], "values": form, "field_names": ["amount"],
               "fields_spec": first_onchange(&context)["fields_spec"], "context": context}),
    )
    .await;
    assert_eq!(status, 200, "the onchange of the amount: {edited}");
    if let Some(answered) = edited["value"].get("amount") {
        assert_eq!(answered, &amount, "the edited amount stays: {edited}");
    }

    // the form saves (writable fields; the payment date moved into the open year)
    let saved = json2_ok(
        service,
        principal,
        "account.payment.register/web_save",
        json!({
            "ids": [],
            "vals": {
                "journal_id": journal, "payment_method_line_id": line,
                "group_payment": value["group_payment"], "amount": amount,
                "payment_date": "2024-03-01", "communication": memo,
                "invoice_ids": [[6, 0, docs]],
            },
            "specification": {"display_name": {}},
            "context": context,
        }),
    )
    .await;
    let wizard = saved[0]["id"].as_i64().unwrap_or_else(|| panic!("{saved}"));
    // the saved row holds what was entered, and the documents' company and currency
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let row = &tx
        .query(
            "SELECT company_id::int8, currency_id::int8, amount::float8, communication::text, \
             journal_id::int8, payment_method_line_id::int8 FROM account_payment_register \
             WHERE id = $1",
            &[&wizard],
        )
        .await
        .unwrap()[0];
    let stored = (
        row.get::<_, i64>(0),
        row.get::<_, i64>(1),
        json!(row.get::<_, f64>(2)),
        json!(row.get::<_, String>(3)),
        row.get::<_, i64>(4),
        row.get::<_, i64>(5),
    );
    assert_eq!(
        stored,
        (
            company,
            currency,
            json!(amount.as_f64().unwrap()),
            memo.clone(),
            journal,
            line
        ),
        "the saved dialog row"
    );
    tx.rollback().await.unwrap();
    let done = json2_ok(
        service,
        principal,
        "account.payment.register/action_create_payments",
        json!({"ids": [wizard], "context": context}),
    )
    .await;
    // what Pay answers ends the dialog with a message: the client closes it and refreshes the
    // document; it never reads the dialog row again (Pay deleted it)
    let ended = json2_ok(
        service,
        principal,
        "account.payment.register/erp_normalize_action_result",
        json!({"wire_major": 1, "result": done}),
    )
    .await;
    assert_eq!(ended["result"]["kind"], "notification", "{ended}");
    assert_eq!(ended["result"]["then_close"], true, "{ended}");
    assert_eq!(ended["result"]["level"], "success", "{ended}");

    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let states: Vec<String> = tx
        .query(
            "SELECT payment_state::text FROM account_move WHERE id = ANY($1) ORDER BY id",
            &[&docs],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    let paid = match &expected.entered {
        Some(e) if e.amount < expected.due => "partial",
        _ => "paid",
    };
    assert_eq!(states, vec![paid.to_owned(); docs.len()], "{docs:?}");
    let payment = &tx
        .query(
            "SELECT amount::float8, journal_id::int8, payment_method_line_id::int8, memo::text \
             FROM account_payment ORDER BY id DESC LIMIT 1",
            &[],
        )
        .await
        .unwrap()[0];
    assert_eq!(
        (
            json!(payment.get::<_, f64>(0)),
            payment.get::<_, i64>(1),
            payment.get::<_, i64>(2),
            json!(payment.get::<_, String>(3)),
        ),
        (json!(amount.as_f64().unwrap()), journal, line, memo),
        "the registered payment"
    );
    let gone = tx
        .query(
            "SELECT 1 FROM account_payment_register WHERE id = $1",
            &[&wizard],
        )
        .await
        .unwrap();
    assert!(gone.is_empty(), "a paid dialog is deleted");
    tx.rollback().await.unwrap();
}

/// on a new record whose documents come from the button's context)
/// through the client's calls)
#[test]
fn the_dialog_opens_and_pays_as_the_web_client_calls_it() {
    big_stack(client_body);
}

async fn client_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let bank = bank_journal(&mut tx, company, "BNK6").await;
    let cash = money_journal(&mut tx, company, "CSH6", "cash").await;
    let one = |r: Records| r.ids()[0];
    let partly = one(posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "1000",
        d(2024, 1, 16),
    )
    .await);
    let single = one(posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "1000",
        d(2024, 1, 10),
    )
    .await);
    let first = one(posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "300",
        d(2024, 1, 11),
    )
    .await);
    let second = one(posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "200",
        d(2024, 1, 12),
    )
    .await);
    let invoice = one(posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "1000",
        d(2024, 1, 13),
    )
    .await);
    let refund =
        one(posted_invoice(&mut tx, &b, "out_refund", b.customer, "200", d(2024, 1, 14)).await);
    let bill =
        one(posted_invoice(&mut tx, &b, "in_invoice", b.vendor, "700", d(2024, 1, 15)).await);
    let system = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "client-pay-admin", company, &[system]).await;
    let accounting = xml_id(&mut tx, "account", "group_account_user").await;
    let accountant = user(&mut tx, "client-pay-accountant", company, &[accounting]).await;
    tx.commit().await.unwrap();

    let policy = policy_orm(&db, &orm).await;
    // JSON-2 records audited calls; without the audit schema a write answers 503
    let mut audit = policy
        .begin_opts(
            erp_orm::BeginOptions::new(SUPERUSER_ID)
                .isolation(erp_orm::Isolation::ReadCommitted)
                .on_primary(),
        )
        .await
        .unwrap();
    audit.install_audit_schema().await.unwrap();
    audit.commit().await.unwrap();
    let mut tx = policy
        .begin(erp_orm::UserId::new(accountant).unwrap())
        .await
        .unwrap();
    let principal = tx.env().principal().clone();
    tx.rollback().await.unwrap();
    let service = erp_rpc::RpcService::new(policy.clone(), Host, erp_rpc::RpcConfig::default());
    let customer = |due| Expected {
        due,
        partner: b.customer,
        payment_type: "inbound",
        entered: None,
    };
    client_pays(
        &service,
        &principal,
        &policy,
        &[single],
        bank,
        customer(1000.0),
    )
    .await;
    client_pays(
        &service,
        &principal,
        &policy,
        &[first, second],
        bank,
        customer(500.0),
    )
    .await;
    client_pays(
        &service,
        &principal,
        &policy,
        &[invoice, refund],
        bank,
        customer(800.0),
    )
    .await;
    let vendor = Expected {
        due: 700.0,
        partner: b.vendor,
        payment_type: "outbound",
        entered: None,
    };
    client_pays(&service, &principal, &policy, &[bill], bank, vendor).await;
    // the user pays 400 of 1,000 in cash with a memo of their own
    let entered = Expected {
        entered: Some(Entered {
            amount: 400.0,
            journal: cash,
            memo: "Deposit at the counter",
        }),
        ..customer(1000.0)
    };
    client_pays(&service, &principal, &policy, &[partly], bank, entered).await;
    let mut tx = policy.begin(SUPERUSER_ID).await.unwrap();
    let residual: f64 = tx
        .query(
            "SELECT amount_residual::float8 FROM account_move WHERE id = $1",
            &[&partly],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(residual, 600.0, "the rest stays due");
    tx.rollback().await.unwrap();
    drop(service);
    drop(policy);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// A dialog opened by an accountant of one company on a document of another company the caller
/// cannot read: the onchange is refused as an access error that names the model, not the
/// document; saving such a dialog is refused the same way.
#[test]
fn a_dialog_on_a_document_of_another_company_shows_nothing_of_it() {
    big_stack(foreign_body);
}

async fn foreign_body() {
    use erp_sdk::serde_json::json;
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let second = other_company(&mut tx, company).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    periods(&orm, second).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    bank_journal(&mut tx, company, "BNK1").await;
    let theirs = books(&mut tx, second).await;
    let foreign = posted_invoice(
        &mut tx,
        &theirs,
        "out_invoice",
        theirs.customer,
        "900",
        d(2024, 1, 10),
    )
    .await
    .ids()[0];
    let name = string(
        &moves(&tx, second).browse([foreign]).unwrap(),
        &mut tx,
        "name",
    )
    .await;
    let system = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "foreign-pay-admin", company, &[system]).await;
    let accounting = xml_id(&mut tx, "account", "group_account_user").await;
    let accountant = user(&mut tx, "foreign-pay-accountant", company, &[accounting]).await;
    tx.commit().await.unwrap();

    let policy = policy_orm(&db, &orm).await;
    let mut audit = policy
        .begin_opts(
            erp_orm::BeginOptions::new(SUPERUSER_ID)
                .isolation(erp_orm::Isolation::ReadCommitted)
                .on_primary(),
        )
        .await
        .unwrap();
    audit.install_audit_schema().await.unwrap();
    audit.commit().await.unwrap();
    let mut tx = policy
        .begin(erp_orm::UserId::new(accountant).unwrap())
        .await
        .unwrap();
    let principal = tx.env().principal().clone();
    tx.rollback().await.unwrap();
    let service = erp_rpc::RpcService::new(policy.clone(), Host, erp_rpc::RpcConfig::default());
    let context = json!({"default_invoice_ids": [[6, 0, [foreign]]]});
    let (status, shown) = json2(
        &service,
        &principal,
        "account.payment.register/onchange",
        first_onchange(&context),
    )
    .await;
    assert_eq!(status, 403, "{shown}");
    assert_eq!(shown["name"], "erp.exceptions.AccessError", "{shown}");
    assert!(
        !shown.to_string().contains(&name),
        "the answer names the other company's document: {shown}"
    );
    let (status, refused) = json2(
        &service,
        &principal,
        "account.payment.register/web_save",
        json!({
            "ids": [],
            "vals": {"payment_date": "2024-03-01", "invoice_ids": [[6, 0, [foreign]]]},
            "specification": {"display_name": {}},
            "context": context,
        }),
    )
    .await;
    assert_ne!(
        status, 200,
        "a dialog on another company's document: {refused}"
    );
    assert!(
        !refused.to_string().contains(&name),
        "the refusal names the other company's document: {refused}"
    );
    drop(service);
    drop(policy);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// `hide_payment_journal_id`; the read by an employee without journal access is not covered, the
/// harness has no restricted user here and the compute reads the journal with elevated rights)
#[test]
fn a_method_line_is_named_with_its_journal_unless_the_journal_is_hidden() {
    big_stack(line_name_body);
}

async fn line_name_body() {
    let (_db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let bank = bank_journal(&mut tx, company, "BNK7").await;
    let line = line_of(&mut tx, bank, "inbound").await;
    let lines = tx
        .env()
        .records("account.payment.method.line")
        .unwrap()
        .browse([line])
        .unwrap();
    assert_eq!(
        string(&lines, &mut tx, "display_name").await,
        "Manual Payment (BNK7)"
    );
    let hidden = lines
        .clone()
        .with_context([("hide_payment_journal_id", Value::Int(1))]);
    assert_eq!(
        string(&hidden, &mut tx, "display_name").await,
        "Manual Payment"
    );
    // Several lines in one recordset, two on one journal and one on another: each names its own.
    let cash = money_journal(&mut tx, company, "CSH7", "cash").await;
    let lines = [
        line_of(&mut tx, bank, "inbound").await,
        line_of(&mut tx, cash, "inbound").await,
        line_of(&mut tx, bank, "outbound").await,
    ];
    let names = tx
        .env()
        .records("account.payment.method.line")
        .unwrap()
        .browse(lines)
        .unwrap()
        .cells(&mut tx, "display_name")
        .await
        .unwrap();
    let cash_name = string(
        &tx.env()
            .records("account.journal")
            .unwrap()
            .browse([cash])
            .unwrap(),
        &mut tx,
        "name",
    )
    .await;
    assert_eq!(
        names,
        [
            Cell::Str("Manual Payment (BNK7)".into()),
            Cell::Str(format!("Manual Payment ({cash_name})").into()),
            Cell::Str("Manual Payment (BNK7)".into()),
        ]
    );
}

/// plain-name context; a hand-built search on the same two conditions finds exactly one line, the
/// served domain string itself is not evaluated)
#[test]
fn the_dialog_method_dropdown_is_limited_to_its_journal_and_direction() {
    big_stack(dropdown_body);
}

async fn dropdown_body() {
    let (_db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let bank = bank_journal(&mut tx, company, "BNK8").await;
    let cash = money_journal(&mut tx, company, "CSH8", "cash").await;
    let env = tx.env().with_company(company).unwrap();
    let views = erp_addon_web::metadata_test_support::test_get_views(
        &env,
        &mut tx,
        "account.payment.register",
        vec![Value::list([Value::False, Value::str("form")])],
        32 * 1024 * 1024,
    )
    .await
    .unwrap();
    let served = erp_sdk::serde_json::to_value(views).unwrap();
    let node = served["views"][0]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["field"]["name"] == "payment_method_line_id")
        .unwrap_or_else(|| panic!("the method field is served: {served}"));
    // The served form of `[('journal_id', '=', journal_id), ('payment_type', '=', payment_type)]`.
    let lit =
        |s: &str| erp_sdk::serde_json::json!({"kind":"literal","value":{"kind":"str","value":s}});
    let name = |s: &str| erp_sdk::serde_json::json!({"kind":"name","name":s});
    let expected = erp_sdk::serde_json::json!({"schema":1,"nodes":[
        lit("journal_id"), lit("="), name("journal_id"), {"kind":"tuple","items":[0,1,2]},
        lit("payment_type"), lit("="), name("payment_type"), {"kind":"tuple","items":[4,5,6]},
        {"kind":"list","items":[3,7]},
    ]});
    assert_eq!(node["options"]["domain"], expected, "served domain: {node}");
    assert_eq!(
        node["options"]["context"]["nodes"][0],
        lit("hide_payment_journal_id"),
        "served context: {node}"
    );
    let lines = env.records("account.payment.method.line").unwrap();
    for (journal, direction) in [(bank, "inbound"), (bank, "outbound"), (cash, "inbound")] {
        let found = lines
            .search(
                &mut tx,
                &Domain::and([
                    Domain::condition("journal_id", Operator::Eq, journal).unwrap(),
                    Domain::condition("payment_type", Operator::Eq, direction).unwrap(),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(
            found.ids(),
            vec![line_of(&mut tx, journal, direction).await]
        );
    }
}
