// SPDX-License-Identifier: MIT OR Apache-2.0
//! The printed accounting documents (`addons/account/views/account_reports.xml`) on a full
//! install: customer invoice, credit note, vendor bill, a cancelled invoice, an invoice in another
//! currency, a 60-line invoice over several pages, a journal entry and an invoice of a second
//! company printed from a session in the first, printed through the report service and read back
//! from the PDF; and the same documents printed by an accounting user.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::fs;
use std::path::{Path, PathBuf};

use erp_addon_account::fiscal::calendar::FiscalYearEnd;
use erp_addon_account::fiscal::ensure_periods_with;
use erp_addon_account::fiscal::settings::FiscalCalendar;
use erp_modules::system::{InitOptions, ModuleSystem};
use erp_modules::{AddonIndex, ModuleName};
use erp_report::LayoutEngine;
use erp_sdk::prelude::*;
use erp_sdk::{BeginOptions, Command, Context, Orm, SUPERUSER_ID};
use erp_testkit::TestDb;
use tokio_postgres::types::ToSql;

#[path = "support/report_access.rs"]
mod report_access;
#[path = "support/report_frame.rs"]
mod report_frame;

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
        .require_sql_objects([&erp_addon_account::reconcile::history_guard::MANIFEST])
        .require_sql_uninstall_guards([&erp_addon_account::reconcile::history_guard::UNINSTALL])
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
    (db, outcome.orm)
}

fn d(y: i32, m: u32, day: u32) -> Date {
    Date::from_ymd(y, m, day).unwrap()
}

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

fn num(value: &str) -> Cell {
    Cell::Decimal(value.parse().unwrap())
}

async fn one(tx: &mut Transaction, sql: &str, params: &[&(dyn ToSql + Sync)]) -> i64 {
    tx.query(sql, params).await.unwrap()[0].get(0)
}

async fn create(tx: &mut Transaction, model: &str, vals: RawVals) -> i64 {
    tx.env()
        .records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

async fn account(tx: &mut Transaction, company: i64, code: &str, name: &str, kind: &str) -> i64 {
    let vals = RawVals::new()
        .set_cell("name", text(name))
        .set_cell("code", text(code))
        .set_cell("account_type", text(kind));
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.account")
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

/// A journal with monthly numbers of four digits, written directly so its numbering is fixed.
async fn journal(tx: &mut Transaction, company: i64, code: &str, kind: &str, account: i64) {
    tx.query(
        "INSERT INTO account_journal (name, code, active, type, company_id, sequence, \
         refund_sequence, payment_sequence, sequence_reset, sequence_padding, color, \
         show_on_dashboard, default_account_id) \
         VALUES (jsonb_build_object('en_US', $2::text), $2::text, true, $3::text, $1, 10, \
         false, false, 'month', 4, 0, true, $4) RETURNING id::int8",
        &[&company, &code, &kind, &account],
    )
    .await
    .unwrap();
}

/// A percent tax of `group`, booked on `account`, written directly as in
/// `crates/erp-cli/tests/account_posting.rs`.
async fn tax(
    tx: &mut Transaction,
    company: i64,
    (name, label, rate, use_): (&str, &str, i64, &str),
    group: i64,
    account: i64,
) -> i64 {
    let country = one(
        tx,
        "SELECT id::int8 FROM res_country ORDER BY id LIMIT 1",
        &[],
    )
    .await;
    let tax = one(
        tx,
        "INSERT INTO account_tax (name, invoice_label, type_tax_use, amount_type, active, company_id, \
         root_company_id, sequence, amount, include_base_amount, is_base_affected, tax_group_id, \
         country_id, repartition_serial, analytic) \
         VALUES (jsonb_build_object('en_US', $4::text), jsonb_build_object('en_US', $5::text), \
         $6::text, 'percent', true, $1, $1, 1, $7, false, true, $2, $3, 0, false) RETURNING id::int8",
        &[
            &company,
            &group,
            &country,
            &name,
            &label,
            &use_,
            &Decimal::from(rate),
        ],
    )
    .await;
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

async fn tax_group(tx: &mut Transaction, company: i64, name: &str, sequence: i32) -> i64 {
    one(
        tx,
        "INSERT INTO account_tax_group (name, sequence, company_id) \
         VALUES (jsonb_build_object('en_US', $2::text), $3, $1) RETURNING id::int8",
        &[&company, &name, &sequence],
    )
    .await
}

struct Books {
    vat: i64,
    luxury: i64,
    purchase_vat: i64,
    customer: i64,
    vendor: i64,
    bank: i64,
    income: i64,
}

async fn books(tx: &mut Transaction, company: i64) -> Books {
    let income = account(tx, company, "400000", "Product Sales", "income").await;
    let expense = account(tx, company, "600000", "Office Expenses", "expense").await;
    account(tx, company, "121000", "Receivable", "asset_receivable").await;
    account(tx, company, "211000", "Payable", "liability_payable").await;
    let bank = account(tx, company, "101000", "Bank", "asset_cash").await;
    let sale_tax = account(tx, company, "251000", "VAT Payable", "liability_current").await;
    let purchase_tax = account(tx, company, "131000", "VAT Receivable", "asset_current").await;
    journal(tx, company, "INV", "sale", income).await;
    journal(tx, company, "BILL", "purchase", expense).await;
    journal(tx, company, "MISC", "general", bank).await;
    let vat_group = tax_group(tx, company, "VAT", 10).await;
    let luxury_group = tax_group(tx, company, "Luxury Tax", 20).await;
    let vat = tax(
        tx,
        company,
        ("VAT 11%", "11%", 11, "sale"),
        vat_group,
        sale_tax,
    )
    .await;
    let luxury = tax(
        tx,
        company,
        ("Luxury 10%", "Lux 10%", 10, "sale"),
        luxury_group,
        sale_tax,
    )
    .await;
    let purchase_vat = tax(
        tx,
        company,
        ("VAT 11% (purchase)", "11%", 11, "purchase"),
        vat_group,
        purchase_tax,
    )
    .await;
    let country = create(
        tx,
        "res.country",
        RawVals::new()
            .set_cell("name", text("Testland"))
            .set_cell("code", text("ZZ")),
    )
    .await;
    let customer = create(
        tx,
        "res.partner",
        RawVals::new()
            .set_cell("name", text("Sinar Abadi"))
            .set_cell("street", text("Jalan Asia Afrika 8"))
            .set_cell("zip", text("40112"))
            .set_cell("city", text("Bandung"))
            .set_cell("country_id", Cell::Id(country))
            .set_cell("vat", text("02.345.678.9-012.000")),
    )
    .await;
    let vendor = create(
        tx,
        "res.partner",
        RawVals::new()
            .set_cell("name", text("Kertas Nusantara"))
            .set_cell("city", text("Surabaya")),
    )
    .await;
    Books {
        vat,
        luxury,
        purchase_vat,
        customer,
        vendor,
        bank,
        income,
    }
}

fn product(name: &str, quantity: &str, price: &str, discount: &str, taxes: &[i64]) -> Command {
    Command::create(
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("quantity", num(quantity))
            .set_cell("price_unit", num(price))
            .set_cell("discount", num(discount))
            .set_commands(
                "tax_ids",
                [Command::set(
                    taxes.iter().map(|t| RecId::from_real(*t).unwrap()),
                )],
            ),
    )
}

fn text_line(kind: &str, name: &str) -> Command {
    Command::create(
        RawVals::new()
            .set_cell("display_type", text(kind))
            .set_cell("name", text(name)),
    )
}

fn document(move_type: &str, partner: i64, on: Date, lines: Vec<Command>) -> RawVals {
    RawVals::new()
        .set_cell("move_type", text(move_type))
        .set_cell("partner_id", Cell::Id(partner))
        .set_cell("invoice_date", Cell::Date(on))
        .set_commands("invoice_line_ids", lines)
}

async fn new_move(tx: &mut Transaction, company: i64, vals: RawVals) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move")
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
}

async fn call(record: &Records, tx: &mut Transaction, method: &'static str) {
    record.call::<(), ()>(tx, method, ()).await.unwrap();
}

async fn str_of(record: &Records, tx: &mut Transaction, field: &str) -> String {
    match record.cell(tx, field).await.unwrap() {
        Cell::Str(value) => value.to_string(),
        other => panic!("{field}: {other:?}"),
    }
}

/// A monetary field of `record` as the en_US language prints an amount.
async fn amount(record: &Records, tx: &mut Transaction, field: &str) -> String {
    let Cell::Decimal(value) = record.cell(tx, field).await.unwrap() else {
        panic!("{field} is not a number");
    };
    fmt_amount(value.to_string().parse().unwrap())
}

/// Text of every page of a PDF without white space, read back with an independent parser.
fn page_texts(bytes: &[u8]) -> Vec<String> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    pdf.get_pages()
        .keys()
        .map(|page| {
            let text = pdf.extract_text(&[*page]).unwrap();
            text.chars().filter(|c| !c.is_whitespace()).collect()
        })
        .collect()
}

fn squeeze(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whether `value` follows `label` on `page` within a currency symbol's room, so an amount is read
/// next to its label wherever the currency puts its symbol.
fn follows(page: &str, label: &str, value: &str) -> bool {
    page.match_indices(label).any(|(at, _)| {
        let rest: String = page[at + label.len()..]
            .chars()
            .take(value.chars().count() + 6)
            .collect();
        rest.contains(value)
    })
}

/// What one print gave: the HTML preview, the text of each PDF page and the file name.
struct Printed {
    html: String,
    pages: Vec<String>,
    file: String,
}

async fn print(tx: &mut Transaction, report: &str, ids: &[i64], keep: &str) -> Printed {
    let action = erp_addon_base::report_print::find(tx, report)
        .await
        .unwrap();
    erp_addon_base::report_print::check_records(tx, &action, None, ids)
        .await
        .unwrap();
    let document = erp_addon_base::report_print::prepare(tx, &action, ids)
        .await
        .unwrap();
    let file = erp_addon_base::report_print::file_name(tx, &action, ids, "pdf")
        .await
        .unwrap();
    let html = erp_report::render_html(&document, &[]).unwrap();
    let fonts = addons_dir().join("report_fonts");
    let engine = erp_report_typst::TypstEngine::new(&fonts, erp_report::Limits::default()).unwrap();
    let pdf = engine
        .render(&report_frame::through_frame(&document))
        .unwrap();
    // Kept in the test scratch folder for a look at the layout.
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::write(out.join(format!("account-{keep}.pdf")), pdf.bytes()).unwrap();
    Printed {
        html,
        pages: page_texts(pdf.bytes()),
        file,
    }
}

/// report service; title by type, partner address, dates, lines with section and note, a total
/// row per tax group, the payment line, and the journal entry with its items)
#[test]
fn accounting_documents_print_with_their_titles_lines_and_totals() {
    big_stack(accounting_documents_body);
}

async fn accounting_documents_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = one(
        &mut tx,
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
        &[],
    )
    .await;
    let currency = one(
        &mut tx,
        "SELECT currency_id::int8 FROM res_company WHERE id = $1",
        &[&company],
    )
    .await;
    let other = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Lautan Biru Trading"))
            .set_cell("currency_id", Cell::Id(currency))
            .set_cell("street", text("Jalan Pemuda 5"))
            .set_cell("zip", text("50132"))
            .set_cell("city", text("Semarang")),
    )
    .await;
    tx.commit().await.unwrap();
    for each in [company, other] {
        ensure_periods_with(
            &orm,
            BeginOptions::new(SUPERUSER_ID),
            &FiscalCalendar::new(FiscalYearEnd::new(31, 12).unwrap(), 12).unwrap(),
            each,
            d(2024, 1, 1),
            d(2024, 3, 31),
        )
        .await
        .unwrap();
    }
    let mut tx = orm
        .begin_with(SUPERUSER_ID, Context::new().with_company_id(company))
        .await
        .unwrap();
    tx.env()
        .records("res.company")
        .unwrap()
        .browse(vec![company])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new()
                .set_cell("street", text("Jalan Merdeka 12"))
                .set_cell("zip", text("40111"))
                .set_cell("city", text("Bandung"))
                .set_cell("phone", text("+62 22 555 0100"))
                .set_cell("email", text("billing@example.com"))
                .set_cell("vat", text("01.234.567.8-901.000")),
        )
        .await
        .unwrap();
    let b = books(&mut tx, company).await;
    let on = d(2024, 2, 10);

    // Both reports are in the Print menu of journal entries.
    let listed = erp_addon_base::report_print::printable(&mut tx, "account.move")
        .await
        .unwrap();
    for (report, label) in [
        ("account.report_invoice", "Invoice"),
        ("account.report_move", "Journal Entry"),
    ] {
        assert!(
            listed.contains(&(report.to_owned(), label.to_owned())),
            "{listed:?}"
        );
    }

    // A posted customer invoice: a section, two product lines (one discounted, one with two
    // taxes of two groups) and a note.
    let invoice = new_move(
        &mut tx,
        company,
        document(
            "out_invoice",
            b.customer,
            on,
            vec![
                text_line("line_section", "Consulting services"),
                product(
                    "Workshop facilitation, two days on site",
                    "3",
                    "1500",
                    "10",
                    &[b.vat],
                ),
                product("Executive chair", "2", "2750", "0", &[b.vat, b.luxury]),
                text_line("line_note", "Travel costs are included."),
            ],
        ),
    )
    .await;
    call(&invoice, &mut tx, "action_post").await;
    let number = str_of(&invoice, &mut tx, "name").await;
    let printed = print(&mut tx, "account.report_invoice", &invoice.ids(), "invoice").await;
    let page = &printed.pages[0];
    assert_eq!(printed.pages.len(), 1);
    assert_eq!(
        printed.file,
        format!("Invoice - {}.pdf", number.replace('/', "_"))
    );
    for expected in [
        format!("Invoice{}", squeeze(&number)),
        "Customer".into(),
        "SinarAbadi".into(),
        "JalanAsiaAfrika8".into(),
        "40112Bandung".into(),
        "Testland".into(),
        "02.345.678.9-012.000".into(),
        "InvoiceDate".into(),
        "DueDate".into(),
        "DescriptionQuantityUnitPriceDisc.%TaxesAmount".into(),
        "Consultingservices".into(),
        "Executivechair".into(),
        "11%Lux10%".into(),
        "Travelcostsareincluded.".into(),
        "UntaxedAmount".into(),
        "LuxuryTax".into(),
        squeeze(&amount(&invoice, &mut tx, "amount_untaxed").await),
        squeeze(&amount(&invoice, &mut tx, "amount_total").await),
        format!("Pleasequote{}", squeeze(&number)),
    ] {
        assert!(page.contains(&expected), "{expected}: {page}");
    }
    // A final invoice has no status mark, and nothing was paid yet.
    for absent in ["Draft", "Cancelled", "AmountDue", "Paid"] {
        assert!(!page.contains(absent), "{absent}: {page}");
    }
    // One total row per tax group, in the order of the tax lines; the luxury tax is 10 % of the
    // chair line, the VAT 11 % of both lines.
    assert!(follows(page, "LuxuryTax", &fmt_amount(550.0)), "{page}");
    let vat = fmt_amount((4050.0 + 5500.0) * 0.11);
    assert!(follows(page, "VAT", &vat), "{vat}: {page}");
    let total = amount(&invoice, &mut tx, "amount_total").await;
    assert!(follows(page, "Total", &total), "{total}: {page}");
    // The section and the note span the six columns of the lines table.
    assert_eq!(
        printed.html.matches("colspan=\"6\"").count(),
        2,
        "{}",
        printed.html
    );

    // A draft credit note without a discount column.
    let refund = new_move(
        &mut tx,
        company,
        document(
            "out_refund",
            b.customer,
            on,
            vec![product("Returned chair", "1", "2750", "0", &[b.vat])],
        ),
    )
    .await;
    let printed = print(
        &mut tx,
        "account.report_invoice",
        &refund.ids(),
        "credit-note",
    )
    .await;
    let page = &printed.pages[0];
    for expected in [
        "CreditNote",
        "Draft",
        "CreditNoteDate",
        "DescriptionQuantityUnitPriceTaxesAmount",
        "Returnedchair",
    ] {
        assert!(page.contains(expected), "{expected}: {page}");
    }
    assert!(!page.contains("Disc."), "{page}");
    assert!(!page.contains("Pleasequote"), "{page}");
    assert_eq!(printed.file, "Credit Note - Draft.pdf");

    // A posted vendor bill with its reference.
    let bill = new_move(
        &mut tx,
        company,
        document(
            "in_invoice",
            b.vendor,
            on,
            vec![product(
                "Copy paper A4, 80 g",
                "40",
                "52.5",
                "0",
                &[b.purchase_vat],
            )],
        )
        .set_cell("ref", text("BILL-778")),
    )
    .await;
    call(&bill, &mut tx, "action_post").await;
    let printed = print(
        &mut tx,
        "account.report_invoice",
        &bill.ids(),
        "vendor-bill",
    )
    .await;
    let page = &printed.pages[0];
    let bill_number = str_of(&bill, &mut tx, "name").await;
    for expected in [
        format!("VendorBill{}", squeeze(&bill_number)),
        "Vendor".into(),
        "KertasNusantara".into(),
        "BillDate".into(),
        "BILL-778".into(),
        squeeze(&amount(&bill, &mut tx, "amount_total").await),
    ] {
        assert!(page.contains(&expected), "{expected}: {page}");
    }
    assert!(!page.contains("Pleasequote"), "{page}");

    // A cancelled invoice says so.
    let cancelled = new_move(
        &mut tx,
        company,
        document(
            "out_invoice",
            b.customer,
            on,
            vec![product("Desk lamp", "1", "300", "0", &[b.vat])],
        ),
    )
    .await;
    call(&cancelled, &mut tx, "button_cancel").await;
    let printed = print(
        &mut tx,
        "account.report_invoice",
        &cancelled.ids(),
        "cancelled",
    )
    .await;
    assert!(
        printed.pages[0].contains("Cancelled"),
        "{}",
        printed.pages[0]
    );

    // An invoice in another currency prints its amounts in that currency and names it.
    let foreign = foreign_currency(&mut tx, company).await;
    let abroad = new_move(
        &mut tx,
        company,
        document(
            "out_invoice",
            b.customer,
            on,
            vec![product("Licence, one year", "1", "1200", "0", &[b.vat])],
        )
        .set_cell("currency_id", Cell::Id(foreign)),
    )
    .await;
    let printed = print(&mut tx, "account.report_invoice", &abroad.ids(), "foreign").await;
    let page = &printed.pages[0];
    assert!(page.contains("CurrencyXEU"), "{page}");
    assert!(
        follows(page, "UntaxedAmount", &fmt_amount(1200.0)),
        "{page}"
    );
    let total = amount(&abroad, &mut tx, "amount_total").await;
    assert!(follows(page, "Total", &total), "{total}: {page}");

    // A tax amount without a tax line of a group still prints, as one row of taxes.
    let ungrouped = new_move(
        &mut tx,
        company,
        document(
            "out_invoice",
            b.customer,
            on,
            vec![product("Stamp duty item", "1", "1200", "0", &[])],
        ),
    )
    .await;
    tx.query(
        "UPDATE account_move SET amount_tax = 7, amount_total = amount_untaxed + 7 WHERE id = $1",
        &[&ungrouped.ids()[0]],
    )
    .await
    .unwrap();
    tx.invalidate_all().await.unwrap();
    let printed = print(
        &mut tx,
        "account.report_invoice",
        &ungrouped.ids(),
        "ungrouped-tax",
    )
    .await;
    let page = &printed.pages[0];
    // the column title and the row
    assert_eq!(page.matches("Taxes").count(), 2, "{page}");
    assert!(follows(page, "Taxes", &fmt_amount(7.0)), "{page}");
    assert!(follows(page, "Total", &fmt_amount(1207.0)), "{page}");

    // 60 lines: the header repeats on every page, no line is split, the totals and the payment
    // line close the last page; a part payment shows the paid amount and the amount due.
    let lines: Vec<Command> = (1..=60)
        .map(|n| {
            product(
                &format!(
                    "Rowstart{n} office supplies delivered to the branch, packed per floor Rowend{n}."
                ),
                "1",
                "100",
                "0",
                &[b.vat],
            )
        })
        .collect();
    let long = new_move(
        &mut tx,
        company,
        document("out_invoice", b.customer, on, lines),
    )
    .await;
    call(&long, &mut tx, "action_post").await;
    tx.query(
        "UPDATE account_move SET amount_residual = amount_total - 1000 WHERE id = $1",
        &[&long.ids()[0]],
    )
    .await
    .unwrap();
    tx.invalidate_all().await.unwrap();
    let printed = print(&mut tx, "account.report_invoice", &long.ids(), "multipage").await;
    let pages = &printed.pages;
    assert!(pages.len() >= 2, "60 lines fill more than one page");
    let last = pages.len() - 1;
    for (index, page) in pages.iter().enumerate() {
        if page.contains("Rowstart") {
            assert!(
                page.contains("DescriptionQuantityUnitPriceTaxesAmount"),
                "page {index}: {page}"
            );
        }
        for closing in ["UntaxedAmount", "AmountDue", "Pleasequote"] {
            assert_eq!(
                page.contains(closing),
                index == last,
                "{closing} on page {index}"
            );
        }
    }
    for n in 1..=60 {
        let start = format!("Rowstart{n}office");
        let end = format!("Rowend{n}.");
        let page = pages
            .iter()
            .position(|p| p.contains(&start))
            .unwrap_or_else(|| panic!("line {n} is printed"));
        assert!(pages[page].contains(&end), "line {n} ends on page {page}");
    }
    assert!(
        follows(&pages[last], "Paid", &fmt_amount(1000.0)),
        "{}",
        pages[last]
    );
    let due = amount(&long, &mut tx, "amount_residual").await;
    assert!(follows(&pages[last], "AmountDue", &due), "{}", pages[last]);

    // A posted journal entry: its items with account, label, debit and credit, and the totals.
    let entry = new_move(
        &mut tx,
        company,
        RawVals::new()
            .set_cell("date", Cell::Date(on))
            .set_cell("ref", text("Owner deposit"))
            .set_commands(
                "line_ids",
                vec![
                    Command::create(
                        RawVals::new()
                            .set_cell("account_id", Cell::Id(b.bank))
                            .set_cell("name", text("Deposit"))
                            .set_cell("debit", num("2500")),
                    ),
                    Command::create(
                        RawVals::new()
                            .set_cell("account_id", Cell::Id(b.income))
                            .set_cell("partner_id", Cell::Id(b.customer))
                            .set_cell("name", text("Deposit"))
                            .set_cell("credit", num("2500")),
                    ),
                ],
            ),
    )
    .await;
    call(&entry, &mut tx, "action_post").await;
    let entry_number = str_of(&entry, &mut tx, "name").await;
    for report in ["account.report_move", "account.report_invoice"] {
        let printed = print(&mut tx, report, &entry.ids(), "journal-entry").await;
        let page = &printed.pages[0];
        for expected in [
            format!("JournalEntry{}", squeeze(&entry_number)),
            "AccountingDate".into(),
            "MISC".into(),
            "Ownerdeposit".into(),
            "AccountPartnerLabelDebitCredit".into(),
            "101000".into(),
            "SinarAbadi".into(),
        ] {
            assert!(page.contains(&expected), "{report} {expected}: {page}");
        }
        // Debit and credit totals, each the sum of its column.
        let sum = fmt_amount(2500.0);
        assert!(follows(page, "Total", &sum), "{report}: {page}");
        assert_eq!(page.matches(&sum).count(), 4, "{report}: {page}");
        assert_eq!(
            printed.file,
            format!("Journal Entry - {}.pdf", entry_number.replace('/', "_"))
        );
    }

    // An invoice of the second company, printed from this session in the first company: the
    // letterhead is the invoice's company, not the session's.
    let other_income = account(&mut tx, other, "400000", "Survey Income", "income").await;
    account(&mut tx, other, "121000", "Receivable", "asset_receivable").await;
    account(&mut tx, other, "211000", "Payable", "liability_payable").await;
    journal(&mut tx, other, "INV", "sale", other_income).await;
    let foreign_invoice = new_move(
        &mut tx,
        other,
        document(
            "out_invoice",
            b.customer,
            on,
            vec![product("Harbour survey", "1", "1200", "0", &[])],
        ),
    )
    .await;
    assert_eq!(tx.env().company_id().unwrap(), Some(company));
    let printed = print(
        &mut tx,
        "account.report_invoice",
        &foreign_invoice.ids(),
        "company-b",
    )
    .await;
    let page = &printed.pages[0];
    for expected in [
        "LautanBiruTrading",
        "JalanPemuda5",
        "50132Semarang",
        "Harboursurvey",
    ] {
        assert!(page.contains(expected), "{expected}: {page}");
    }
    for absent in [
        "MainCompany",
        "JalanMerdeka12",
        "billing@example.com",
        "01.234.567.8-901.000",
    ] {
        assert!(!page.contains(absent), "{absent}: {page}");
    }
    // A selection of both companies prints company by company, in the order given; one document
    // never mixes two letterheads.
    let action = erp_addon_base::report_print::find(&mut tx, "account.report_invoice")
        .await
        .unwrap();
    let both = [invoice.ids()[0], foreign_invoice.ids()[0]];
    assert_eq!(
        erp_addon_base::report_print::company_runs(&mut tx, &action, &both)
            .await
            .unwrap(),
        vec![(Some(company), vec![both[0]]), (Some(other), vec![both[1]])]
    );
    let error = erp_addon_base::report_print::prepare(&mut tx, &action, &both)
        .await
        .unwrap_err();
    assert!(
        error.message().contains("several companies"),
        "{}",
        error.message()
    );

    // A read-only accounting user of the first company prints its invoice and journal entry; a
    // user without an accounting role and an invoice of the second company are refused.
    report_access::admin(&mut tx, company).await;
    let accountant = report_access::user(
        &mut tx,
        "report-accountant",
        company,
        &[("account", "group_account_readonly")],
    )
    .await;
    let outsider = report_access::user(&mut tx, "report-account-outsider", company, &[]).await;
    tx.commit().await.unwrap();
    let users = report_access::policy_orm(&db, &orm).await;
    for (report, ids, title) in [
        (
            "account.report_invoice",
            invoice.ids(),
            format!("Invoice{}", squeeze(&number)),
        ),
        (
            "account.report_move",
            entry.ids(),
            format!("JournalEntry{}", squeeze(&entry_number)),
        ),
    ] {
        let pages = report_access::print_as(&users, accountant, company, report, &ids)
            .await
            .unwrap_or_else(|e| panic!("{report} as an accounting user: {e:?}"));
        assert!(pages[0].contains(&title), "{title}: {}", pages[0]);
        report_access::refused(
            report_access::print_as(&users, outsider, company, report, &ids).await,
            &format!("{report} as a user without an accounting role"),
        );
        report_access::refused(
            report_access::print_as(&users, accountant, company, report, &foreign_invoice.ids())
                .await,
            &format!("{report} of another company"),
        );
    }
    drop(users);
    drop(orm);
    db.cleanup().await.unwrap();
}

/// An amount with two decimals and comma thousands, as the en_US language prints it.
fn fmt_amount(value: f64) -> String {
    let fixed = format!("{value:.2}");
    let (whole, cents) = fixed.split_once('.').unwrap();
    let digits: Vec<char> = whole.chars().collect();
    let mut grouped = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(*c);
    }
    format!("{grouped}.{cents}")
}

/// A foreign currency `XEU` with a rate of 2 before the invoice date.
async fn foreign_currency(tx: &mut Transaction, company: i64) -> i64 {
    // The install ships the spot rate type the rate below uses.
    let foreign = create(
        tx,
        "res.currency",
        RawVals::new()
            .set_cell("name", text("XEU"))
            .set_cell("symbol", text("XEU"))
            .set_cell("rounding", num("0.01"))
            .set_cell("active", Cell::Bool(true)),
    )
    .await;
    tx.env()
        .with_company(company)
        .unwrap()
        .records("res.currency.rate")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("currency_id", Cell::Id(foreign))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("name", Cell::Date(d(2024, 1, 1)))
                    .set_cell("rate", num("2")),
            ],
        )
        .await
        .unwrap();
    foreign
}
