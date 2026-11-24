// SPDX-License-Identifier: MIT OR Apache-2.0
//! Lock dates and final records on a full install: every route that would change a posted entry
//! in a locked period is refused naming the lock date and the entry (entry write, x2many commands,
//! item create, write and delete, entry delete, Reset to Draft, Cancel, import), for the
//! superuser, an elevated call and an accountant; a draft in a locked period stays editable; the
//! tax lock date binds taxed entries only; matched items and reconciliation rows cannot be changed
//! through the ORM; unmatching is subject to the lock dates an administrator can lower (not the hard
//! lock) and refuses a date without a fiscal period; the Reset to Draft flag of an entry.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

#[path = "support/accounting.rs"]
mod accounting;

use accounting::*;
use erp_data::impex::ImpexExt;
use erp_sdk::prelude::*;
use erp_addon_account::fiscal::status::{
    LockOutcome, PeriodSet, hard_lock_begin, hard_lock_complete, sync_lock_dates,
};
use erp_sdk::{Command, ErrorKind, SUPERUSER_ID};

async fn try_write(record: &Records, tx: &mut Transaction, vals: RawVals) -> Result<()> {
    let record = record.clone();
    tx.savepoint(async move |tx| {
        record.write(tx, vals).await?;
        tx.flush().await
    })
    .await
}

async fn try_unlink(record: &Records, tx: &mut Transaction) -> Result<()> {
    let record = record.clone();
    tx.savepoint(async move |tx| record.unlink(tx).await).await
}

async fn try_create(model: &Records, tx: &mut Transaction, vals: RawVals) -> Result<()> {
    let model = model.clone();
    tx.savepoint(async move |tx| model.create(tx, vec![vals]).await.map(|_| ()))
        .await
}

fn dec(value: &str) -> Cell {
    Cell::Decimal(num(value))
}

/// A lock refusal: `Validation`, naming `entry` and `lock`.
#[track_caller]
fn locked(error: &Error, entry: &str, lock: &str) {
    assert_eq!(error.kind(), ErrorKind::Validation, "{}", error.message());
    assert!(
        error.message().contains(entry) && error.message().contains(lock),
        "{entry} / {lock}: {}",
        error.message()
    );
}

/// The refusal of a closed entry that no lock date covers.
#[track_caller]
fn closed(error: &Error, entry: &str) {
    assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    assert!(
        error.message().contains("posted entry") && error.message().contains(entry),
        "{entry}: {}",
        error.message()
    );
    assert!(
        !error.message().contains("is locked"),
        "{}",
        error.message()
    );
}

async fn set_lock(tx: &mut Transaction, company: i64, field: &str, date: Option<Date>) {
    tx.execute(
        &format!("UPDATE res_company SET {field} = $2 WHERE id = $1"),
        &[&company, &date],
    )
    .await
    .unwrap();
    tx.invalidate_all().await.unwrap();
}

/// A 15 % sale tax of `company` with its repartition lines, written directly.
async fn sale_tax(tx: &mut Transaction, b: &Books) -> i64 {
    let tax_account = account(tx, b.company, "251000", "liability_current").await;
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
            &[&b.company],
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
            &[&b.company, &group, &country],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.query(
        "INSERT INTO account_tax_repartition_line (factor_percent, repartition_type, document_type, \
         account_id, tax_id, company_id, sequence, use_in_tax_closing) VALUES \
         (100, 'base', 'invoice', NULL, $1, $2, 1, false), (100, 'tax', 'invoice', $3, $1, $2, 2, true), \
         (100, 'base', 'refund', NULL, $1, $2, 1, false), (100, 'tax', 'refund', $3, $1, $2, 2, true)",
        &[&tax, &b.company, &tax_account],
    )
    .await
    .unwrap();
    tax
}

#[test]
fn a_posted_entry_in_a_locked_period_is_refused_on_every_route_naming_the_lock() {
    big_stack(every_route_body);
}

async fn every_route_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let locked_entry = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "100",
        d(2024, 1, 15),
    )
    .await;
    let open_entry = posted_entry(&mut tx, &b, b.bank, b.income, None, "40", d(2024, 2, 10)).await;
    let draft = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(d(2024, 1, 20)))
                    .set_commands(
                        "line_ids",
                        vec![
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.bank))
                                    .set_cell("balance", dec("7")),
                            ),
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.income))
                                    .set_cell("balance", dec("-7")),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap();
    let name = string(&locked_entry, &mut tx, "name").await;
    let open_name = string(&open_entry, &mut tx, "name").await;
    set_lock(
        &mut tx,
        company,
        "fiscalyear_lock_date",
        Some(d(2024, 1, 31)),
    )
    .await;
    let lock = "fiscalyear_lock_date through 2024-01-31";
    let item = items_on(&mut tx, &[&locked_entry], b.receivable).await;

    // the allow-listed fields stay writable in a locked period
    try_write(
        &locked_entry,
        &mut tx,
        RawVals::new().set_cell("ref", text("checked")),
    )
    .await
    .unwrap();
    try_write(
        &item,
        &mut tx,
        RawVals::new().set_cell("name", text("label")),
    )
    .await
    .unwrap();

    for elevated in [false, true] {
        let entry = locked_entry.sudo(elevated);
        let item = item.sudo(elevated);
        let lines = tx
            .env()
            .records("account.move.line")
            .unwrap()
            .sudo(elevated);
        for vals in [
            RawVals::new().set_cell("date", Cell::Date(d(2024, 2, 15))),
            RawVals::new().set_cell("name", text("MISC/2024/01/0099")),
            RawVals::new().set_commands(
                "line_ids",
                vec![Command::create(
                    RawVals::new()
                        .set_cell("account_id", Cell::Id(b.bank))
                        .set_cell("balance", dec("0")),
                )],
            ),
            RawVals::new().set_commands("line_ids", vec![Command::delete(rec(item.ids()[0]))]),
        ] {
            let error = try_write(&entry, &mut tx, vals).await.unwrap_err();
            locked(&error, &name, lock);
        }
        // a header field no lock date protects keeps the posted-entry refusal
        let error = try_write(
            &entry,
            &mut tx,
            RawVals::new().set_cell("partner_id", Cell::Id(b.vendor)),
        )
        .await
        .unwrap_err();
        closed(&error, &name);
        for vals in [
            RawVals::new().set_cell("balance", dec("9")),
            RawVals::new().set_cell("account_id", Cell::Id(b.other_receivable)),
            RawVals::new().set_cell("partner_id", Cell::Id(b.vendor)),
        ] {
            let error = try_write(&item, &mut tx, vals).await.unwrap_err();
            locked(&error, &name, lock);
        }
        let error = try_create(
            &lines,
            &mut tx,
            RawVals::new()
                .set_cell("move_id", Cell::Id(locked_entry.ids()[0]))
                .set_cell("account_id", Cell::Id(b.bank))
                .set_cell("balance", dec("0")),
        )
        .await
        .unwrap_err();
        locked(&error, &name, lock);
        locked(&try_unlink(&item, &mut tx).await.unwrap_err(), &name, lock);
        locked(&try_unlink(&entry, &mut tx).await.unwrap_err(), &name, lock);
        for method in ["button_draft", "button_cancel"] {
            let error = try_call(&entry, &mut tx, method).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
            for part in [name.as_str(), lock, "only allows a reversal"] {
                assert!(
                    error.message().contains(part),
                    "{method}: {}",
                    error.message()
                );
            }
        }
        // a posted entry in an open period: moving it into the locked period names the lock,
        // a move inside open periods keeps the posted-entry refusal
        let open = open_entry.sudo(elevated);
        let error = try_write(
            &open,
            &mut tx,
            RawVals::new().set_cell("date", Cell::Date(d(2024, 1, 20))),
        )
        .await
        .unwrap_err();
        locked(&error, &open_name, lock);
        let error = try_write(
            &open,
            &mut tx,
            RawVals::new().set_cell("date", Cell::Date(d(2024, 2, 20))),
        )
        .await
        .unwrap_err();
        closed(&error, &open_name);
    }
    // the import route writes through the same override and rolls back
    let item_id = item.ids()[0];
    let model = tx.env().records("account.move.line").unwrap();
    let result = tx
        .savepoint(async |tx| {
            model
                .load(
                    tx,
                    &[".id", "balance"],
                    &[vec![item_id.to_string(), "9".into()]],
                )
                .await
        })
        .await
        .unwrap();
    assert!(result.has_errors(), "{:?}", result.messages);
    assert!(
        result
            .messages
            .iter()
            .any(|m| m.message.contains(lock) && m.message.contains(&name)),
        "{:?}",
        result.messages
    );
    assert_eq!(decimal(&item, &mut tx, "balance").await, num("100"));
    assert_eq!(string(&locked_entry, &mut tx, "state").await, "posted");
    assert_eq!(
        locked_entry.cell(&mut tx, "date").await.unwrap(),
        Cell::Date(d(2024, 1, 15))
    );

    // a draft dated in the locked period stays a draft to edit and delete (posting re-dates it)
    try_write(
        &draft,
        &mut tx,
        RawVals::new().set_cell("date", Cell::Date(d(2024, 1, 21))),
    )
    .await
    .unwrap();
    try_unlink(&draft, &mut tx).await.unwrap();

    // an accountant who is not an administrator is refused the same way; a read-only user is
    // refused access first
    let system = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "lock-admin", company, &[system]).await;
    let accounting = xml_id(&mut tx, "account", "group_account_user").await;
    let readonly = xml_id(&mut tx, "account", "group_account_readonly").await;
    let accountant = user(&mut tx, "lock-accountant", company, &[accounting]).await;
    let reader = user(&mut tx, "lock-reader", company, &[readonly]).await;
    let entry_id = locked_entry.ids()[0];
    tx.commit().await.unwrap();
    let policy = policy_orm(&db, &orm).await;
    for (who, kind) in [
        (accountant, ErrorKind::Validation),
        (reader, ErrorKind::Access),
    ] {
        let mut tx = policy
            .begin(erp_orm::UserId::new(who).unwrap())
            .await
            .unwrap();
        let entry = tx
            .env()
            .records("account.move")
            .unwrap()
            .browse([entry_id])
            .unwrap();
        let error = try_write(
            &entry,
            &mut tx,
            RawVals::new().set_cell("date", Cell::Date(d(2024, 2, 15))),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), kind, "{}", error.message());
        if kind == ErrorKind::Validation {
            locked(&error, &name, lock);
            let error = try_call(&entry, &mut tx, "button_draft").await.unwrap_err();
            assert!(error.message().contains(lock), "{}", error.message());
        }
        tx.rollback().await.unwrap();
    }
    drop(policy);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn the_tax_lock_date_binds_posted_entries_that_reach_the_tax_report() {
    big_stack(tax_lock_body);
}

async fn tax_lock_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let tax = sale_tax(&mut tx, &b).await;
    let taxed = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("move_type", text("out_invoice"))
                    .set_cell("partner_id", Cell::Id(b.customer))
                    .set_cell("invoice_date", Cell::Date(d(2024, 1, 15)))
                    .set_cell("date", Cell::Date(d(2024, 1, 15)))
                    .set_commands(
                        "invoice_line_ids",
                        vec![Command::create(
                            RawVals::new()
                                .set_cell("name", text("Consulting"))
                                .set_cell("quantity", dec("1"))
                                .set_cell("price_unit", dec("100"))
                                .set_commands("tax_ids", [Command::set([rec(tax)])]),
                        )],
                    ),
            ],
        )
        .await
        .unwrap();
    taxed
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap();
    let untaxed = posted_entry(&mut tx, &b, b.bank, b.income, None, "40", d(2024, 1, 15)).await;
    let taxed_name = string(&taxed, &mut tx, "name").await;
    let untaxed_name = string(&untaxed, &mut tx, "name").await;
    set_lock(&mut tx, company, "tax_lock_date", Some(d(2024, 1, 31))).await;
    let rename = || RawVals::new().set_cell("name", text("RENAMED/0001"));
    let error = try_write(&taxed, &mut tx, rename()).await.unwrap_err();
    locked(&error, &taxed_name, "tax_lock_date through 2024-01-31");
    locked(
        &try_unlink(&taxed, &mut tx).await.unwrap_err(),
        &taxed_name,
        "tax_lock_date",
    );
    // an entry with no tax line is outside the tax lock date: only the posted-entry rule refuses
    let error = try_write(&untaxed, &mut tx, rename()).await.unwrap_err();
    closed(&error, &untaxed_name);
    // an account change asks the fiscal dates only: the tax lock date does not name it
    let item = items_on(&mut tx, &[&taxed], b.receivable).await;
    let error = try_write(
        &item,
        &mut tx,
        RawVals::new().set_cell("account_id", Cell::Id(b.other_receivable)),
    )
    .await
    .unwrap_err();
    closed(&error, &taxed_name);
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn matched_items_and_reconciliation_rows_stay_as_the_reconciliation_left_them() {
    big_stack(matched_body);
}

async fn matched_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    // a settled pair and a partly matched pair
    let invoice = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "1000",
        d(2024, 1, 10),
    )
    .await;
    let paid = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-1000",
        d(2024, 2, 1),
    )
    .await;
    let second = posted_invoice(&mut tx, &b, "out_invoice", b.customer, "500", d(2024, 2, 2)).await;
    let part = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-200",
        d(2024, 2, 3),
    )
    .await;
    let settled = items_on(&mut tx, &[&invoice, &paid], b.receivable).await;
    try_call(&settled, &mut tx, "reconcile").await.unwrap();
    let partly = items_on(&mut tx, &[&second, &part], b.receivable).await;
    try_call(&partly, &mut tx, "reconcile").await.unwrap();
    let invoice_name = string(&invoice, &mut tx, "name").await;
    let partials: Vec<i64> = tx
        .query(
            "SELECT id::int8 FROM account_partial_reconcile ORDER BY id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    let full: i64 = tx
        .query("SELECT id::int8 FROM account_full_reconcile", &[])
        .await
        .unwrap()[0]
        .get(0);
    let open: i64 = tx
        .query(
            "SELECT id::int8 FROM account_open_item ORDER BY id LIMIT 1",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let env = tx.env();
    let partial = env
        .records("account.partial.reconcile")
        .unwrap()
        .browse([partials[0]])
        .unwrap();
    let settlement = env
        .records("account.full.reconcile")
        .unwrap()
        .browse([full])
        .unwrap();
    let open_item = env
        .records("account.open.item")
        .unwrap()
        .browse([open])
        .unwrap();

    // no ORM change of a reconciliation row, for the superuser and an elevated call
    let service = |error: Error| {
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        assert!(
            error.message().contains("Only matching and unmatching"),
            "{}",
            error.message()
        );
    };
    for elevated in [false, true] {
        let (partial, settlement, open_item) = (
            partial.sudo(elevated),
            settlement.sudo(elevated),
            open_item.sudo(elevated),
        );
        service(
            try_write(
                &partial,
                &mut tx,
                RawVals::new().set_cell("amount", dec("1")),
            )
            .await
            .unwrap_err(),
        );
        service(
            try_write(
                &settlement,
                &mut tx,
                RawVals::new().set_cell("company_id", Cell::Id(company)),
            )
            .await
            .unwrap_err(),
        );
        service(
            try_write(
                &open_item,
                &mut tx,
                RawVals::new().set_cell("amount_residual", dec("0")),
            )
            .await
            .unwrap_err(),
        );
        service(try_unlink(&open_item, &mut tx).await.unwrap_err());
        for (model, vals) in [
            (&partial, RawVals::new().set_cell("amount", dec("1"))),
            (
                &settlement,
                RawVals::new().set_cell("company_id", Cell::Id(company)),
            ),
            (
                &open_item,
                RawVals::new().set_cell("amount_residual", dec("0")),
            ),
        ] {
            service(try_create(model, &mut tx, vals).await.unwrap_err());
            let source = model.clone();
            service(
                tx.savepoint(async move |tx| source.copy(tx, None).await.map(|_| ()))
                    .await
                    .unwrap_err(),
            );
        }
        let error = try_unlink(&settlement, &mut tx).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
    }
    assert_eq!(decimal(&partial, &mut tx, "amount").await, num("1000"));

    // a matched posted entry: Reset to Draft, Cancel and Delete name the entry and its matches
    for method in ["button_draft", "button_cancel"] {
        let error = try_call(&invoice, &mut tx, method).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        for part in [invoice_name.as_str(), "matched", "only allows a reversal"] {
            assert!(
                error.message().contains(part),
                "{method}: {}",
                error.message()
            );
        }
    }
    let error = try_unlink(&invoice, &mut tx).await.unwrap_err();
    assert!(
        error.message().contains("matched") && error.message().contains(&invoice_name),
        "{}",
        error.message()
    );
    assert_eq!(
        invoice.cell(&mut tx, "reset_blocked").await.unwrap(),
        Cell::Bool(true)
    );

    // a matched item of a draft (the services never make one; written here directly) keeps its
    // amounts, account, currency and partner, and blocks Cancel and Delete of its entry
    let draft = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(d(2024, 3, 1)))
                    .set_commands(
                        "line_ids",
                        vec![
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.receivable))
                                    .set_cell("partner_id", Cell::Id(b.customer))
                                    .set_cell("balance", dec("50")),
                            ),
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.income))
                                    .set_cell("balance", dec("-50")),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap();
    let cancelled = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(d(2024, 3, 2)))
                    .set_commands(
                        "line_ids",
                        vec![
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.receivable))
                                    .set_cell("balance", dec("60")),
                            ),
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.income))
                                    .set_cell("balance", dec("-60")),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap();
    try_call(&cancelled, &mut tx, "button_cancel")
        .await
        .unwrap();
    assert_eq!(
        cancelled.cell(&mut tx, "reset_blocked").await.unwrap(),
        Cell::Bool(false)
    );
    assert_eq!(
        draft.cell(&mut tx, "reset_blocked").await.unwrap(),
        Cell::Bool(true)
    );
    tx.flush().await.unwrap();
    let draft_item = items_on(&mut tx, &[&draft], b.receivable).await;
    let cancelled_item = items_on(&mut tx, &[&cancelled], b.receivable).await;
    for line in [draft_item.ids()[0], cancelled_item.ids()[0]] {
        tx.execute(
            "INSERT INTO account_open_item (line_id, company_id, account_id, partner_id, \
             currency_id, balance, amount_currency, amount_residual, amount_residual_currency, \
             component_id) SELECT id, company_id, account_id, partner_id, currency_id, \
             COALESCE(balance, 0), COALESCE(amount_currency, 0), COALESCE(balance, 0), \
             COALESCE(amount_currency, 0), 0 FROM account_move_line WHERE id = $1",
            &[&line],
        )
        .await
        .unwrap();
    }
    tx.invalidate_all().await.unwrap();
    for vals in [
        RawVals::new().set_cell("balance", dec("55")),
        RawVals::new().set_cell("debit", dec("55")),
        RawVals::new().set_cell("account_id", Cell::Id(b.other_receivable)),
        RawVals::new().set_cell("partner_id", Cell::Id(b.vendor)),
    ] {
        let error = try_write(&draft_item, &mut tx, vals).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        assert!(
            error.message().contains("is matched with other items"),
            "{}",
            error.message()
        );
    }
    try_write(
        &draft_item,
        &mut tx,
        RawVals::new().set_cell("name", text("still a label")),
    )
    .await
    .unwrap();
    for error in [
        try_call(&draft, &mut tx, "button_cancel")
            .await
            .unwrap_err(),
        try_unlink(&draft, &mut tx).await.unwrap_err(),
        try_call(&cancelled, &mut tx, "button_draft")
            .await
            .unwrap_err(),
    ] {
        assert_eq!(error.kind(), ErrorKind::UserError, "{}", error.message());
        assert!(error.message().contains("matched"), "{}", error.message());
    }
    assert_eq!(
        cancelled.cell(&mut tx, "reset_blocked").await.unwrap(),
        Cell::Bool(true)
    );
    tx.rollback().await.unwrap();

    // unmatching is subject to the lock dates; once the period is open again it works
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let invoice = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "300",
        d(2024, 1, 12),
    )
    .await;
    let pay = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-300",
        d(2024, 2, 12),
    )
    .await;
    let items = items_on(&mut tx, &[&invoice, &pay], b.receivable).await;
    try_call(&items, &mut tx, "reconcile").await.unwrap();
    let name = string(&invoice, &mut tx, "name").await;
    set_lock(
        &mut tx,
        company,
        "fiscalyear_lock_date",
        Some(d(2024, 1, 31)),
    )
    .await;
    let error = try_call(&items, &mut tx, "remove_move_reconcile")
        .await
        .unwrap_err();
    locked(&error, &name, "fiscalyear_lock_date through 2024-01-31");
    assert!(error.message().contains("unmatch"), "{}", error.message());
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("0")
    );
    set_lock(&mut tx, company, "fiscalyear_lock_date", None).await;
    try_call(&items, &mut tx, "remove_move_reconcile")
        .await
        .unwrap();
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("300")
    );
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn unmatching_is_refused_when_any_item_it_changes_is_in_a_locked_period() {
    big_stack(walk_body);
}

async fn walk_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    // January's invoice and February's invoice settled by one February payment
    let january = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "100",
        d(2024, 1, 12),
    )
    .await;
    let february =
        posted_invoice(&mut tx, &b, "out_invoice", b.customer, "200", d(2024, 2, 5)).await;
    let pay = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-300",
        d(2024, 2, 12),
    )
    .await;
    set_lock(
        &mut tx,
        company,
        "fiscalyear_lock_date",
        Some(d(2024, 1, 31)),
    )
    .await;
    // matching an item of a locked period is allowed: it changes no accounting date or amount
    let items = items_on(&mut tx, &[&january, &february, &pay], b.receivable).await;
    try_call(&items, &mut tx, "reconcile").await.unwrap();
    assert_eq!(
        decimal(&january, &mut tx, "amount_residual").await,
        num("0")
    );
    let february_item = items_on(&mut tx, &[&february], b.receivable).await.ids()[0];
    let pay_item = items_on(&mut tx, &[&pay], b.receivable).await.ids()[0];
    let rows = tx
        .query(
            "SELECT id::int8 FROM account_partial_reconcile \
             WHERE debit_move_id = $1 AND credit_move_id = $2",
            &[&february_item, &pay_item],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "February's invoice has its own match");
    let partial = tx
        .env()
        .records("account.partial.reconcile")
        .unwrap()
        .browse([rows[0].get::<_, i64>(0)])
        .unwrap();
    // both ends of that match are in February, but removing it unsettles January's item too
    let name = string(&january, &mut tx, "name").await;
    let error = try_unlink(&partial, &mut tx).await.unwrap_err();
    locked(&error, &name, "fiscalyear_lock_date through 2024-01-31");
    let left: i64 = tx
        .query(
            "SELECT count(*)::int8 FROM account_partial_reconcile WHERE credit_move_id = $1",
            &[&pay_item],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(left, 2);
    set_lock(&mut tx, company, "fiscalyear_lock_date", None).await;
    try_unlink(&partial, &mut tx).await.unwrap();
    assert_eq!(
        decimal(&february, &mut tx, "amount_residual").await,
        num("200")
    );
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn a_match_into_a_hard_locked_period_can_be_undone_and_a_date_without_a_period_refuses() {
    big_stack(hard_body);
}

async fn hard_body() {
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
        "400",
        d(2024, 1, 12),
    )
    .await;
    let pay = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-400",
        d(2024, 2, 12),
    )
    .await;
    let later = posted_invoice(
        &mut tx,
        &b,
        "out_invoice",
        b.customer,
        "300",
        d(2024, 2, 10),
    )
    .await;
    let later_pay = posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-300",
        d(2024, 3, 10),
    )
    .await;
    let later_name = string(&later, &mut tx, "name").await;
    // January closed by the fiscal-year lock date, then hard-locked by the hard lock operation; the
    // fiscal-year date can no longer be lowered across it
    set_lock(
        &mut tx,
        company,
        "fiscalyear_lock_date",
        Some(d(2024, 1, 31)),
    )
    .await;
    sync_lock_dates(&mut tx, company).await.unwrap();
    let january: i64 = tx
        .query_one(
            "SELECT id::int8 FROM account_fiscal_period WHERE company_id = $1 AND kind = 'regular' \
             AND $2 BETWEEN date_from AND date_to",
            &[&company, &d(2024, 1, 12)],
        )
        .await
        .unwrap()
        .get(0);
    let set = PeriodSet::of(&mut tx, january).await.unwrap();
    let run = hard_lock_begin(&mut tx, &set).await.unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    let mut tx = orm
        .begin_opts(erp_sdk::BeginOptions::new(SUPERUSER_ID).isolation(erp_sdk::Isolation::ReadCommitted))
        .await
        .unwrap();
    assert_eq!(
        hard_lock_complete(&mut tx, &run).await.unwrap(),
        LockOutcome::Applied
    );
    tx.commit().await.unwrap();
    drop(tx);
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    // matching into the hard-locked period is allowed, so its undoing stays possible: neither the
    // hard lock nor the fiscal-year date that cannot be lowered across that period binds it
    let items = items_on(&mut tx, &[&invoice, &pay], b.receivable).await;
    try_call(&items, &mut tx, "reconcile").await.unwrap();
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("0")
    );
    try_call(&items, &mut tx, "remove_move_reconcile")
        .await
        .unwrap();
    assert_eq!(
        decimal(&invoice, &mut tx, "amount_residual").await,
        num("400")
    );
    // outside that period a lock date that can be lowered refuses the unmatch until it is lowered
    let items = items_on(&mut tx, &[&later, &later_pay], b.receivable).await;
    try_call(&items, &mut tx, "reconcile").await.unwrap();
    set_lock(
        &mut tx,
        company,
        "fiscalyear_lock_date",
        Some(d(2024, 2, 29)),
    )
    .await;
    sync_lock_dates(&mut tx, company).await.unwrap();
    let error = try_call(&items, &mut tx, "remove_move_reconcile")
        .await
        .unwrap_err();
    locked(&error, &later_name, "fiscalyear_lock_date through 2024-02-29");
    assert!(
        !error.message().contains("hard_lock_date"),
        "{}",
        error.message()
    );
    set_lock(
        &mut tx,
        company,
        "fiscalyear_lock_date",
        Some(d(2024, 1, 31)),
    )
    .await;
    sync_lock_dates(&mut tx, company).await.unwrap();
    try_call(&items, &mut tx, "remove_move_reconcile")
        .await
        .unwrap();
    assert_eq!(
        decimal(&later, &mut tx, "amount_residual").await,
        num("300")
    );
    // a posted date without a fiscal period (posting creates it, so only a direct write removes
    // it) refuses the unmatch instead of letting it through
    try_call(&items, &mut tx, "reconcile").await.unwrap();
    let gone = tx
        .execute(
            "DELETE FROM account_fiscal_period WHERE company_id = $1 AND kind = 'regular' \
             AND $2 BETWEEN date_from AND date_to",
            &[&company, &d(2024, 2, 10)],
        )
        .await
        .unwrap();
    assert_eq!(gone, 1);
    tx.invalidate_all().await.unwrap();
    let error = try_call(&items, &mut tx, "remove_move_reconcile")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{}", error.message());
    assert!(
        error.message().contains(&later_name) && error.message().contains("No fiscal period"),
        "{}",
        error.message()
    );
    assert_eq!(
        decimal(&later, &mut tx, "amount_residual").await,
        num("0")
    );
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}
