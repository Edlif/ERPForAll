// SPDX-License-Identifier: MIT OR Apache-2.0
//! Foreign-currency journal items in draft: the missing amount of a line in another
//! currency than the company's is converted through the `spot` rate strictly before the entry date
//! and rounded on the target currency (/D3). Posting comes with.
//!
//! Expected amounts come from `erp_addon_base::money::convert` (the base one-call conversion), not
//! from typed goldens, except the plain spec examples (1200 at rate 3 is 400).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
use erp_addon_base::money;
use erp_sdk::money::parse_decimal;
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

fn num(value: &str) -> Decimal {
    parse_decimal(value).unwrap()
}

fn amount(value: &str) -> Cell {
    Cell::Decimal(num(value))
}

fn day(y: i32, m: u32, d: u32) -> Date {
    Date::from_ymd(y, m, d).unwrap()
}

fn rid(id: i64) -> RecId {
    RecId::from_real(id).unwrap()
}

const SKIP: (&str, Value) = ("check_move_validity", Value::Bool(false));

/// A company in USD, its journal and accounts, the `spot` rate type and the foreign currencies:
/// EUR (0.01) at 3 from 2026-01-01 and 2 from 2026-07-01, JPY (1) at 150.5, CHF (0.01) at the
/// 6-decimal rate 0.912345, and XAU (0.01) without any rate.
struct Books {
    company: i64,
    journal: i64,
    bank: i64,
    income: i64,
    usd: i64,
    eur: i64,
    jpy: i64,
    chf: i64,
    xau: i64,
}

async fn currency(tx: &mut Transaction, code: &str, rounding: &str) -> i64 {
    tx.env()
        .records("res.currency")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text(code))
                    .set_cell("symbol", text(code))
                    .set_cell("rounding", amount(rounding))
                    .set_cell("active", Cell::Bool(true)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

async fn rate(tx: &mut Transaction, company: i64, currency: i64, on: Date, value: &str) {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("res.currency.rate")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("currency_id", Cell::Id(currency))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("name", Cell::Date(on))
                    .set_cell("rate", amount(value)),
            ],
        )
        .await
        .unwrap();
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

async fn books(tx: &mut Transaction) -> Books {
    tx.env()
        .records("res.currency.rate.type")
        .unwrap()
        .create(
            tx,
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
    let usd = currency(tx, "USD", "0.01").await;
    let eur = currency(tx, "EUR", "0.01").await;
    let jpy = currency(tx, "JPY", "1").await;
    let chf = currency(tx, "CHF", "0.01").await;
    let xau = currency(tx, "XAU", "0.01").await;
    let company = tx
        .env()
        .records("res.company")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Foreign"))
                    .set_cell("currency_id", Cell::Id(usd)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    rate(tx, company, eur, day(2026, 1, 1), "3").await;
    rate(tx, company, eur, day(2026, 7, 1), "2").await;
    rate(tx, company, jpy, day(2026, 1, 1), "150.5").await;
    rate(tx, company, chf, day(2026, 1, 1), "0.912345").await;
    // a miscellaneous journal written directly, as in move_drafts.rs: the journal rules read
    // declared settings that a harness without data files does not install
    let journal: i64 = tx
        .query(
            "INSERT INTO account_journal (name, code, active, type, company_id, sequence, \
             refund_sequence, payment_sequence, sequence_reset, sequence_padding, color, \
             show_on_dashboard) VALUES (jsonb_build_object('en_US', 'Misc'), 'MISC', true, \
             'general', $1, 10, false, false, 'month', 4, 0, true) RETURNING id::int8",
            &[&company],
        )
        .await
        .unwrap()[0]
        .get(0);
    let bank = account(tx, company, "1000").await;
    let income = account(tx, company, "4000").await;
    Books {
        company,
        journal,
        bank,
        income,
        usd,
        eur,
        jpy,
        chf,
        xau,
    }
}

fn moves(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move")
        .unwrap()
}

fn lines_model(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move.line")
        .unwrap()
}

/// A line command: `account`, `currency` and the given amount cells.
fn line(account: i64, currency: i64, amounts: &[(&'static str, &str)]) -> Command {
    let mut vals = RawVals::new()
        .set_cell("account_id", Cell::Id(account))
        .set_cell("currency_id", Cell::Id(currency));
    for (field, value) in amounts {
        vals.insert((*field).into(), amount(value));
    }
    Command::create(vals)
}

fn entry(books: &Books, on: Date, lines: Vec<Command>) -> RawVals {
    RawVals::new()
        .set_cell("journal_id", Cell::Id(books.journal))
        .set_cell("date", Cell::Date(on))
        .set_commands("line_ids", lines)
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

/// The lines of `entry`, one record each, in creation order.
async fn lines_of(entry: &Records, tx: &mut Transaction) -> Vec<Records> {
    let lines = entry.related(tx, "line_ids").await.unwrap();
    let mut ids = lines.ids();
    ids.sort_unstable();
    ids.into_iter()
        .map(|id| lines.browse([id]).unwrap())
        .collect()
}

/// `value` converted by base's one-call conversion for the books' company on `on`.
async fn converted(
    tx: &mut Transaction,
    books: &Books,
    value: Decimal,
    from: i64,
    to: i64,
    on: Date,
) -> Decimal {
    let env = tx.env().with_company(books.company).unwrap();
    money::convert(&env, tx, value, rid(from), rid(to), rid(books.company), on)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_balance_is_derived_from_the_amount_in_currency() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx).await;
    let on = day(2026, 6, 15);
    let created = moves(&tx, b.company)
        .create(
            &mut tx,
            vec![entry(
                &b,
                on,
                vec![
                    line(b.bank, b.eur, &[("amount_currency", "1200")]),
                    line(b.income, b.eur, &[("amount_currency", "-1200")]),
                ],
            )],
        )
        .await
        .unwrap();
    let lines = lines_of(&created, &mut tx).await;
    assert_eq!(decimal(&lines[0], &mut tx, "balance").await, num("400"));
    assert_eq!(decimal(&lines[1], &mut tx, "balance").await, num("-400"));
    assert_eq!(decimal(&lines[0], &mut tx, "debit").await, num("400"));
    assert_eq!(decimal(&lines[1], &mut tx, "credit").await, num("400"));

    // writing the amounts in currency through the entry re-derives both balances at its date
    try_write(
        &created,
        &mut tx,
        RawVals::new().set_commands(
            "line_ids",
            vec![
                Command::update(
                    rid(lines[0].ids()[0]),
                    RawVals::new().set_cell("amount_currency", amount("1500")),
                ),
                Command::update(
                    rid(lines[1].ids()[0]),
                    RawVals::new().set_cell("amount_currency", amount("-1500")),
                ),
            ],
        ),
    )
    .await
    .unwrap();
    assert_eq!(decimal(&lines[0], &mut tx, "balance").await, num("500"));
    assert_eq!(decimal(&lines[1], &mut tx, "balance").await, num("-500"));
    assert_eq!(
        decimal(&lines[0], &mut tx, "amount_currency").await,
        num("1500")
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn one_write_converts_each_line_at_its_own_rate() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx).await;
    let on = day(2026, 6, 15);
    let created = moves(&tx, b.company)
        .with_context([SKIP])
        .create(
            &mut tx,
            vec![entry(
                &b,
                on,
                vec![
                    line(b.bank, b.eur, &[("amount_currency", "3"), ("balance", "1")]),
                    line(
                        b.bank,
                        b.jpy,
                        &[("amount_currency", "301"), ("balance", "2")],
                    ),
                    line(b.income, b.usd, &[("balance", "-3")]),
                ],
            )],
        )
        .await
        .unwrap();
    let lines = lines_of(&created, &mut tx).await;
    let foreign = lines_model(&tx, b.company)
        .with_context([SKIP])
        .browse([lines[0].ids()[0], lines[1].ids()[0]])
        .unwrap();
    try_write(
        &foreign,
        &mut tx,
        RawVals::new().set_cell("amount_currency", amount("903")),
    )
    .await
    .unwrap();
    let in_eur = converted(&mut tx, &b, num("903"), b.eur, b.usd, on).await;
    let in_jpy = converted(&mut tx, &b, num("903"), b.jpy, b.usd, on).await;
    assert_eq!(in_eur, num("301"), "903 EUR at 3 is 301 USD");
    assert_ne!(in_eur, in_jpy);
    assert_eq!(decimal(&lines[0], &mut tx, "balance").await, in_eur);
    assert_eq!(decimal(&lines[1], &mut tx, "balance").await, in_jpy);
    // the company-currency line of the same entry keeps its amounts
    assert_eq!(decimal(&lines[2], &mut tx, "balance").await, num("-3"));
    assert_eq!(
        decimal(&lines[2], &mut tx, "amount_currency").await,
        num("-3")
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn each_amount_rounds_on_its_own_currency() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx).await;
    let on = day(2026, 6, 15);
    let model = moves(&tx, b.company).with_context([SKIP]);
    // balance only (as a debit): the amount in JPY is rounded to whole yen
    let created = model
        .create(
            &mut tx,
            vec![entry(
                &b,
                on,
                vec![
                    line(b.bank, b.jpy, &[("debit", "10.01")]),
                    // amount in JPY only: the balance is rounded to the cent
                    line(b.income, b.jpy, &[("amount_currency", "-1000")]),
                ],
            )],
        )
        .await
        .unwrap();
    let lines = lines_of(&created, &mut tx).await;
    let in_jpy = converted(&mut tx, &b, num("10.01"), b.usd, b.jpy, on).await;
    let in_usd = converted(&mut tx, &b, num("-1000"), b.jpy, b.usd, on).await;
    assert_eq!(in_jpy.fract(), Decimal::ZERO, "JPY has no decimals");
    assert_eq!(
        (in_usd * Decimal::from(100)).fract(),
        Decimal::ZERO,
        "USD rounds to the cent"
    );
    assert_eq!(decimal(&lines[0], &mut tx, "balance").await, num("10.01"));
    assert_eq!(decimal(&lines[0], &mut tx, "amount_currency").await, in_jpy);
    assert_eq!(decimal(&lines[1], &mut tx, "balance").await, in_usd);
    assert_eq!(
        decimal(&lines[1], &mut tx, "amount_currency").await,
        num("-1000")
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_six_decimal_rate_is_applied_exactly() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx).await;
    let on = day(2026, 6, 15);
    // the rate is stored exactly, so the conversion below sees all six decimals
    let stored: Decimal = tx
        .query(
            "SELECT rate FROM res_currency_rate WHERE currency_id = $1",
            &[&b.chf],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(stored, num("0.912345"));
    let created = moves(&tx, b.company)
        .with_context([SKIP])
        .create(
            &mut tx,
            vec![entry(
                &b,
                on,
                vec![line(b.bank, b.chf, &[("amount_currency", "1000")])],
            )],
        )
        .await
        .unwrap();
    let lines = lines_of(&created, &mut tx).await;
    let expected = converted(&mut tx, &b, num("1000"), b.chf, b.usd, on).await;
    assert_eq!(decimal(&lines[0], &mut tx, "balance").await, expected);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_currency_without_rates_is_refused_by_name() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx).await;
    let on = day(2026, 6, 15);
    let model = moves(&tx, b.company).with_context([SKIP]);
    let message = format!(
        "No exchange rate is defined for currency XAU for company {} on 2026-06-15. Add a rate \
         for this currency first.",
        b.company
    );
    let error = try_create(
        &model,
        &mut tx,
        entry(
            &b,
            on,
            vec![line(b.bank, b.xau, &[("amount_currency", "100")])],
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(error.message(), message);

    // with both amounts given nothing is converted, so the missing rate is not read
    let created = try_create(
        &model,
        &mut tx,
        entry(
            &b,
            on,
            vec![line(
                b.bank,
                b.xau,
                &[("amount_currency", "7"), ("balance", "10")],
            )],
        ),
    )
    .await
    .unwrap();
    let lines = lines_of(&created, &mut tx).await;
    let line = lines[0].with_context([SKIP]);
    // a later amount in currency alone needs the rate
    let error = try_write(
        &line,
        &mut tx,
        RawVals::new().set_cell("amount_currency", amount("8")),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(error.message(), message);
    assert_eq!(decimal(&line, &mut tx, "amount_currency").await, num("7"));
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn given_amounts_are_kept() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx).await;
    let on = day(2026, 6, 15);
    let created = moves(&tx, b.company)
        .with_context([SKIP])
        .create(
            &mut tx,
            vec![entry(
                &b,
                on,
                vec![line(
                    b.bank,
                    b.xau,
                    &[("amount_currency", "7"), ("balance", "10")],
                )],
            )],
        )
        .await
        .unwrap();
    let item = lines_of(&created, &mut tx).await[0].with_context([SKIP]);
    // both given: kept as given, no agreement check with the rate
    assert_eq!(decimal(&item, &mut tx, "balance").await, num("10"));
    assert_eq!(decimal(&item, &mut tx, "amount_currency").await, num("7"));
    // a balance alone keeps the stored amount in currency
    try_write(
        &item,
        &mut tx,
        RawVals::new().set_cell("balance", amount("12")),
    )
    .await
    .unwrap();
    assert_eq!(decimal(&item, &mut tx, "balance").await, num("12"));
    assert_eq!(decimal(&item, &mut tx, "amount_currency").await, num("7"));
    // both written: kept
    try_write(
        &item,
        &mut tx,
        RawVals::new()
            .set_cell("balance", amount("14"))
            .set_cell("amount_currency", amount("9")),
    )
    .await
    .unwrap();
    assert_eq!(decimal(&item, &mut tx, "balance").await, num("14"));
    assert_eq!(decimal(&item, &mut tx, "amount_currency").await, num("9"));
    // the currency alone is refused: it would keep both amounts under the new currency
    let refused = try_write(
        &item,
        &mut tx,
        RawVals::new().set_cell("currency_id", Cell::Id(b.eur)),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Validation);
    assert!(
        refused
            .message()
            .starts_with("Changing only the currency of a journal item that has an amount")
    );
    assert_eq!(
        item.cell(&mut tx, "currency_id").await.unwrap(),
        Cell::Id(b.xau)
    );
    assert_eq!(decimal(&item, &mut tx, "balance").await, num("14"));
    assert_eq!(decimal(&item, &mut tx, "amount_currency").await, num("9"));
    // a currency with an amount in currency converts in the new currency
    try_write(
        &item,
        &mut tx,
        RawVals::new()
            .set_cell("currency_id", Cell::Id(b.jpy))
            .set_cell("amount_currency", amount("903")),
    )
    .await
    .unwrap();
    let in_jpy = converted(&mut tx, &b, num("903"), b.jpy, b.usd, on).await;
    assert_eq!(decimal(&item, &mut tx, "balance").await, in_jpy);
    // neither amount on create: both zero, no rate read (XAU has none)
    let empty = moves(&tx, b.company)
        .with_context([SKIP])
        .create(&mut tx, vec![entry(&b, on, vec![line(b.bank, b.xau, &[])])])
        .await
        .unwrap();
    let empty = lines_of(&empty, &mut tx).await;
    assert_eq!(decimal(&empty[0], &mut tx, "balance").await, Decimal::ZERO);
    assert_eq!(
        decimal(&empty[0], &mut tx, "amount_currency").await,
        Decimal::ZERO
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn rounded_conversions_can_unbalance_an_entry() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx).await;
    let on = day(2026, 6, 15);
    let lines = || {
        vec![
            line(b.bank, b.eur, &[("amount_currency", "1")]),
            line(b.bank, b.eur, &[("amount_currency", "1")]),
            line(b.income, b.eur, &[("amount_currency", "-2")]),
        ]
    };
    let one = converted(&mut tx, &b, num("1"), b.eur, b.usd, on).await;
    let two = converted(&mut tx, &b, num("-2"), b.eur, b.usd, on).await;
    let difference = one + one + two;
    assert_ne!(difference, Decimal::ZERO, "balanced in EUR, not in USD");
    let model = moves(&tx, b.company);
    let error = try_create(&model, &mut tx, entry(&b, on, lines()))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    // the entry id in the message is the refused row's sequence value, so only the text around it
    // is fixed
    let refusal = error.message();
    assert!(
        refusal.starts_with("Debits and credits must be equal before saving; in entry ")
            && refusal.ends_with(&format!(" they differ by {difference}.")),
        "{refusal}"
    );
    // with the check skipped the draft saves with the converted balances
    let created = try_create(&model.with_context([SKIP]), &mut tx, entry(&b, on, lines()))
        .await
        .unwrap();
    let saved = lines_of(&created, &mut tx).await;
    assert_eq!(decimal(&saved[0], &mut tx, "balance").await, one);
    assert_eq!(decimal(&saved[1], &mut tx, "balance").await, one);
    assert_eq!(decimal(&saved[2], &mut tx, "balance").await, two);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn line_commands_convert_at_the_entry_date() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let b = books(&mut tx).await;
    let model = moves(&tx, b.company);
    // before 2026-07-01 the rate is 3, from then on 2 (and today, 2026-11 or later, is 2)
    let june = model
        .create(
            &mut tx,
            vec![entry(
                &b,
                day(2026, 6, 15),
                vec![
                    line(b.bank, b.eur, &[("amount_currency", "1200")]),
                    line(b.income, b.usd, &[("credit", "400")]),
                ],
            )],
        )
        .await
        .unwrap();
    let june = lines_of(&june, &mut tx).await;
    assert_eq!(decimal(&june[0], &mut tx, "balance").await, num("400"));
    // the rate dated on the entry day itself is not used yet (strictly before)
    let first_of_july = model
        .create(
            &mut tx,
            vec![entry(
                &b,
                day(2026, 7, 1),
                vec![
                    line(b.bank, b.eur, &[("amount_currency", "1200")]),
                    line(b.income, b.usd, &[("credit", "400")]),
                ],
            )],
        )
        .await
        .unwrap();
    let first_of_july = lines_of(&first_of_july, &mut tx).await;
    assert_eq!(
        decimal(&first_of_july[0], &mut tx, "balance").await,
        num("400")
    );
    let august = model
        .create(
            &mut tx,
            vec![entry(
                &b,
                day(2026, 8, 1),
                vec![
                    line(b.bank, b.eur, &[("amount_currency", "1200")]),
                    // a credit alone in EUR: the amount in currency follows at the same rate
                    line(b.income, b.eur, &[("credit", "600")]),
                ],
            )],
        )
        .await
        .unwrap();
    let august = lines_of(&august, &mut tx).await;
    assert_eq!(decimal(&august[0], &mut tx, "balance").await, num("600"));
    assert_eq!(
        decimal(&august[1], &mut tx, "amount_currency").await,
        num("-1200")
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}
