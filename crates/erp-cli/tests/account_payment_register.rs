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
                    .set_cell("type", text("bank"))
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
        "a new bank journal has its two manual lines"
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
