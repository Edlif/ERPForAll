// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reconciliation of receivable and payable items on a full install: partial and full matches,
//! residuals, matching numbers, the amount due and payment status of invoices, unmatching, the
//! refusals by name, the database bounds, access (an accountant who is not an administrator, a
//! read-only user, another company) and two units racing for one item.
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

async fn reconcile(tx: &mut Transaction, lines: &Records) -> Result<()> {
    try_call(lines, tx, "reconcile").await
}

/// The item of `entries` on the receivable account.
async fn receivable(tx: &mut Transaction, b: &Books, entry: &Records) -> Records {
    items_on(tx, &[entry], b.receivable).await
}

async fn partial_ids(tx: &mut Transaction, line: &Records) -> Vec<i64> {
    let mut ids = line.related(tx, "matched_credit_ids").await.unwrap().ids();
    ids.extend(line.related(tx, "matched_debit_ids").await.unwrap().ids());
    ids.sort_unstable();
    ids
}

#[test]
fn paying_an_invoice_in_two_parts_settles_it_and_unmatching_reopens_it() {
    big_stack(two_parts_body);
}

async fn two_parts_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let invoice = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "1000",
        d(2024, 1, 1),
    )
    .await;
    let first = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-300",
        d(2024, 2, 1),
    )
    .await;
    let inv_item = receivable(&mut tx, &b, &invoice).await;
    let first_item = receivable(&mut tx, &b, &first).await;
    assert_eq!(
        decimal(&inv_item, &mut tx, "amount_residual").await,
        num("1000")
    );
    assert_eq!(string(&inv_item, &mut tx, "matching_number").await, "");

    let both = inv_item.union(&first_item).unwrap();
    reconcile(&mut tx, &both).await.unwrap();
    assert_eq!(
        decimal(&inv_item, &mut tx, "amount_residual").await,
        num("700")
    );
    assert_eq!(
        decimal(&first_item, &mut tx, "amount_residual").await,
        num("0")
    );
    assert_eq!(
        first_item.cell(&mut tx, "reconciled").await.unwrap(),
        Cell::Bool(true)
    );
    assert_eq!(
        inv_item.cell(&mut tx, "reconciled").await.unwrap(),
        Cell::Bool(false)
    );
    let partials = partial_ids(&mut tx, &inv_item).await;
    assert_eq!(partials.len(), 1);
    let label = format!("P{}", partials[0]);
    assert_eq!(string(&inv_item, &mut tx, "matching_number").await, label);
    assert_eq!(string(&first_item, &mut tx, "matching_number").await, label);
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("700")
    );
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "partial");

    let second = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-700",
        d(2024, 2, 5),
    )
    .await;
    let second_item = receivable(&mut tx, &b, &second).await;
    reconcile(&mut tx, &inv_item.union(&second_item).unwrap())
        .await
        .unwrap();
    assert_eq!(
        decimal(&inv_item, &mut tx, "amount_residual").await,
        num("0")
    );
    let full = inv_item.cell(&mut tx, "full_reconcile_id").await.unwrap();
    let Cell::Id(full) = full else {
        panic!("a settled invoice item has its settlement, got {full:?}");
    };
    for item in [&inv_item, &first_item, &second_item] {
        assert_eq!(
            string(item, &mut tx, "matching_number").await,
            full.to_string()
        );
    }
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("0")
    );
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "paid");
    let open: i64 = tx
        .query("SELECT count(*)::int8 FROM account_open_item", &[])
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(open, 0, "a settled component keeps no open-item rows");

    // unmatching the second payment: back to partial, the settlement is gone
    try_call(&second_item, &mut tx, "remove_move_reconcile")
        .await
        .unwrap();
    assert_eq!(
        inv_item.cell(&mut tx, "full_reconcile_id").await.unwrap(),
        Cell::Null
    );
    assert_eq!(
        decimal(&inv_item, &mut tx, "amount_residual").await,
        num("700")
    );
    assert_eq!(string(&second_item, &mut tx, "matching_number").await, "");
    assert_eq!(string(&inv_item, &mut tx, "matching_number").await, label);
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "partial");
    // and the first: not paid, the whole amount due
    try_call(&inv_item, &mut tx, "remove_move_reconcile")
        .await
        .unwrap();
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("1000")
    );
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "not_paid");
    assert_eq!(string(&inv_item, &mut tx, "matching_number").await, "");

    // an invoice in two instalments: one payment over both, and one per instalment in due order
    let term = tx
        .env()
        .records("account.payment.term")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("30 now, 70 in 30"))
                    .set_commands(
                        "line_ids",
                        [("30", 0), ("70", 30)].map(|(share, days)| {
                            Command::create(
                                RawVals::new()
                                    .set_cell("value", text("percent"))
                                    .set_cell("value_amount", Cell::Decimal(num(share)))
                                    .set_cell("nb_days", Cell::Int(days)),
                            )
                        }),
                    ),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    let instalment_invoice = async |tx: &mut Transaction| {
        let line = Command::create(
            RawVals::new()
                .set_cell("name", text("Service"))
                .set_cell("quantity", Cell::Decimal(num("1")))
                .set_cell("price_unit", Cell::Decimal(num("1000"))),
        );
        let created = moves(tx, b.company)
            .create(
                tx,
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
        created.call::<(), ()>(tx, "action_post", ()).await.unwrap();
        created
    };
    // the residual of each instalment of `invoice`, earliest due first
    let residuals = async |tx: &mut Transaction, invoice: &Records| {
        let ids: Vec<i64> = tx
            .query(
                "SELECT id::int8 FROM account_move_line WHERE move_id = $1 \
                 AND display_type = 'payment_term' ORDER BY date_maturity, id",
                &[&invoice.ids()[0]],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect();
        let items = tx
            .env()
            .records("account.move.line")
            .unwrap()
            .browse(ids)
            .unwrap();
        items
            .cells(tx, "amount_residual")
            .await
            .unwrap()
            .into_iter()
            .map(|cell| match cell {
                Cell::Decimal(value) => value,
                other => panic!("amount_residual is {other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let whole = instalment_invoice(&mut tx).await;
    assert_eq!(residuals(&mut tx, &whole).await, [num("300"), num("700")]);
    let payment = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-1000",
        d(2024, 3, 2),
    )
    .await;
    let items = items_on(&mut tx, &[&whole, &payment], b.receivable).await;
    reconcile(&mut tx, &items).await.unwrap();
    assert_eq!(residuals(&mut tx, &whole).await, [num("0"), num("0")]);
    assert_eq!(decimal(&whole, &mut tx, "amount_residual").await, num("0"));
    assert_eq!(string(&whole, &mut tx, "payment_state").await, "paid");

    let split = instalment_invoice(&mut tx).await;
    let first = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-300",
        d(2024, 3, 2),
    )
    .await;
    let items = items_on(&mut tx, &[&split, &first], b.receivable).await;
    reconcile(&mut tx, &items).await.unwrap();
    assert_eq!(
        residuals(&mut tx, &split).await,
        [num("0"), num("700")],
        "the payment settles the instalment due first"
    );
    assert_eq!(
        decimal(&split, &mut tx, "amount_residual").await,
        num("700")
    );
    assert_eq!(string(&split, &mut tx, "payment_state").await, "partial");
    let rest = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-700",
        d(2024, 3, 3),
    )
    .await;
    // the settled first instalment is not selected again: only what is still open
    let open: Vec<i64> = tx
        .query(
            "SELECT id::int8 FROM account_move_line WHERE move_id = ANY($1) AND account_id = $2 \
             AND full_reconcile_id IS NULL ORDER BY id",
            &[&vec![split.ids()[0], rest.ids()[0]], &b.receivable],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        open.len(),
        2,
        "the second instalment and the second payment"
    );
    let items = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .browse(open)
        .unwrap();
    reconcile(&mut tx, &items).await.unwrap();
    assert_eq!(residuals(&mut tx, &split).await, [num("0"), num("0")]);
    assert_eq!(decimal(&split, &mut tx, "amount_residual").await, num("0"));
    assert_eq!(string(&split, &mut tx, "payment_state").await, "paid");
    let settlements: i64 = tx
        .query(
            "SELECT count(DISTINCT full_reconcile_id)::int8 FROM account_move_line \
             WHERE move_id = ANY($1) AND account_id = $2",
            &[
                &vec![split.ids()[0], first.ids()[0], rest.ids()[0]],
                &b.receivable,
            ],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(
        settlements, 2,
        "each instalment is settled with the payment that covered it"
    );

    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn chains_of_matches_share_their_smallest_match_id() {
    big_stack(chain_body);
}

async fn chain_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let day = d(2024, 1, 1);
    let mut lines = Vec::new();
    for amount in ["1000", "-500", "-1000", "1000"] {
        let entry = posted_entry(
            &mut tx,
            &b,
            b.receivable,
            b.income,
            Some(b.customer),
            amount,
            day,
        )
        .await;
        lines.push(receivable(&mut tx, &b, &entry).await);
    }
    let [a, bb, c, dd] = [&lines[0], &lines[1], &lines[2], &lines[3]];
    reconcile(&mut tx, &a.union(bb).unwrap()).await.unwrap();
    let first = partial_ids(&mut tx, bb).await[0];
    // B is settled, A of its component is open in the selection: B is left out, A meets C
    reconcile(&mut tx, &a.union(bb).unwrap().union(c).unwrap())
        .await
        .unwrap();
    assert_eq!(partial_ids(&mut tx, bb).await, vec![first]);
    reconcile(&mut tx, &c.union(dd).unwrap()).await.unwrap();
    for line in [a, bb, c, dd] {
        assert_eq!(
            string(line, &mut tx, "matching_number").await,
            format!("P{first}")
        );
    }
    // a settled item with an item outside its component is refused, naming its counterpart
    let z = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.income,
        Some(b.customer),
        "-1",
        day,
    )
    .await;
    let z = receivable(&mut tx, &b, &z).await;
    let error = reconcile(&mut tx, &bb.union(&z).unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    assert!(
        error.message().contains("already fully matched"),
        "{}",
        error.message()
    );

    try_call(bb, &mut tx, "remove_move_reconcile")
        .await
        .unwrap();
    assert_eq!(string(bb, &mut tx, "matching_number").await, "");
    let remaining = partial_ids(&mut tx, a).await[0];
    for line in [a, c, dd] {
        assert_eq!(
            string(line, &mut tx, "matching_number").await,
            format!("P{remaining}")
        );
    }
    // A (500 open) with B again: B joins the component; C (settled) is left out with it
    reconcile(&mut tx, &a.union(bb).unwrap().union(c).unwrap())
        .await
        .unwrap();
    assert_eq!(decimal(a, &mut tx, "amount_residual").await, num("0"));
    assert_eq!(
        string(bb, &mut tx, "matching_number").await,
        format!("P{remaining}")
    );

    // a settlement is not deleted on its own; deleting its match unmatches the items
    let x = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.income,
        Some(b.customer),
        "250",
        day,
    )
    .await;
    let y = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.income,
        Some(b.customer),
        "-250",
        day,
    )
    .await;
    let (x, y) = (
        receivable(&mut tx, &b, &x).await,
        receivable(&mut tx, &b, &y).await,
    );
    reconcile(&mut tx, &x.union(&y).unwrap()).await.unwrap();
    let full = x.related(&mut tx, "full_reconcile_id").await.unwrap();
    assert_eq!(full.len(), 1);
    let error = tx
        .savepoint(async |tx| full.unlink(tx).await)
        .await
        .unwrap_err();
    assert!(
        error.message().contains("unmatch its journal items"),
        "{}",
        error.message()
    );
    assert_eq!(decimal(&x, &mut tx, "amount_residual").await, num("0"));
    let pair = x.related(&mut tx, "matched_credit_ids").await.unwrap();
    pair.unlink(&mut tx).await.unwrap();
    assert_eq!(decimal(&x, &mut tx, "amount_residual").await, num("250"));
    assert_eq!(
        x.cell(&mut tx, "full_reconcile_id").await.unwrap(),
        Cell::Null
    );
    assert_eq!(string(&y, &mut tx, "matching_number").await, "");

    // an invoice settled only by its credit note reads reversed
    let invoice = posted_invoice(&mut tx, &b, "out_invoice", b.customer, "80", day).await;
    let refund = posted_invoice(&mut tx, &b, "out_refund", b.customer, "80", day).await;
    let both = items_on(&mut tx, &[&invoice, &refund], b.receivable).await;
    reconcile(&mut tx, &both).await.unwrap();
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "reversed");
    assert_eq!(string(&refund, &mut tx, "payment_state").await, "paid");
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn what_cannot_be_matched_is_refused_by_name() {
    big_stack(refusals_body);
}

async fn refusals_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let second = other_company(&mut tx, company).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    periods(&orm, second).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let o = books(&mut tx, second).await;
    let day = d(2024, 3, 1);
    let refused = |error: Error, words: &str| {
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        assert!(
            error.message().contains(words),
            "{words}: {}",
            error.message()
        );
    };
    // two reconcilable accounts
    let clearing = account(&mut tx, company, "121900", "asset_current").await;
    tx.env()
        .records("account.account")
        .unwrap()
        .browse([clearing])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("reconcile", Cell::Bool(true)),
        )
        .await
        .unwrap();
    let on_receivable = posted_entry(&mut tx, &b, b.receivable, b.income, None, "40", day).await;
    let on_clearing = posted_entry(&mut tx, &b, clearing, b.income, None, "-40", day).await;
    let mixed = receivable(&mut tx, &b, &on_receivable)
        .await
        .union(&items_on(&mut tx, &[&on_clearing], clearing).await)
        .unwrap();
    refused(reconcile(&mut tx, &mixed).await.unwrap_err(), "121900");

    // an account without reconciliation
    let income = items_on(&mut tx, &[&on_receivable, &on_clearing], b.income).await;
    refused(
        reconcile(&mut tx, &income).await.unwrap_err(),
        "does not allow reconciliation",
    );

    // a draft entry
    let draft = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(day))
                    .set_commands(
                        "line_ids",
                        vec![
                            erp_sdk::Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.receivable))
                                    .set_cell("balance", Cell::Decimal(num("-40"))),
                            ),
                            erp_sdk::Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.income))
                                    .set_cell("balance", Cell::Decimal(num("40"))),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap();
    let with_draft = receivable(&mut tx, &b, &on_receivable)
        .await
        .union(&receivable(&mut tx, &b, &draft).await)
        .unwrap();
    refused(
        reconcile(&mut tx, &with_draft).await.unwrap_err(),
        "posted entries",
    );

    // another root company (its own accounts: refused before the company check)
    let theirs = posted_entry(&mut tx, &o, o.receivable, o.income, None, "-40", day).await;
    let across = receivable(&mut tx, &b, &on_receivable)
        .await
        .union(&items_on(&mut tx, &[&theirs], o.receivable).await)
        .unwrap();
    let error = reconcile(&mut tx, &across).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());

    // an item without an amount has nothing to match, which is its reason
    let empty = posted_entry(&mut tx, &b, b.receivable, b.income, None, "0", day).await;
    let some = posted_entry(&mut tx, &b, b.receivable, b.income, None, "-15", day).await;
    let pair = items_on(&mut tx, &[&empty, &some], b.receivable).await;
    let error = reconcile(&mut tx, &pair).await.unwrap_err();
    assert!(
        error.message().contains("has no amount to match"),
        "{}",
        error.message()
    );

    // the database bounds hold whatever path writes
    let item = receivable(&mut tx, &b, &on_receivable).await.ids()[0];
    let counter = posted_entry(&mut tx, &b, b.receivable, b.income, None, "-40", day).await;
    let counter = receivable(&mut tx, &b, &counter).await.ids()[0];
    let raw = tx
        .savepoint(async |tx| {
            tx.execute(
                "INSERT INTO account_partial_reconcile (debit_move_id, credit_move_id, company_id, \
                 debit_currency_id, credit_currency_id, amount, debit_amount_currency, \
                 credit_amount_currency) \
                 SELECT $1, $2, company_id, currency_id, currency_id, 0, 0, 0 \
                 FROM account_move_line WHERE id = $1",
                &[&item, &counter],
            )
            .await
            .map(|_| ())
        })
        .await;
    assert!(raw.is_err(), "a match of nothing is refused by its CHECK");
    let lines = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .browse([item, counter])
        .unwrap();
    reconcile(&mut tx, &lines).await.unwrap();
    let full: Option<i64> = tx
        .query(
            "SELECT full_reconcile_id::int8 FROM account_move_line WHERE id = $1",
            &[&item],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert!(full.is_some());
    // an over-applied residual on an open item fails its bound
    let partial = posted_entry(&mut tx, &b, b.receivable, b.income, None, "100", day).await;
    let partial = receivable(&mut tx, &b, &partial).await;
    let small = posted_entry(&mut tx, &b, b.receivable, b.income, None, "-10", day).await;
    let small = receivable(&mut tx, &b, &small).await;
    reconcile(&mut tx, &partial.union(&small).unwrap())
        .await
        .unwrap();
    let id = partial.ids()[0];
    let over = tx
        .savepoint(async |tx| {
            tx.execute(
                "UPDATE account_open_item SET amount_residual = amount_residual - 200 \
                 WHERE line_id = $1",
                &[&id],
            )
            .await
            .map(|_| ())
        })
        .await;
    assert!(
        over.is_err(),
        "a residual beyond the item's amount fails its CHECK"
    );
    // a matched item cannot be deleted under its matches, partly or fully settled
    for line in [id, item] {
        let deleted = tx
            .savepoint(async |tx| {
                tx.execute("DELETE FROM account_move_line WHERE id = $1", &[&line])
                    .await
                    .map(|_| ())
            })
            .await;
        assert!(deleted.is_err(), "the database keeps matched item {line}");
    }
    assert_eq!(
        decimal(&partial, &mut tx, "amount_residual").await,
        num("90")
    );
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

/// An accountant who is not an administrator matches the items of the company; a read-only user
/// and an accountant of another company are refused before anything is read; two units racing for
/// the same item never both apply.
#[test]
fn matching_asks_the_accounting_rights_and_never_applies_twice() {
    big_stack(access_body);
}

async fn access_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let second = other_company(&mut tx, company).await;
    let branch = tx
        .env()
        .records("res.company")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Branch"))
                    .set_cell("parent_id", Cell::Id(company)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    tx.commit().await.unwrap();
    // the root's periods cover its branch
    periods(&orm, company).await;
    periods(&orm, second).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    // an item of the company settled by an entry of a branch the accountant below cannot open
    journal(&mut tx, branch, "BMISC", "general", None).await;
    let in_branch = Books {
        company: branch,
        ..b
    };
    let settled = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "70",
        d(2024, 4, 3),
    )
    .await;
    let theirs = posted_entry(
        &mut tx,
        &in_branch,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-70",
        d(2024, 4, 3),
    )
    .await;
    let theirs_name = string(&theirs, &mut tx, "name").await;
    let pair = items_on(&mut tx, &[&settled, &theirs], b.receivable).await;
    reconcile(&mut tx, &pair).await.unwrap();
    let settled_item = items_on(&mut tx, &[&settled], b.receivable).await;
    let settled_match = partial_ids(&mut tx, &settled_item).await;
    let settled_item = settled_item.ids();
    assert_eq!(settled_match.len(), 1);
    let extra = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-5",
        d(2024, 4, 3),
    )
    .await;
    let settled_ids = items_on(&mut tx, &[&settled, &extra], b.receivable)
        .await
        .ids();
    let invoice =
        posted_invoice(&mut tx, &b, "out_invoice", b.customer, "500", d(2024, 4, 1)).await;
    let pay = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-200",
        d(2024, 4, 2),
    )
    .await;
    let ids = items_on(&mut tx, &[&invoice, &pay], b.receivable)
        .await
        .ids();
    // the user rules ask for an administrator before other users are created
    let system = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "reconcile-admin", company, &[system]).await;
    let accounting = xml_id(&mut tx, "account", "group_account_user").await;
    let readonly = xml_id(&mut tx, "account", "group_account_readonly").await;
    let accountant = user(&mut tx, "reconcile-accountant", company, &[accounting]).await;
    let reader = user(&mut tx, "reconcile-reader", company, &[readonly]).await;
    let outsider = user(&mut tx, "reconcile-outsider", second, &[accounting]).await;
    tx.commit().await.unwrap();
    let policy = policy_orm(&db, &orm).await;
    for who in [reader, outsider] {
        let mut tx = policy
            .begin(erp_orm::UserId::new(who).unwrap())
            .await
            .unwrap();
        let lines = tx
            .env()
            .records("account.move.line")
            .unwrap()
            .browse(ids.clone())
            .unwrap();
        let error = reconcile(&mut tx, &lines).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access, "{}", error.message());
        tx.rollback().await.unwrap();
    }
    // an accountant who is not an administrator matches the company's items
    let mut tx = policy
        .begin(erp_orm::UserId::new(accountant).unwrap())
        .await
        .unwrap();
    let lines = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .browse(ids.clone())
        .unwrap();
    reconcile(&mut tx, &lines).await.unwrap();
    let mine = tx
        .env()
        .records("account.move")
        .unwrap()
        .browse(invoice.ids())
        .unwrap();
    assert_eq!(string(&mine, &mut tx, "payment_state").await, "partial");
    // the refusal of a settled item names no entry the accountant cannot open
    let lines = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .browse(settled_ids)
        .unwrap();
    let error = reconcile(&mut tx, &lines).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    assert!(
        error.message().contains("1 entry you cannot open")
            && !error.message().contains(&theirs_name),
        "{}",
        error.message()
    );
    tx.rollback().await.unwrap();
    // unmatching asks the right to delete the match and to change both items: a read-only user,
    // an accountant of another company, and an accountant whose match reaches an entry of a branch
    // they cannot change are refused, by removing the item's matches or by deleting the match
    for who in [reader, outsider, accountant] {
        let mut tx = policy
            .begin(erp_orm::UserId::new(who).unwrap())
            .await
            .unwrap();
        let lines = tx
            .env()
            .records("account.move.line")
            .unwrap()
            .browse(settled_item.clone())
            .unwrap();
        let error = try_call(&lines, &mut tx, "remove_move_reconcile")
            .await
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::Access,
            "{who}: {}",
            error.message()
        );
        let matches = tx
            .env()
            .records("account.partial.reconcile")
            .unwrap()
            .browse(settled_match.clone())
            .unwrap();
        let error = tx
            .savepoint(async |tx| matches.unlink(tx).await)
            .await
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::Access,
            "{who}: {}",
            error.message()
        );
        tx.rollback().await.unwrap();
    }
    drop(policy);

    // two units on the same items: the later one fails, one match exists
    let mut late = orm.begin(SUPERUSER_ID).await.unwrap();
    late.query("SELECT 1", &[]).await.unwrap();
    let mut early = orm.begin(SUPERUSER_ID).await.unwrap();
    let lines = early
        .env()
        .records("account.move.line")
        .unwrap()
        .browse(ids.clone())
        .unwrap();
    reconcile(&mut early, &lines).await.unwrap();
    early.commit().await.unwrap();
    let lines = late
        .env()
        .records("account.move.line")
        .unwrap()
        .browse(ids.clone())
        .unwrap();
    let error = reconcile(&mut late, &lines).await.unwrap_err();
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
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn unmatching_a_settling_payment_reopens_the_invoice() {
    big_stack(unmatch_settled_body);
}

async fn unmatch_settled_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let invoice =
        posted_invoice(&mut tx, &b, "out_invoice", b.customer, "400", d(2024, 1, 1)).await;
    let payment = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-400",
        d(2024, 2, 1),
    )
    .await;
    let inv_item = receivable(&mut tx, &b, &invoice).await;
    let pay_item = receivable(&mut tx, &b, &payment).await;
    reconcile(&mut tx, &inv_item.union(&pay_item).unwrap())
        .await
        .unwrap();
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "paid");

    try_call(&pay_item, &mut tx, "remove_move_reconcile")
        .await
        .unwrap();
    for item in [&inv_item, &pay_item] {
        assert_eq!(string(item, &mut tx, "matching_number").await, "");
        assert_eq!(
            item.cell(&mut tx, "full_reconcile_id").await.unwrap(),
            Cell::Null
        );
        assert_eq!(
            item.cell(&mut tx, "reconciled").await.unwrap(),
            Cell::Bool(false)
        );
    }
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("400")
    );
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "not_paid");
    assert!(partial_ids(&mut tx, &inv_item).await.is_empty());
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}
