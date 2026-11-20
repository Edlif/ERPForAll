// SPDX-License-Identifier: MIT OR Apache-2.0
//! Payment terms: their rules, the partner defaults, and the instalment lines and due date of
//! invoices and bills that use them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
use erp_sdk::prelude::*;
use erp_sdk::{Command, ErrorKind};
use erp_testkit::Harness;

async fn harness() -> Harness {
    Harness::builder([
        &erp_addon_base::ADDON,
        &erp_addon_web::ADDON,
        &erp_addon_uom::ADDON,
        &erp_addon_product::ADDON,
        &erp_addon_analytic::ADDON,
        &erp_addon_account::ADDON,
    ])
    .require_sql_objects([&erp_addon_account::reconcile::history_guard::MANIFEST])
    .schema_sync_options(erp_testkit::SchemaSyncOptions::inline().lifecycle(true))
    .build()
    .await
    .unwrap()
}

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

fn d(value: &str) -> Decimal {
    value.parse().unwrap()
}

fn day(value: &str) -> Date {
    Date::parse_odoo(value).unwrap()
}

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

/// A journal written directly: the journal rules read declared settings that a harness without
/// data files does not install.
async fn journal(
    tx: &mut Transaction,
    company: i64,
    code: &str,
    kind: &str,
    default_account: i64,
) -> i64 {
    tx.query(
        "INSERT INTO account_journal (name, code, active, type, company_id, sequence, \
         refund_sequence, payment_sequence, sequence_reset, sequence_padding, color, show_on_dashboard, \
         default_account_id) \
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

async fn partner(tx: &mut Transaction, name: &str) -> i64 {
    tx.env()
        .records("res.partner")
        .unwrap()
        .create(tx, vec![RawVals::new().set_cell("name", text(name))])
        .await
        .unwrap()
        .ids()[0]
}

struct Books {
    company: i64,
    partner: i64,
}

async fn books(tx: &mut Transaction, name: &str) -> Books {
    let company = company(tx, name).await;
    let income = account(tx, company, "400000", "income").await;
    let expense = account(tx, company, "600000", "expense").await;
    account(tx, company, "121000", "asset_receivable").await;
    account(tx, company, "211000", "liability_payable").await;
    journal(tx, company, "INV", "sale", income).await;
    journal(tx, company, "BILL", "purchase", expense).await;
    let partner = partner(tx, &format!("{name} partner")).await;
    Books { company, partner }
}

fn line_vals(kind: &str, amount: &str, days: i32) -> Command {
    Command::create(
        RawVals::new()
            .set_cell("value", text(kind))
            .set_cell("value_amount", Cell::Decimal(d(amount)))
            .set_cell("nb_days", Cell::Int(days)),
    )
}

async fn try_create(model: &Records, tx: &mut Transaction, vals: RawVals) -> Result<Records> {
    let model = model.clone();
    tx.savepoint(async move |tx| model.create(tx, vec![vals]).await)
        .await
}

fn terms(tx: &Transaction) -> Records {
    tx.env().records("account.payment.term").unwrap()
}

async fn term(tx: &mut Transaction, name: &str, lines: Vec<Command>) -> Result<Records> {
    let vals = RawVals::new()
        .set_cell("name", text(name))
        .set_commands("line_ids", lines);
    try_create(&terms(tx), tx, vals).await
}

fn moves(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move")
        .unwrap()
}

fn invoice(move_type: &str, partner: i64, price: &str) -> RawVals {
    RawVals::new()
        .set_cell("move_type", text(move_type))
        .set_cell("partner_id", Cell::Id(partner))
        .set_cell("invoice_date", Cell::Date(day("2026-11-16")))
        .set_commands(
            "invoice_line_ids",
            [Command::create(
                RawVals::new()
                    .set_cell("name", text("Consulting"))
                    .set_cell("quantity", Cell::Decimal(d("1")))
                    .set_cell("price_unit", Cell::Decimal(d(price))),
            )],
        )
}

/// The receivable or payable lines of an entry: (due date, balance), by due date.
async fn instalments(entry: &Records, tx: &mut Transaction) -> Vec<(String, Decimal)> {
    tx.flush().await.unwrap();
    let id = entry.ids()[0];
    tx.query(
        "SELECT date_maturity::text, balance::text FROM account_move_line \
         WHERE move_id = $1 AND display_type = 'payment_term' ORDER BY date_maturity, id",
        &[&id],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), d(&r.get::<_, String>(1))))
    .collect()
}

#[tokio::test]
async fn a_term_is_refused_unless_its_lines_are_valid() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    term(
        &mut tx,
        "Good",
        vec![
            line_vals("percent", "30", 0),
            line_vals("percent", "70", 30),
        ],
    )
    .await
    .unwrap();
    for (name, lines, needle) in [
        (
            "Short",
            vec![line_vals("percent", "95", 0)],
            "add up to exactly 100",
        ),
        (
            "Fixed",
            vec![line_vals("fixed", "100", 0)],
            "add up to exactly 100",
        ),
        ("None", vec![], "add up to exactly 100"),
        (
            "Range",
            vec![
                line_vals("percent", "105", 0),
                line_vals("percent", "-5", 0),
            ],
            "from 0 to 100",
        ),
    ] {
        let error = term(&mut tx, name, lines).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation, "{name}: {error:?}");
        assert!(error.message().contains(needle), "{name}: {error:?}");
    }
    for (day_text, needle) in [("ab", "not a whole number"), ("32", "from 0 to 31")] {
        let bad = Command::create(
            RawVals::new()
                .set_cell("value", text("percent"))
                .set_cell("value_amount", Cell::Decimal(d("100")))
                .set_cell("days_next_month", text(day_text)),
        );
        let error = term(&mut tx, "Day", vec![bad]).await.unwrap_err();
        assert!(error.message().contains(needle), "{day_text}: {error:?}");
    }
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn an_invoice_gets_one_line_per_instalment_and_the_last_due_date() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Instalments").await;
    let t = term(
        &mut tx,
        "30 now, 70 in 60",
        vec![
            line_vals("percent", "30", 0),
            line_vals("percent", "70", 60),
        ],
    )
    .await
    .unwrap();
    let mut vals = invoice("out_invoice", b.partner, "1000.01");
    vals.insert("invoice_payment_term_id".into(), Cell::Id(t.ids()[0]));
    let entry = moves(&tx, b.company)
        .create(&mut tx, vec![vals])
        .await
        .unwrap();
    assert_eq!(
        instalments(&entry, &mut tx).await,
        [
            ("2026-11-16".to_owned(), d("300.00")),
            ("2027-01-15".to_owned(), d("700.01")),
        ]
    );
    assert_eq!(
        entry.cell(&mut tx, "invoice_date_due").await.unwrap(),
        Cell::Date(day("2027-01-15"))
    );
    // a different term replaces the instalments
    let single = term(&mut tx, "30 days", vec![line_vals("percent", "100", 30)])
        .await
        .unwrap();
    entry
        .write(
            &mut tx,
            RawVals::new().set_cell("invoice_payment_term_id", Cell::Id(single.ids()[0])),
        )
        .await
        .unwrap();
    assert_eq!(
        instalments(&entry, &mut tx).await,
        [("2026-12-16".to_owned(), d("1000.01"))]
    );
    assert_eq!(
        entry.cell(&mut tx, "invoice_date_due").await.unwrap(),
        Cell::Date(day("2026-12-16"))
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_fixed_instalment_is_paid_first_and_the_last_line_takes_the_rest() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Fixed").await;
    let t = term(
        &mut tx,
        "Deposit",
        vec![
            line_vals("fixed", "200", 0),
            line_vals("percent", "100", 30),
        ],
    )
    .await
    .unwrap();
    let mut vals = invoice("out_invoice", b.partner, "500");
    vals.insert("invoice_payment_term_id".into(), Cell::Id(t.ids()[0]));
    let entry = moves(&tx, b.company)
        .create(&mut tx, vec![vals])
        .await
        .unwrap();
    let rows = instalments(&entry, &mut tx).await;
    assert_eq!(rows[0].1, d("200"));
    assert_eq!(rows[1].1, d("300"));
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_new_invoice_takes_the_partner_terms_of_its_kind() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Defaults").await;
    let customer = term(&mut tx, "Customer", vec![line_vals("percent", "100", 15)])
        .await
        .unwrap();
    let vendor = term(&mut tx, "Vendor", vec![line_vals("percent", "100", 45)])
        .await
        .unwrap();
    tx.env()
        .with_company(b.company)
        .unwrap()
        .records("res.partner")
        .unwrap()
        .browse([b.partner])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new()
                .set_cell("property_payment_term_id", Cell::Id(customer.ids()[0]))
                .set_cell(
                    "property_supplier_payment_term_id",
                    Cell::Id(vendor.ids()[0]),
                ),
        )
        .await
        .unwrap();
    let sale = moves(&tx, b.company)
        .create(&mut tx, vec![invoice("out_invoice", b.partner, "100")])
        .await
        .unwrap();
    assert_eq!(
        sale.cell(&mut tx, "invoice_payment_term_id").await.unwrap(),
        Cell::Id(customer.ids()[0])
    );
    assert_eq!(
        sale.cell(&mut tx, "invoice_date_due").await.unwrap(),
        Cell::Date(day("2026-12-01"))
    );
    let bill = moves(&tx, b.company)
        .create(&mut tx, vec![invoice("in_invoice", b.partner, "100")])
        .await
        .unwrap();
    assert_eq!(
        bill.cell(&mut tx, "invoice_payment_term_id").await.unwrap(),
        Cell::Id(vendor.ids()[0])
    );
    assert_eq!(
        instalments(&bill, &mut tx).await,
        [("2026-12-31".to_owned(), d("-100"))]
    );
    // the partner's terms fill only what the create leaves open
    let given = term(&mut tx, "Given", vec![line_vals("percent", "100", 5)])
        .await
        .unwrap();
    let mut with_term = invoice("out_invoice", b.partner, "100");
    with_term.insert("invoice_payment_term_id".into(), Cell::Id(given.ids()[0]));
    let mut without_term = invoice("out_invoice", b.partner, "100");
    without_term.insert("invoice_payment_term_id".into(), Cell::Null);
    let mut with_company = invoice("out_invoice", b.partner, "100");
    with_company.insert("company_id".into(), Cell::Id(b.company));
    // the company given with the values, not the active one, decides whose partner terms apply
    let any_company = tx.env().records("account.move").unwrap();
    for (case, model, vals, expected) in [
        (
            "a given term",
            moves(&tx, b.company),
            with_term,
            Cell::Id(given.ids()[0]),
        ),
        (
            "an explicitly empty term",
            moves(&tx, b.company),
            without_term,
            Cell::Null,
        ),
        (
            "the company given",
            any_company,
            with_company,
            Cell::Id(customer.ids()[0]),
        ),
    ] {
        let created = model.create(&mut tx, vec![vals]).await.unwrap();
        assert_eq!(
            created
                .cell(&mut tx, "invoice_payment_term_id")
                .await
                .unwrap(),
            expected,
            "{case}"
        );
    }
    let from_context = moves(&tx, b.company)
        .with_context([(
            "default_invoice_payment_term_id",
            Value::Int(given.ids()[0]),
        )])
        .create(&mut tx, vec![invoice("out_invoice", b.partner, "100")])
        .await
        .unwrap();
    assert_eq!(
        from_context
            .cell(&mut tx, "invoice_payment_term_id")
            .await
            .unwrap(),
        Cell::Id(given.ids()[0]),
        "the context default wins over the partner's terms"
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}
