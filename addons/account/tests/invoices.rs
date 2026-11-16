// SPDX-License-Identifier: MIT OR Apache-2.0
//! Customer invoices and vendor bills in draft: the invoice line sync (tax lines, the
//! receivable/payable line, the totals) for the four invoice types, the posting checks the posting
//! service calls, and company isolation of the accounts the sync picks.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
use erp_addon_account::invoice::post::{before_post, check_postable};
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

fn rec(id: i64) -> RecId {
    RecId::from_real(id).unwrap()
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
    default_account: Option<i64>,
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

/// A percent tax of `company` with one base and one tax repartition line per document kind,
/// written directly (the tax rules read settings the harness does not install).
async fn tax(
    tx: &mut Transaction,
    company: i64,
    name: &str,
    rate: &str,
    use_: &str,
    account: i64,
) -> i64 {
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
    let id: i64 = tx
        .query(
            "INSERT INTO account_tax (name, type_tax_use, amount_type, active, company_id, \
             root_company_id, sequence, amount, include_base_amount, is_base_affected, tax_group_id, \
             country_id, repartition_serial, analytic) \
             VALUES (jsonb_build_object('en_US', $2::text), $3::text, 'percent', true, $1, $1, 1, \
             $4::text::numeric, false, true, $5, $6, 0, false) RETURNING id::int8",
            &[&company, &name, &use_, &rate, &group, &country],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.query(
        "INSERT INTO account_tax_repartition_line (factor_percent, repartition_type, document_type, \
         account_id, tax_id, company_id, sequence, use_in_tax_closing) VALUES \
         (100, 'base', 'invoice', NULL, $1, $2, 1, false), (100, 'tax', 'invoice', $3, $1, $2, 2, true), \
         (100, 'base', 'refund', NULL, $1, $2, 1, false), (100, 'tax', 'refund', $3, $1, $2, 2, true)",
        &[&id, &company, &account],
    )
    .await
    .unwrap();
    id
}

/// A company with sale and purchase journals, their accounts and a 15 % sale and purchase tax.
struct Books {
    company: i64,
    sale: i64,
    purchase: i64,
    income: i64,
    expense: i64,
    receivable: i64,
    payable: i64,
    sale_tax_account: i64,
    sale_tax: i64,
    purchase_tax: i64,
    customer: i64,
}

async fn books(tx: &mut Transaction, name: &str) -> Books {
    let company = company(tx, name).await;
    let income = account(tx, company, "400000", "income").await;
    let expense = account(tx, company, "600000", "expense").await;
    let receivable = account(tx, company, "121000", "asset_receivable").await;
    let payable = account(tx, company, "211000", "liability_payable").await;
    let sale_tax_account = account(tx, company, "251000", "liability_current").await;
    let purchase_tax_account = account(tx, company, "131000", "asset_current").await;
    let sale = journal(tx, company, "INV", "sale", Some(income)).await;
    let purchase = journal(tx, company, "BILL", "purchase", Some(expense)).await;
    let sale_tax = tax(tx, company, "VAT 15%", "15", "sale", sale_tax_account).await;
    let purchase_tax = tax(
        tx,
        company,
        "VAT 15% (purchase)",
        "15",
        "purchase",
        purchase_tax_account,
    )
    .await;
    let customer = partner(tx, &format!("{name} customer")).await;
    Books {
        company,
        sale,
        purchase,
        income,
        expense,
        receivable,
        payable,
        sale_tax_account,
        sale_tax,
        purchase_tax,
        customer,
    }
}

fn moves(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move")
        .unwrap()
}

fn product_line(quantity: &str, price: &str, discount: &str, taxes: &[i64]) -> Command {
    Command::create(
        RawVals::new()
            .set_cell("name", text("Consulting"))
            .set_cell("quantity", Cell::Decimal(d(quantity)))
            .set_cell("price_unit", Cell::Decimal(d(price)))
            .set_cell("discount", Cell::Decimal(d(discount)))
            .set_commands("tax_ids", [Command::set(taxes.iter().map(|t| rec(*t)))]),
    )
}

fn invoice(move_type: &str, partner: i64, lines: Vec<Command>) -> RawVals {
    RawVals::new()
        .set_cell("move_type", text(move_type))
        .set_cell("partner_id", Cell::Id(partner))
        .set_cell(
            "invoice_date",
            Cell::Date(Date::parse_odoo("2026-11-16").unwrap()),
        )
        .set_commands("invoice_line_ids", lines)
}

async fn try_create(model: &Records, tx: &mut Transaction, vals: RawVals) -> Result<Records> {
    let model = model.clone();
    tx.savepoint(async move |tx| model.create(tx, vec![vals]).await)
        .await
}

async fn decimal(record: &Records, tx: &mut Transaction, field: &str) -> Decimal {
    match record.cell(tx, field).await.unwrap() {
        Cell::Decimal(value) => value,
        Cell::Null => Decimal::ZERO,
        other => panic!("{field} is {other:?}"),
    }
}

/// (display type, account, balance) of every journal item of `entry`, sorted.
async fn items(entry: &Records, tx: &mut Transaction) -> Vec<(String, i64, Decimal)> {
    tx.flush().await.unwrap();
    let id = entry.ids()[0];
    let mut rows: Vec<(String, i64, Decimal)> = tx
        .query(
            "SELECT display_type, account_id::int8, balance::text FROM account_move_line \
             WHERE move_id = $1 ORDER BY display_type, balance",
            &[&id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), d(&r.get::<_, String>(2))))
        .collect();
    rows.sort();
    rows
}

#[tokio::test]
async fn a_customer_invoice_gets_its_tax_and_receivable_lines_and_totals() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Totals").await;
    let entry = moves(&tx, b.company)
        .create(
            &mut tx,
            vec![invoice(
                "out_invoice",
                b.customer,
                vec![
                    // 2 * 100 less 10 % = 180, 15 % tax = 27
                    product_line("2", "100", "10", &[b.sale_tax]),
                    product_line("1", "50", "0", &[]),
                ],
            )],
        )
        .await
        .unwrap();
    assert_eq!(
        entry.cell(&mut tx, "journal_id").await.unwrap(),
        Cell::Id(b.sale)
    );
    let mut expected = vec![
        ("payment_term".to_owned(), b.receivable, d("257")),
        ("product".to_owned(), b.income, d("-180")),
        ("product".to_owned(), b.income, d("-50")),
        ("tax".to_owned(), b.sale_tax_account, d("-27")),
    ];
    expected.sort();
    assert_eq!(items(&entry, &mut tx).await, expected);
    assert_eq!(decimal(&entry, &mut tx, "amount_untaxed").await, d("230"));
    assert_eq!(decimal(&entry, &mut tx, "amount_tax").await, d("27"));
    assert_eq!(decimal(&entry, &mut tx, "amount_total").await, d("257"));
    assert_eq!(decimal(&entry, &mut tx, "amount_residual").await, d("257"));
    assert_eq!(
        entry.cell(&mut tx, "payment_state").await.unwrap(),
        text("not_paid")
    );
    assert_eq!(
        entry.cell(&mut tx, "invoice_date_due").await.unwrap(),
        entry.cell(&mut tx, "invoice_date").await.unwrap(),
        "the due date is the invoice date until payment terms exist"
    );
    let lines = entry.related(&mut tx, "invoice_line_ids").await.unwrap();
    assert_eq!(
        lines.len(),
        2,
        "invoice_line_ids holds only the product lines"
    );
    let subtotals = lines.cells(&mut tx, "price_subtotal").await.unwrap();
    let totals = lines.cells(&mut tx, "price_total").await.unwrap();
    assert!(
        subtotals.contains(&Cell::Decimal(d("180"))) && totals.contains(&Cell::Decimal(d("207")))
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn each_invoice_type_books_its_lines_on_the_right_side() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Signs").await;
    // (type, journal, product account, term account, sign of the product line)
    let cases = [
        (
            "out_invoice",
            b.sale,
            b.income,
            b.receivable,
            b.sale_tax,
            "-1",
        ),
        (
            "out_refund",
            b.sale,
            b.income,
            b.receivable,
            b.sale_tax,
            "1",
        ),
        (
            "in_invoice",
            b.purchase,
            b.expense,
            b.payable,
            b.purchase_tax,
            "1",
        ),
        (
            "in_refund",
            b.purchase,
            b.expense,
            b.payable,
            b.purchase_tax,
            "-1",
        ),
    ];
    for (move_type, journal, product_account, term_account, tax, sign) in cases {
        let entry = moves(&tx, b.company)
            .create(
                &mut tx,
                vec![invoice(
                    move_type,
                    b.customer,
                    vec![product_line("1", "100", "0", &[tax])],
                )],
            )
            .await
            .unwrap();
        assert_eq!(
            entry.cell(&mut tx, "journal_id").await.unwrap(),
            Cell::Id(journal),
            "{move_type}"
        );
        let sign = d(sign);
        let found = items(&entry, &mut tx).await;
        let product = found.iter().find(|r| r.0 == "product").unwrap();
        assert_eq!(
            (product.1, product.2),
            (product_account, sign * d("100")),
            "{move_type}"
        );
        let tax_line = found.iter().find(|r| r.0 == "tax").unwrap();
        assert_eq!(tax_line.2, sign * d("15"), "{move_type}");
        let term = found.iter().find(|r| r.0 == "payment_term").unwrap();
        assert_eq!(
            (term.1, term.2),
            (term_account, -sign * d("115")),
            "{move_type}"
        );
        assert_eq!(
            decimal(&entry, &mut tx, "amount_total").await,
            d("115"),
            "{move_type}"
        );
    }
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn editing_the_lines_rebuilds_the_tax_and_receivable_lines() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Edits").await;
    let entry = moves(&tx, b.company)
        .create(
            &mut tx,
            vec![invoice(
                "out_invoice",
                b.customer,
                vec![product_line("1", "100", "0", &[b.sale_tax])],
            )],
        )
        .await
        .unwrap();
    let line = entry
        .related(&mut tx, "invoice_line_ids")
        .await
        .unwrap()
        .ids()[0];
    entry
        .write(
            &mut tx,
            RawVals::new().set_commands(
                "invoice_line_ids",
                [Command::update(
                    rec(line),
                    RawVals::new().set_cell("price_unit", Cell::Decimal(d("200"))),
                )],
            ),
        )
        .await
        .unwrap();
    let found = items(&entry, &mut tx).await;
    assert_eq!(found.iter().find(|r| r.0 == "tax").unwrap().2, d("-30"));
    assert_eq!(
        found.iter().find(|r| r.0 == "payment_term").unwrap().2,
        d("230")
    );
    entry
        .write(
            &mut tx,
            RawVals::new().set_commands(
                "invoice_line_ids",
                [Command::update(
                    rec(line),
                    RawVals::new().set_commands("tax_ids", [Command::clear()]),
                )],
            ),
        )
        .await
        .unwrap();
    let found = items(&entry, &mut tx).await;
    assert!(!found.iter().any(|r| r.0 == "tax"), "{found:?}");
    assert_eq!(decimal(&entry, &mut tx, "amount_total").await, d("200"));
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_zero_tax_makes_no_tax_line() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Zero").await;
    let zero = tax(
        &mut tx,
        b.company,
        "VAT 0%",
        "0",
        "sale",
        b.sale_tax_account,
    )
    .await;
    let entry = moves(&tx, b.company)
        .create(
            &mut tx,
            vec![invoice(
                "out_invoice",
                b.customer,
                vec![product_line("1", "1000", "0", &[zero])],
            )],
        )
        .await
        .unwrap();
    let kinds: Vec<String> = items(&entry, &mut tx)
        .await
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(kinds, ["payment_term", "product"]);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn missing_configuration_is_refused_by_name() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Config").await;
    let model = moves(&tx, b.company);
    let mut vals = invoice(
        "out_invoice",
        b.customer,
        vec![product_line("1", "10", "0", &[])],
    );
    vals.insert("journal_id".into(), Cell::Id(b.purchase));
    let err = try_create(&model, &mut tx, vals).await.unwrap_err();
    assert!(err.to_string().contains("needs a sale journal"), "{err}");

    let bare = journal(&mut tx, b.company, "INV2", "sale", None).await;
    let mut vals = invoice(
        "out_invoice",
        b.customer,
        vec![product_line("1", "10", "0", &[])],
    );
    vals.insert("journal_id".into(), Cell::Id(bare));
    let err = try_create(&model, &mut tx, vals).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UserError);
    assert!(err.to_string().contains("default account"), "{err}");
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_receivable_account_comes_from_the_invoice_company_only() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let other = books(&mut tx, "Other").await;
    let mine = company(&mut tx, "Mine").await;
    let income = account(&mut tx, mine, "400000", "income").await;
    journal(&mut tx, mine, "INV", "sale", Some(income)).await;
    let model = moves(&tx, mine);
    let vals = invoice(
        "out_invoice",
        other.customer,
        vec![product_line("1", "10", "0", &[])],
    );
    let err = try_create(&model, &mut tx, vals.clone()).await.unwrap_err();
    assert!(err.to_string().contains("no receivable account"), "{err}");
    let receivable = account(&mut tx, mine, "121000", "asset_receivable").await;
    let entry = try_create(&model, &mut tx, vals).await.unwrap();
    let term = items(&entry, &mut tx)
        .await
        .into_iter()
        .find(|r| r.0 == "payment_term")
        .unwrap();
    assert_eq!(term.1, receivable);
    assert_ne!(term.1, other.receivable);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn posting_checks_list_every_problem() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Checks").await;
    let model = moves(&tx, b.company);
    let good = model
        .create(
            &mut tx,
            vec![invoice(
                "out_invoice",
                b.customer,
                vec![product_line("1", "10", "0", &[])],
            )],
        )
        .await
        .unwrap();
    let env = tx.env().with_company(b.company).unwrap();
    check_postable(&mut tx, &env, &good.ids()).await.unwrap();

    let empty = model
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("move_type", text("out_invoice"))],
        )
        .await
        .unwrap();
    let negative = model
        .create(
            &mut tx,
            vec![invoice(
                "out_invoice",
                b.customer,
                vec![product_line("-1", "10", "0", &[])],
            )],
        )
        .await
        .unwrap();
    let mut bill = invoice(
        "in_invoice",
        b.customer,
        vec![product_line("1", "10", "0", &[])],
    );
    bill.remove("invoice_date");
    let bill = model.create(&mut tx, vec![bill]).await.unwrap();
    let ids = [empty.ids(), negative.ids(), bill.ids()].concat();
    let err = check_postable(&mut tx, &env, &ids).await.unwrap_err();
    let message = err.to_string();
    assert_eq!(err.kind(), ErrorKind::UserError);
    for part in [
        "choose the customer",
        "add at least one product line",
        "the total is negative",
        "enter the bill date",
    ] {
        assert!(message.contains(part), "{part:?} missing in {message}");
    }
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn before_post_dates_customer_documents_only() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Dates").await;
    let model = moves(&tx, b.company);
    let mut sale = invoice(
        "out_invoice",
        b.customer,
        vec![product_line("1", "10", "0", &[])],
    );
    sale.remove("invoice_date");
    let sale = model.create(&mut tx, vec![sale]).await.unwrap();
    let mut bill = invoice(
        "in_invoice",
        b.customer,
        vec![product_line("1", "10", "0", &[])],
    );
    bill.remove("invoice_date");
    let bill = model.create(&mut tx, vec![bill]).await.unwrap();
    let env = tx.env().with_company(b.company).unwrap();
    before_post(&mut tx, &env, &[sale.ids(), bill.ids()].concat())
        .await
        .unwrap();
    let today = env.context_today().unwrap();
    assert_eq!(
        sale.cell(&mut tx, "invoice_date").await.unwrap(),
        Cell::Date(today)
    );
    assert_eq!(sale.cell(&mut tx, "date").await.unwrap(), Cell::Date(today));
    assert_eq!(
        sale.cell(&mut tx, "invoice_date_due").await.unwrap(),
        Cell::Date(today)
    );
    assert!(!matches!(
        bill.cell(&mut tx, "invoice_date").await.unwrap(),
        Cell::Date(_)
    ));
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// Plain journal entries are not touched by the invoice sync or checks.
#[tokio::test]
async fn journal_entries_are_left_alone() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Entries").await;
    let misc = journal(&mut tx, b.company, "MISC", "general", None).await;
    let entry = moves(&tx, b.company)
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("journal_id", Cell::Id(misc))],
        )
        .await
        .unwrap();
    assert_eq!(
        entry.cell(&mut tx, "move_type").await.unwrap(),
        text("entry")
    );
    assert!(items(&entry, &mut tx).await.is_empty());
    let env = tx.env().with_company(b.company).unwrap();
    check_postable(&mut tx, &env, &entry.ids()).await.unwrap();
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn action_post_runs_the_invoice_checks() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Wired").await;
    let model = moves(&tx, b.company);
    let mut bill = invoice(
        "in_invoice",
        b.customer,
        vec![product_line("1", "10", "0", &[])],
    );
    bill.remove("invoice_date");
    let bill = model.create(&mut tx, vec![bill]).await.unwrap();
    let err = bill
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UserError);
    let message = err.to_string();
    assert!(message.contains("enter the bill date"), "{message}");
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// receivable lines to the new currency, the balance converted at its rate; sections and notes
/// keep theirs)
#[tokio::test]
async fn a_currency_change_moves_every_amount_line_to_the_new_currency() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx, "Currency").await;
    tx.env()
        .records("res.currency.rate.type")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Spot"))
                    .set_cell("code", text("spot"))
                    .set_cell("date_rule", text("strictly_before"))
                    .set_cell("later_fallback", Cell::Bool(true)),
            ],
        )
        .await
        .unwrap();
    let foreign = tx
        .env()
        .records("res.currency")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("XEU"))
                    .set_cell("symbol", text("XEU"))
                    .set_cell("rounding", Cell::Decimal(d("0.01")))
                    .set_cell("active", Cell::Bool(true)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    // two units of the foreign currency per unit of the company currency
    tx.env()
        .with_company(b.company)
        .unwrap()
        .records("res.currency.rate")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("currency_id", Cell::Id(foreign))
                    .set_cell("company_id", Cell::Id(b.company))
                    .set_cell("name", Cell::Date(Date::parse_odoo("2026-01-01").unwrap()))
                    .set_cell("rate", Cell::Decimal(d("2"))),
            ],
        )
        .await
        .unwrap();
    let entry = moves(&tx, b.company)
        .create(
            &mut tx,
            vec![invoice(
                "out_invoice",
                b.customer,
                vec![product_line("1", "100", "0", &[b.sale_tax])],
            )],
        )
        .await
        .unwrap();
    entry
        .write(
            &mut tx,
            RawVals::new().set_cell("currency_id", Cell::Id(foreign)),
        )
        .await
        .unwrap();
    tx.flush().await.unwrap();
    let id = entry.ids()[0];
    let rows: Vec<(String, i64, Decimal, Decimal)> = tx
        .query(
            "SELECT display_type, currency_id::int8, amount_currency::text, balance::text \
             FROM account_move_line WHERE move_id = $1 ORDER BY display_type",
            &[&id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get(0),
                r.get(1),
                d(&r.get::<_, String>(2)),
                d(&r.get::<_, String>(3)),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("payment_term".to_owned(), foreign, d("115"), d("57.5")),
            ("product".to_owned(), foreign, d("-100"), d("-50")),
            ("tax".to_owned(), foreign, d("-15"), d("-7.5")),
        ]
    );
    assert_eq!(decimal(&entry, &mut tx, "amount_total").await, d("115"));
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}
