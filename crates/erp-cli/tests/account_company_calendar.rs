// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company-local defaults and decision boundaries on the installed accounting stack.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
#[path = "support/accounting.rs"]
mod accounting;
use accounting::*;
use erp_addon_account::locking::{LockField, LockPatch, change_lock_dates};
use erp_sdk::prelude::*;
use erp_sdk::{
    BeginOptions, Command, Context, ErrorKind, OperationIdentity, SUPERUSER_ID, ValueMap,
};

async fn set_zone(tx: &mut Transaction, company: i64, zone: &str) {
    tx.env()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap()
        .write(tx, RawVals::new().set_cell("tz", text(zone)))
        .await
        .unwrap();
}
async fn day(tx: &mut Transaction, zone: &str) -> Date {
    tx.query(
        "SELECT (transaction_timestamp() AT TIME ZONE $1)::date",
        &[&zone],
    )
    .await
    .unwrap()[0]
        .get(0)
}
fn east(env: &Env) -> Env {
    env.with_context([("tz", Value::str("Pacific/Kiritimati"))])
}
async fn cash_journal(tx: &mut Transaction, company: i64) -> i64 {
    let cash = account(tx, company, "101999", "asset_cash").await;
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.journal")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Calendar bank"))
                    .set_cell("code", text("CAL"))
                    .set_cell("type", text("bank"))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("default_account_id", Cell::Id(cash)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

#[test]
fn accounting_defaults_use_record_company_calendar() {
    big_stack(defaults_body);
}
async fn defaults_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let main = main_company(&mut tx).await;
    let company = other_company(&mut tx, main).await;
    set_zone(&mut tx, company, "Pacific/Pago_Pago").await;
    let expected = day(&mut tx, "Pacific/Pago_Pago").await;
    assert!(expected < day(&mut tx, "Pacific/Kiritimati").await);
    let journal = journal(&mut tx, company, "CAL", "general", None).await;
    let env = east(&tx.env()).with_company(main).unwrap();
    let model = env.records("account.move").unwrap();
    let rows = model
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("journal_id", Cell::Id(journal))
                    .set_cell("company_id", Cell::Id(company)),
                RawVals::new()
                    .set_cell("journal_id", Cell::Id(journal))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("date", Cell::Date(d(2024, 2, 1))),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.cells(&mut tx, "date").await.unwrap(),
        vec![Cell::Date(expected), Cell::Date(d(2024, 2, 1))]
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn payment_defaults_use_record_company_calendar() {
    big_stack(payments_body);
}
async fn payments_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let main = main_company(&mut tx).await;
    let company = other_company(&mut tx, main).await;
    set_zone(&mut tx, company, "Pacific/Pago_Pago").await;
    let expected = day(&mut tx, "Pacific/Pago_Pago").await;
    let env = east(&tx.env()).with_company(main).unwrap();
    let bank = cash_journal(&mut tx, company).await;
    let payments = env
        .records("account.payment")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("journal_id", Cell::Id(bank))
                    .set_cell("amount", Cell::Decimal(num("10"))),
                RawVals::new()
                    .set_cell("journal_id", Cell::Id(bank))
                    .set_cell("amount", Cell::Decimal(num("10")))
                    .set_cell("date", Cell::Date(d(2024, 2, 1))),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        payments.cells(&mut tx, "date").await.unwrap(),
        vec![Cell::Date(expected), Cell::Date(d(2024, 2, 1))]
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn invoice_defaults_use_each_company_calendar() {
    big_stack(invoice_body);
}
async fn invoice_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let main = main_company(&mut tx).await;
    let other = other_company(&mut tx, main).await;
    let env = east(&tx.env()).with_company(main).unwrap();
    let mut ids = Vec::new();
    let mut expected = Vec::new();
    for (company, zone) in [(main, "Pacific/Kiritimati"), (other, "Pacific/Pago_Pago")] {
        set_zone(&mut tx, company, zone).await;
        let b = books(&mut tx, company).await;
        let records = env
            .records("account.move")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("company_id", Cell::Id(company))
                        .set_cell("move_type", text("out_invoice"))
                        .set_cell("partner_id", Cell::Id(b.customer)),
                ],
            )
            .await
            .unwrap();
        ids.extend(records.ids());
        expected.push(Cell::Date(day(&mut tx, zone).await));
    }
    erp_addon_account::invoice::post::before_post(&mut tx, &env, &ids)
        .await
        .unwrap();
    assert_eq!(
        env.records("account.move")
            .unwrap()
            .browse(ids)
            .unwrap()
            .cells(&mut tx, "invoice_date")
            .await
            .unwrap(),
        expected
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn future_lock_boundary_uses_company_calendar() {
    big_stack(lock_body);
}
async fn lock_body() {
    let (db, orm) = install().await;
    let mut ctx = Context::new();
    ctx.set("tz", Value::str("Pacific/Kiritimati"));
    let mut tx = orm
        .begin_opts(
            BeginOptions::new(SUPERUSER_ID)
                .context(ctx)
                .operation_identity(OperationIdentity::generate().unwrap()),
        )
        .await
        .unwrap();
    let company = main_company(&mut tx).await;
    set_zone(&mut tx, company, "Pacific/Pago_Pago").await;
    let future = day(&mut tx, "Pacific/Kiritimati").await;
    let error = change_lock_dates(
        &mut tx,
        company,
        &LockPatch::new().set(LockField::Sale, Some(future)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert_eq!(
        error.message(),
        "sale_lock_date cannot be set in the future."
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn posting_boundary_uses_company_calendar() {
    big_stack(posting_body);
}
async fn posting_body() {
    let (db, orm) = install().await;
    let mut ctx = Context::new();
    ctx.set("tz", Value::str("Pacific/Kiritimati"));
    let mut tx = orm
        .begin_opts(BeginOptions::new(SUPERUSER_ID).context(ctx))
        .await
        .unwrap();
    let company = main_company(&mut tx).await;
    set_zone(&mut tx, company, "Pacific/Pago_Pago").await;
    let west = day(&mut tx, "Pacific/Pago_Pago").await;
    let b = books(&mut tx, company).await;
    // Fixture policy, not a lock-service test: this ORM has no audit-event store.
    let through = erp_addon_account::fiscal::calendar::previous_day(west);
    tx.execute(
        "UPDATE res_company SET sale_lock_date=$2 WHERE id=$1",
        &[&company, &through],
    )
    .await
    .unwrap();
    tx.invalidate_model("res.company", None).await.unwrap();
    let invoice = posted_invoice(&mut tx, &b, "out_invoice", b.customer, "10", d(2024, 2, 1)).await;
    assert_eq!(
        invoice.cell(&mut tx, "date").await.unwrap(),
        Cell::Date(west)
    );
    assert_eq!(
        invoice.cell(&mut tx, "invoice_date").await.unwrap(),
        Cell::Date(d(2024, 2, 1))
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn dashboard_uses_company_calendar() {
    big_stack(dashboard_body);
}
async fn dashboard_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let main = main_company(&mut tx).await;
    let company = other_company(&mut tx, main).await;
    set_zone(&mut tx, company, "Pacific/Pago_Pago").await;
    let expected = day(&mut tx, "Pacific/Pago_Pago").await;
    let b = books(&mut tx, company).await;
    let _invoice =
        posted_invoice(&mut tx, &b, "out_invoice", b.customer, "10", d(2024, 2, 1)).await;
    let env = east(&tx.env()).with_company(main).unwrap();
    let journals = env
        .records("account.journal")
        .unwrap()
        .search(
            &mut tx,
            &Domain::and([
                Domain::field("company_id").unwrap().eq(company),
                Domain::field("type").unwrap().eq("sale"),
            ]),
        )
        .await
        .unwrap();
    let cards = journals.cells(&mut tx, "dashboard_card").await.unwrap();
    let Cell::Str(card) = &cards[0] else {
        panic!("dashboard must return text")
    };
    let card: serde_json::Value = serde_json::from_str(card).unwrap();
    let late = card["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["label"] == "Late")
        .unwrap();
    assert!(
        late["domain"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!([
                "invoice_date_due",
                "<",
                expected.to_string()
            ]))
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn payment_dialog_uses_company_calendar() {
    big_stack(dialog_body);
}
async fn dialog_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let main = main_company(&mut tx).await;
    let company = other_company(&mut tx, main).await;
    set_zone(&mut tx, company, "Pacific/Pago_Pago").await;
    let expected = day(&mut tx, "Pacific/Pago_Pago").await;
    let b = books(&mut tx, company).await;
    let invoice = posted_invoice(&mut tx, &b, "out_invoice", b.customer, "10", d(2024, 2, 1)).await;
    let env = east(&tx.env()).with_company(main).unwrap();
    let dialog = env
        .with_context([(
            "default_invoice_ids",
            Value::list([Command::set(invoice.ids().into_iter().map(rec)).to_value()]),
        )])
        .records("account.payment.register")
        .unwrap();
    let defaults: ValueMap = dialog
        .call(&mut tx, "default_get", (vec!["payment_date".to_owned()],))
        .await
        .unwrap();
    assert_eq!(defaults.get("payment_date"), Some(&Value::Date(expected)));
    let created = env
        .records("account.payment.register")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_commands(
                "invoice_ids",
                vec![Command::set(invoice.ids().into_iter().map(rec))],
            )],
        )
        .await
        .unwrap();
    assert_eq!(
        created.cell(&mut tx, "payment_date").await.unwrap(),
        Cell::Date(expected)
    );
    let explicit = env
        .records("account.payment.register")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_commands(
                        "invoice_ids",
                        vec![Command::set(invoice.ids().into_iter().map(rec))],
                    )
                    .set_cell("payment_date", Cell::Date(d(2024, 2, 3))),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        explicit.cell(&mut tx, "payment_date").await.unwrap(),
        Cell::Date(d(2024, 2, 3))
    );
    let main_books = books(&mut tx, main).await;
    let main_invoice = posted_invoice(
        &mut tx,
        &main_books,
        "out_invoice",
        main_books.customer,
        "10",
        d(2024, 2, 1),
    )
    .await;
    let mixed = env
        .with_context([(
            "default_invoice_ids",
            Value::list([Command::set(
                invoice.ids().into_iter().chain(main_invoice.ids()).map(rec),
            )
            .to_value()]),
        )])
        .records("account.payment.register")
        .unwrap();
    let error = mixed
        .call::<_, ValueMap>(&mut tx, "default_get", (vec!["payment_date".to_owned()],))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(
        error.message(),
        "The documents belong to several companies; register their payments one company at a time."
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    db.cleanup().await.unwrap();
}

#[test]
fn ancestor_journal_calendar_preserves_branch_reader_access() {
    big_stack(ancestor_body);
}
async fn ancestor_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let root = main_company(&mut tx).await;
    let currency = tx
        .env()
        .records("res.company")
        .unwrap()
        .browse([root])
        .unwrap()
        .cell(&mut tx, "currency_id")
        .await
        .unwrap();
    let branch = tx
        .env()
        .records("res.company")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Calendar branch"))
                    .set_cell("parent_id", Cell::Id(root))
                    .set_cell("currency_id", currency),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    set_zone(&mut tx, root, "Pacific/Pago_Pago").await;
    let journal = journal(&mut tx, root, "ANC", "sale", None).await;
    let internal = xml_id(&mut tx, "base", "group_user").await;
    let system = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "calendar-admin", root, &[system]).await;
    let readonly = xml_id(&mut tx, "account", "group_account_readonly").await;
    let uid = tx
        .env()
        .records("res.users")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Branch calendar reader"))
                    .set_cell("login", text("branch-calendar-reader"))
                    .set_cell("company_id", Cell::Id(branch))
                    .set_commands("company_ids", [Command::set([rec(branch)])])
                    .set_commands("group_ids", [Command::set([rec(internal), rec(readonly)])]),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    tx.commit().await.unwrap();
    drop(tx);
    let secured = policy_orm(&db, &orm).await;
    let mut tx = secured
        .begin(erp_orm::UserId::new(uid).unwrap())
        .await
        .unwrap();
    let env = tx.env();
    let parent = env.records("res.company").unwrap().browse([root]).unwrap();
    let error = parent
        .check_access(&mut tx, erp_sdk::Operation::Read)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(
        error.message(),
        format!(
            "Access rules protect some of these records.\n\nBranch calendar reader (id={uid}) has no 'read' access to:\n- Company (res.company)\n\nAsk an administrator if you need access to these records."
        )
    );
    let records = env
        .records("account.journal")
        .unwrap()
        .browse([journal])
        .unwrap();
    records
        .check_access(&mut tx, erp_sdk::Operation::Read)
        .await
        .unwrap();
    let Cell::Str(card) = records.cell(&mut tx, "dashboard_card").await.unwrap() else {
        panic!("expected dashboard text")
    };
    let card: serde_json::Value = serde_json::from_str(&card).unwrap();
    let late = card["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["label"] == "Late")
        .unwrap();
    let expected = day(&mut tx, "Pacific/Pago_Pago").await;
    assert!(
        late["domain"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!([
                "invoice_date_due",
                "<",
                expected.to_string()
            ]))
    );
    env.sudo(true)
        .records("res.company")
        .unwrap()
        .browse([root])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("tz", text("Pacific/Kiritimati")),
        )
        .await
        .unwrap();
    let Cell::Str(updated) = records.cell(&mut tx, "dashboard_card").await.unwrap() else {
        panic!("expected dashboard text")
    };
    let updated: serde_json::Value = serde_json::from_str(&updated).unwrap();
    let late = updated["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["label"] == "Late")
        .unwrap();
    let east_day = day(&mut tx, "Pacific/Kiritimati").await;
    assert!(east_day > expected);
    assert!(
        late["domain"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!([
                "invoice_date_due",
                "<",
                east_day.to_string()
            ]))
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(secured);
    drop(orm);
    db.cleanup().await.unwrap();
}
