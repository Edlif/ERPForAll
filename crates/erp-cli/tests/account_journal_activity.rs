// SPDX-License-Identifier: MIT OR Apache-2.0
//! Posted activity through the installed report definition and caller ORM.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]

#[allow(
    clippy::panic,
    reason = "shared fixture readers assert the expected field type"
)]
#[path = "support/accounting.rs"]
mod accounting;

use accounting::*;
use erp_addon_reporting::line::{LineId, Segment};
use erp_addon_reporting::runtime;
use erp_modules::{InitOptions, ModuleName};
use erp_orm::{Command, UserId};
use erp_sdk::prelude::*;
use erp_sdk::{Context, Orm, SUPERUSER_ID};
use erp_testkit::TestDb;
use serde_json::{Value as Json, json};

async fn balanced_entry(
    tx: &mut Transaction,
    b: &Books,
    journal: i64,
    debit: i64,
    amount: &str,
) -> Records {
    let amount = num(amount);
    moves(tx, b.company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(d(2024, 2, 15)))
                    .set_cell("journal_id", Cell::Id(journal))
                    .set_commands(
                        "line_ids",
                        vec![
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(debit))
                                    .set_cell("balance", Cell::Decimal(amount)),
                            ),
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(b.income))
                                    .set_cell("balance", Cell::Decimal(-amount)),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap()
}

fn booked_amount(value: &Value) -> Decimal {
    match value {
        Value::Decimal(n) => Ok(*n),
        other => Err(other),
    }
    .expect("a booked journal amount must be a decimal")
}

fn numbers(line: &Json) -> Vec<Decimal> {
    line["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| num(c["value"].as_str().unwrap()))
        .collect()
}

/// The amounts of `line` by cell index; a blank cell (a debit or credit column that hides zero)
/// reads as zero, and a text or date cell has no amount (zero).
fn cell_values(line: &Json) -> Vec<Decimal> {
    line["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| match (&c["value"], c["kind"].as_str()) {
            (Json::Null, _) | (_, Some("text" | "date" | "empty")) => Decimal::ZERO,
            (value, _) => num(value.as_str().unwrap()),
        })
        .collect()
}

/// The index of the column `label` of column group `group` in resolved options.
fn column(options: &Json, group: u64, label: &str) -> usize {
    options["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c["group"] == group && c["label"] == label)
        .unwrap_or_else(|| panic!("no column {label} in group {group}"))
}

fn line_id(line: &Json) -> &str {
    line["id"].as_str().unwrap()
}

fn group_line(report: i64, field: &str, model: &str, id: i64) -> LineId {
    LineId::root(report).child(Segment::group(field, model, &id.to_string()))
}

fn total_line(report: i64) -> String {
    LineId::root(report).child(Segment::total()).to_string()
}

/// The lines of a native ledger report without its grand total, and the grand total (always last).
fn split_total(lines: &[Json], report: i64) -> (&[Json], &Json) {
    let (total, rest) = lines.split_last().expect("a report with rows has a total");
    assert_eq!(line_id(total), total_line(report));
    assert_eq!(total["kind"], "total");
    (rest, total)
}

async fn drilled_ids(
    env: &Env,
    tx: &mut Transaction,
    options: &Json,
    line: &str,
    cell: usize,
) -> Vec<i64> {
    let action = runtime::report_drill(env, tx, options, line, &json!({"cell":cell}))
        .await
        .unwrap();
    assert_eq!(action["res_model"], "account.move.line");
    let mut ids = env
        .records("account.move.line")
        .unwrap()
        .search(tx, &Domain::from_json(&action["domain"]).unwrap())
        .await
        .unwrap()
        .ids();
    ids.sort_unstable();
    ids
}

#[test]
fn posted_journal_activity_uses_booked_items_and_the_callers_scope() {
    big_stack(activity);
}

struct Fixture {
    _db: TestDb,
    orm: Orm,
    company: i64,
    other: i64,
    journal_report: i64,
    reader: UserId,
    outsider: UserId,
    february_items: Vec<i64>,
    period_items: Vec<i64>,
    books: Books,
    statutory_journal: i64,
    other_books: Books,
}

async fn fixture() -> Fixture {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let system = system(&db, None);
    let init = system
        .initialize(InitOptions::new().with_skip_auto_install(true))
        .await
        .unwrap();
    let installed = system
        .button_immediate_install(
            &init.orm.env(SUPERUSER_ID, Context::new()),
            &[ModuleName::new("account_reporting").unwrap()],
        )
        .await
        .unwrap();
    assert!(installed.post_commit.is_clean());
    let orm = installed.orm;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let other = other_company(&mut tx, company).await;
    let report = xml_id(
        &mut tx,
        "account_reporting",
        "report_posted_journal_activity",
    )
    .await;
    let menu = xml_id(&mut tx, "account_reporting", "menu_posted_journal_activity").await;
    assert!(menu > 0);
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    periods(&orm, other).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let system_group = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "journal.activity.admin", company, &[system_group]).await;
    let group = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader = user(&mut tx, "journal.activity.reader", company, &[group]).await;
    let outsider = user(&mut tx, "journal.activity.internal", company, &[]).await;
    let entry = posted_entry(&mut tx, &b, b.bank, b.income, None, "37.25", d(2024, 2, 1)).await;
    let entry_lines = entry.related(&mut tx, "line_ids").await.unwrap().ids();
    posted_entry(&mut tx, &b, b.bank, b.income, None, "72.10", d(2024, 1, 31)).await;
    let journal_id = entry.related(&mut tx, "journal_id").await.unwrap().ids()[0];
    let reverse = posted_entry(&mut tx, &b, b.income, b.bank, None, "11.05", d(2024, 3, 6)).await;
    let mut period_items = entry_lines.clone();
    period_items.extend(reverse.related(&mut tx, "line_ids").await.unwrap().ids());
    let _draft = balanced_entry(&mut tx, &b, journal_id, b.expense, "81").await;
    let alternate = tx
        .env()
        .records("account.journal.group")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("name", text("Management ledger"))],
        )
        .await
        .unwrap()
        .ids()[0];
    let other_journal = journal(&mut tx, company, "ALT", "general", None).await;
    tx.env()
        .records("account.journal")
        .unwrap()
        .browse([other_journal])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("journal_group_id", Cell::Id(alternate)),
        )
        .await
        .unwrap();
    balanced_entry(&mut tx, &b, other_journal, b.bank, "52")
        .await
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap();
    let b2 = books(&mut tx, other).await;
    posted_entry(&mut tx, &b2, b2.bank, b2.income, None, "910", d(2024, 2, 1)).await;
    tx.install_audit_schema().await.unwrap();
    tx.commit().await.unwrap();

    let orm = policy_orm(&db, &orm).await;
    Fixture {
        _db: db,
        orm,
        company,
        other,
        journal_report: report,
        reader: UserId::new(reader).unwrap(),
        outsider: UserId::new(outsider).unwrap(),
        february_items: entry_lines,
        period_items,
        books: b,
        statutory_journal: journal_id,
        other_books: b2,
    }
}

async fn activity() {
    let f = fixture().await;
    let report = f.journal_report;
    let company = f.company;
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let options = json!({"report":report,"companies":[company],"date":{"preset":"custom","from":"2024-02-01","to":"2024-02-29"}});
    let shown = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    assert_eq!(shown["lines"].as_array().unwrap().len(), 1);
    let root = &shown["lines"][0];
    assert_eq!(numbers(root), [num("2"), num("37.25"), num("37.25")]);
    let journals = runtime::report_expand(&env, &mut tx, &options, root["id"].as_str().unwrap(), 0)
        .await
        .unwrap();
    assert_eq!(journals["lines"].as_array().unwrap().len(), 1);
    let journal = &journals["lines"][0];
    assert_eq!(numbers(journal), numbers(root));
    let accounts =
        runtime::report_expand(&env, &mut tx, &options, journal["id"].as_str().unwrap(), 0)
            .await
            .unwrap();
    assert_eq!(accounts["lines"].as_array().unwrap().len(), 2);
    let debit = accounts["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| numbers(row)[1] > Decimal::ZERO)
        .unwrap();
    assert_eq!(numbers(debit), [num("1"), num("37.25"), num("0")]);
    let action = runtime::report_drill(
        &env,
        &mut tx,
        &options,
        journal["id"].as_str().unwrap(),
        &json!({"cell":0}),
    )
    .await
    .unwrap();
    let records = env
        .records("account.move.line")
        .unwrap()
        .search(
            &mut tx,
            &erp_sdk::Domain::from_json(&action["domain"]).unwrap(),
        )
        .await
        .unwrap();
    let mut actual = records.ids();
    actual.sort_unstable();
    let mut expected = f.february_items;
    expected.sort_unstable();
    assert_eq!(actual, expected);
    drop(tx);

    let mut tx = f.orm.begin(f.outsider).await.unwrap();
    let error = runtime::report_lines(&tx.env(), &mut tx, &options)
        .await
        .unwrap_err();
    assert_eq!(
        error.kind(),
        erp_sdk::ErrorKind::Access,
        "{}",
        error.message()
    );
}

#[test]
fn account_movements_reconcile_signed_period_amounts_to_visible_items() {
    big_stack(movements);
}

async fn movements() {
    let f = fixture().await;
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let report = xml_id(&mut tx, "account_reporting", "report_account_movements").await;
    let menu = xml_id(&mut tx, "account_reporting", "menu_account_movements").await;
    assert!(menu > 0);
    let env = tx.env();
    let options = json!({"report":report, "companies":[f.company], "date":{"preset":"custom","from":"2024-02-01","to":"2024-03-31"}});
    let shown = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    let root = &shown["lines"][0];
    assert_eq!(numbers(root), [num("48.30"), num("48.30"), num("0")]);
    let accounts = runtime::report_expand(&env, &mut tx, &options, root["id"].as_str().unwrap(), 0)
        .await
        .unwrap();
    let accounts = accounts["lines"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    let mut totals: Vec<Vec<Decimal>> = accounts.iter().map(numbers).collect();
    totals.sort();
    assert_eq!(
        totals,
        [
            vec![num("11.05"), num("37.25"), num("-26.20")],
            vec![num("37.25"), num("11.05"), num("26.20")],
        ]
    );
    let mut drilled_ids = Vec::new();
    for account in accounts {
        let entries =
            runtime::report_expand(&env, &mut tx, &options, account["id"].as_str().unwrap(), 0)
                .await
                .unwrap();
        let entries = entries["lines"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let mut sum = [Decimal::ZERO; 3];
        for entry in entries {
            let action = runtime::report_drill(
                &env,
                &mut tx,
                &options,
                entry["id"].as_str().unwrap(),
                &json!({"cell":2}),
            )
            .await
            .unwrap();
            let items = env
                .records("account.move.line")
                .unwrap()
                .search(
                    &mut tx,
                    &erp_sdk::Domain::from_json(&action["domain"]).unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(items.len(), 1);
            drilled_ids.extend(items.ids());
            let mut booked = Vec::new();
            for field in ["debit", "credit", "balance"] {
                booked.push(decimal(&items, &mut tx, field).await);
            }
            assert_eq!(numbers(entry), booked);
            for (total, value) in sum.iter_mut().zip(booked) {
                *total += value;
            }
        }
        assert_eq!(numbers(account), sum);
    }
    drilled_ids.sort_unstable();
    let mut admitted = f.period_items;
    admitted.sort_unstable();
    assert_eq!(drilled_ids, admitted);
    let mut denied_company = options.clone();
    denied_company["companies"] = json!([f.other]);
    let error = runtime::report_lines(&env, &mut tx, &denied_company)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        format!(
            "options.companies: company {} is not one of the companies you may use.",
            f.other
        )
    );
    drop(tx);
    let mut tx = f.orm.begin(f.outsider).await.unwrap();
    let error = runtime::report_lines(&tx.env(), &mut tx, &options)
        .await
        .unwrap_err();
    assert_source_denial(&mut tx, &error).await;
}

#[test]
fn profit_loss_shows_period_earnings_and_excludes_opening_equity() {
    big_stack(profit_loss);
}

async fn profit_loss() {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let system = system(&db, None);
    let init = system
        .initialize(InitOptions::new().with_skip_auto_install(true))
        .await
        .unwrap();
    let installed = system
        .button_immediate_install(
            &init.orm.env(SUPERUSER_ID, Context::new()),
            &[ModuleName::new("account_reporting").unwrap()],
        )
        .await
        .unwrap();
    let orm = &installed.orm;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let equity = account(&mut tx, b.company, "310015", "equity").await;
    posted_entry(&mut tx, &b, b.bank, equity, None, "1000", d(2024, 1, 10)).await;
    posted_entry(&mut tx, &b, b.bank, b.income, None, "128", d(2024, 2, 1)).await;
    posted_entry(&mut tx, &b, b.income, b.bank, None, "8", d(2024, 2, 2)).await;
    posted_entry(&mut tx, &b, b.expense, b.bank, None, "40", d(2024, 2, 10)).await;
    let report = xml_id(&mut tx, "account_reporting", "report_profit_loss").await;
    let result_line = xml_id(&mut tx, "account_reporting", "profit_loss_result").await;
    let admins = xml_id(&mut tx, "base", "group_system").await;
    let admin = user(&mut tx, "profit.loss.admin", company, &[admins]).await;
    let group = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader = user(&mut tx, "profit.loss.reader", company, &[group]).await;
    tx.install_audit_schema().await.unwrap();
    tx.commit().await.unwrap();
    let orm = policy_orm(&db, orm).await;
    let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
    let env = tx.env();
    let shown = runtime::report_lines(
        &env,
        &mut tx,
        &json!({
            "report": report, "companies": [b.company],
            "date": {"preset":"custom", "from":"2024-02-01", "to":"2024-02-29"}
        }),
    )
    .await
    .unwrap();
    let lines = shown["lines"].as_array().unwrap();
    let line = |label: &str| {
        lines
            .iter()
            .find(|line| line["name"] == label)
            .unwrap_or_else(|| panic!("no line {label}"))
    };
    // February only: the January opening equity (1000 on an equity account) is no part of it.
    for (label, expected, drillable) in [
        ("Income", "120", false),
        ("Gross Profit", "120", false),
        ("Operating Income", "120", false),
        ("Expenses", "40", false),
        ("Operating Expenses", "40", true),
        ("Net Profit", "80", false),
    ] {
        assert_eq!(numbers(line(label)), [num(expected)], "{label}");
        assert_eq!(line(label)["cells"][0]["drillable"], drillable, "{label}");
    }
    assert_eq!(
        numbers(line("Net Profit"))[0],
        numbers(line("Income"))[0] - numbers(line("Expenses"))[0]
    );
    let expense = line("Operating Expenses");
    let action = runtime::report_drill(
        &env,
        &mut tx,
        &shown["options"],
        expense["id"].as_str().unwrap(),
        &json!({"cell": 0}),
    )
    .await
    .unwrap();
    assert_eq!(action["res_model"], "account.move.line");
    let items = env
        .records("account.move.line")
        .unwrap()
        .search(&mut tx, &Domain::from_json(&action["domain"]).unwrap())
        .await
        .unwrap();
    let booked: Decimal = items
        .read_with(&mut tx, &["balance"], erp_sdk::Load::Raw)
        .await
        .unwrap()
        .iter()
        .map(|row| booked_amount(&row["balance"]))
        .sum();
    assert_eq!(booked, numbers(expense)[0]);
    let income = line("Operating Income");
    let accounts = runtime::report_expand(
        &env,
        &mut tx,
        &shown["options"],
        income["id"].as_str().unwrap(),
        0,
    )
    .await
    .unwrap();
    let account = &accounts["lines"][0];
    assert_eq!(numbers(account), [num("120")]);
    assert_eq!(account["cells"][0]["drillable"], false);
    assert_eq!(account["caret"], json!(["open"]));
    let error = runtime::report_drill(
        &env,
        &mut tx,
        &shown["options"],
        account["id"].as_str().unwrap(),
        &json!({"cell": 0}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "The amount cell of line OP_INCOME is a formula; no records are behind it."
    );
    tx.rollback().await.unwrap();
    let mut tx = orm.begin(UserId::new(admin).unwrap()).await.unwrap();
    tx.env()
        .records("reporting.report.line")
        .unwrap()
        .browse([result_line])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("name", text("Operating result")),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let upgraded = system
        .button_immediate_upgrade(
            &installed.orm.env(SUPERUSER_ID, Context::new()),
            &[ModuleName::new("account_reporting").unwrap()],
        )
        .await
        .unwrap();
    assert!(upgraded.post_commit.is_clean());
    let orm = policy_orm(&db, &upgraded.orm).await;
    let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
    let updated = runtime::report_lines(&tx.env(), &mut tx, &json!({
        "report": report, "companies":[company], "date":{"preset":"custom","from":"2024-02-01","to":"2024-02-29"}
    })).await.unwrap();
    let retained = updated["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "Operating result")
        .expect("administrator label survived module upgrade");
    assert_eq!(numbers(retained), [num("80")]);
}

// The base fixture also installs ALT; relying on the journal default would post to it.
async fn trial_entry(
    tx: &mut Transaction,
    f: &Fixture,
    account: i64,
    against: i64,
    amount: &str,
    date: Date,
) {
    partner_entry(tx, f, (account, against), None, amount, date).await;
}

async fn partner_entry(
    tx: &mut Transaction,
    f: &Fixture,
    accounts: (i64, i64),
    partner: Option<i64>,
    amount: &str,
    date: Date,
) -> Records {
    let (account, against) = accounts;
    let amount = num(amount);
    let mut first = RawVals::new()
        .set_cell("account_id", Cell::Id(account))
        .set_cell("balance", Cell::Decimal(amount));
    if let Some(partner) = partner {
        first.insert("partner_id".into(), Cell::Id(partner));
    }
    let created = moves(tx, f.company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(date))
                    .set_cell("journal_id", Cell::Id(f.statutory_journal))
                    .set_commands(
                        "line_ids",
                        vec![
                            Command::create(first),
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(against))
                                    .set_cell("balance", Cell::Decimal(-amount)),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap();
    created.call::<(), ()>(tx, "action_post", ()).await.unwrap();
    assert_eq!(
        created.related(tx, "journal_id").await.unwrap().ids(),
        [f.statutory_journal]
    );
    created
}

#[test]
fn ledger_trial_balance_reconciles_opening_activity_and_comparisons() {
    big_stack(trial_balance);
}

async fn trial_balance() {
    let f = fixture().await;
    erp_addon_account::fiscal::ensure_periods_with(
        &f.orm,
        erp_sdk::BeginOptions::new(SUPERUSER_ID),
        &erp_addon_account::fiscal::settings::FiscalCalendar::new(
            erp_addon_account::fiscal::calendar::FiscalYearEnd::new(31, 12).unwrap(),
            12,
        )
        .unwrap(),
        f.company,
        d(2023, 1, 1),
        d(2023, 12, 31),
    )
    .await
    .unwrap();
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let b = &f.books;
    let (bank, income, expense, payable) = (b.bank, b.income, b.expense, b.payable);
    let receivable = b.receivable;
    // Prior-year profit remains on income until an actual closing journal entry exists.
    trial_entry(&mut tx, &f, bank, income, "8.90", d(2023, 12, 31)).await;
    // Beyond binary64's exact cent range: the remaining cent must survive aggregation.
    trial_entry(
        &mut tx,
        &f,
        bank,
        income,
        "90071992547409.91",
        d(2023, 12, 31),
    )
    .await;
    trial_entry(
        &mut tx,
        &f,
        income,
        bank,
        "90071992547409.90",
        d(2023, 12, 31),
    )
    .await;
    // These accounts have an opening balance but no in-period items.
    trial_entry(&mut tx, &f, expense, payable, "5.50", d(2024, 1, 15)).await;
    trial_entry(&mut tx, &f, receivable, income, "10.01", d(2024, 2, 29)).await;
    let report = xml_id(&mut tx, "account_reporting", "report_ledger_trial_balance").await;
    assert!(xml_id(&mut tx, "account_reporting", "menu_ledger_trial_balance").await > 0);
    tx.commit().await.unwrap();

    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let options = json!({"report": report, "companies": [f.company],
        "date": {"preset":"custom", "from":"2024-02-01", "to":"2024-03-31"},
        "comparison": {"mode":"custom", "from":"2024-01-01", "to":"2024-01-31"}});
    let shown = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    let resolved = &shown["options"];
    // Current period first, then the comparison: opening, debit, credit, closing in each group.
    let mut cells = Vec::new();
    for group in [0, 1] {
        for label in ["opening", "debit", "credit", "closing"] {
            cells.push(column(resolved, group, label));
        }
    }
    assert_eq!(cells, (0..8).collect::<Vec<_>>());
    let (rows, total) = split_total(shown["lines"].as_array().unwrap(), report);
    assert_eq!(
        cell_values(total),
        [
            num("0"),
            num("58.31"),
            num("58.31"),
            num("0"),
            num("0"),
            num("77.60"),
            num("77.60"),
            num("0")
        ]
    );
    assert_eq!(
        rows.len(),
        5,
        "opening-only and current-only accounts must remain visible"
    );
    // One flat line per account, ordered by account code, named code + name; none unfolds.
    let mut codes = Vec::new();
    for row in rows {
        assert_eq!(row["level"], 0);
        assert_eq!(row["unfoldable"], false);
        let id = LineId::parse(line_id(row)).unwrap();
        let account = id.segments()[1].value().parse::<i64>().unwrap();
        assert_eq!(
            line_id(row),
            group_line(report, "account_id", "account.account", account).to_string()
        );
        let record = env
            .records("account.account")
            .unwrap()
            .browse([account])
            .unwrap();
        let code = string(&record, &mut tx, "code").await;
        let name = string(&record, &mut tx, "name").await;
        assert_eq!(row["name"], format!("{code} {name}"));
        codes.push(code);
    }
    let mut sorted = codes.clone();
    sorted.sort();
    assert_eq!(codes, sorted);
    let expected = [
        (
            bank,
            [
                "81.01", "37.25", "11.05", "107.21", "8.91", "72.10", "0", "81.01",
            ],
        ),
        (
            income,
            [
                "-81.01", "11.05", "47.26", "-117.22", "-8.91", "0", "72.10", "-81.01",
            ],
        ),
        (receivable, ["0", "10.01", "0", "10.01", "0", "0", "0", "0"]),
        (
            expense,
            ["5.50", "0", "0", "5.50", "0", "5.50", "0", "5.50"],
        ),
        (
            payable,
            ["-5.50", "0", "0", "-5.50", "0", "0", "5.50", "-5.50"],
        ),
    ];
    let mut totals = [Decimal::ZERO; 8];
    for (account, expected) in expected {
        let id = group_line(report, "account_id", "account.account", account).to_string();
        let row = rows.iter().find(|r| line_id(r) == id).unwrap();
        let actual = cell_values(row);
        assert_eq!(actual, expected.map(num));
        assert_eq!(actual[0] + actual[1] - actual[2], actual[3]);
        assert_eq!(actual[4] + actual[5] - actual[6], actual[7]);
        for (total, value) in totals.iter_mut().zip(&actual) {
            *total += value;
        }
        // Each aggregate cell drills through its own date scope, including opening-only rows.
        for (cell, field) in [
            "balance", "debit", "credit", "balance", "balance", "debit", "credit", "balance",
        ]
        .iter()
        .enumerate()
        {
            let action = runtime::report_drill(
                &env,
                &mut tx,
                &options,
                row["id"].as_str().unwrap(),
                &json!({"cell":cell}),
            )
            .await
            .unwrap();
            let items = env
                .records("account.move.line")
                .unwrap()
                .search(&mut tx, &Domain::from_json(&action["domain"]).unwrap())
                .await
                .unwrap();
            let values = items
                .read_with(&mut tx, &[*field], erp_sdk::Load::Raw)
                .await
                .unwrap();
            let sum: Decimal = values.iter().map(|r| booked_amount(&r[*field])).sum();
            assert_eq!(sum, actual[cell]);
        }
    }
    assert_eq!(totals.to_vec(), cell_values(total));
    let error = runtime::report_expand(&env, &mut tx, &options, line_id(&rows[0]), 0)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(error.message(), "Trial Balance lines do not unfold.");
    let mut denied = options.clone();
    denied["companies"] = json!([f.other]);
    let error = runtime::report_lines(&env, &mut tx, &denied)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        format!(
            "options.companies: company {} is not one of the companies you may use.",
            f.other
        )
    );
    drop(tx);
    let mut tx = f.orm.begin(f.outsider).await.unwrap();
    let error = runtime::report_lines(&tx.env(), &mut tx, &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Access);
    let source_error = tx
        .env()
        .records("account.move.line")
        .unwrap()
        ._read_group(
            &mut tx,
            ReadGroup::new(Domain::constant(true)).aggregates(["balance:sum"]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.message(), source_error.message());
    drop(tx);

    // A source-row restriction can make visible books unbalanced; never bypass it to force totals.
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let model = xml_id(&mut tx, "account", "model_account_move_line").await;
    tx.env()
        .records("ir.access")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Trial balance restricted account"))
                    .set_cell("model_id", Cell::Id(model))
                    .set_cell("operation", text("r"))
                    .set_cell("domain", text(&format!("[('account_id', '=', {bank})]"))),
            ],
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let restricted = policy_orm(&f._db, &f.orm).await;
    let mut tx = restricted.begin(f.reader).await.unwrap();
    let env = tx.env();
    let shown = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    let (rows, total) = split_total(shown["lines"].as_array().unwrap(), report);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        line_id(row),
        group_line(report, "account_id", "account.account", bank).to_string()
    );
    let bank_row = [
        num("81.01"),
        num("37.25"),
        num("11.05"),
        num("107.21"),
        num("8.91"),
        num("72.10"),
        num("0"),
        num("81.01"),
    ];
    assert_eq!(cell_values(row), bank_row);
    assert_eq!(cell_values(total), bank_row);
    let drill = runtime::report_drill(
        &env,
        &mut tx,
        &options,
        row["id"].as_str().unwrap(),
        &json!({"cell":3}),
    )
    .await
    .unwrap();
    let admitted = env
        .records("account.move.line")
        .unwrap()
        .search(&mut tx, &Domain::from_json(&drill["domain"]).unwrap())
        .await
        .unwrap();
    let rows = admitted
        .read_with(&mut tx, &["account_id", "balance"], erp_sdk::Load::Raw)
        .await
        .unwrap();
    let mut sum = Decimal::ZERO;
    for item in rows {
        assert_eq!(item["account_id"], Value::Int(bank));
        sum += booked_amount(&item["balance"]);
    }
    assert_eq!(sum, num("107.21"));
}

#[test]
fn trial_balance_lists_every_account_in_one_answer_and_refuses_ambiguous_dates() {
    big_stack(trial_balance_pages);
}

async fn trial_balance_pages() {
    let f = fixture().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let mut created = Vec::new();
    for i in 0..12 {
        let account = account(&mut tx, f.company, &format!("6200{i:02}"), "expense").await;
        trial_entry(&mut tx, &f, account, f.books.bank, "1.01", d(2024, 1, 10)).await;
        created.push(account);
    }
    let report = xml_id(&mut tx, "account_reporting", "report_ledger_trial_balance").await;
    let definition = tx
        .env()
        .records("reporting.report")
        .unwrap()
        .browse([report])
        .unwrap();
    // A load-more page size smaller than the account count does not page or cut the account list.
    definition
        .write(
            &mut tx,
            RawVals::new().set_cell("load_more_limit", Cell::Int(10)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let options = json!({"report": report, "companies":[f.company],
        "date":{"preset":"custom", "from":"2024-02-01", "to":"2024-03-31"}});
    let top = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    let (rows, total) = split_total(top["lines"].as_array().unwrap(), report);
    // bank and income of the fixture plus the twelve opening-only expense accounts
    assert_eq!(rows.len(), 14);
    let mut sum = [Decimal::ZERO; 4];
    let mut ids = std::collections::BTreeSet::new();
    for row in rows {
        assert_ne!(row["kind"], "more");
        assert!(ids.insert(line_id(row).to_owned()));
        for (sum, value) in sum.iter_mut().zip(cell_values(row)) {
            *sum += value;
        }
    }
    for account in [f.books.bank, f.books.income].into_iter().chain(created) {
        assert!(
            ids.contains(&group_line(report, "account_id", "account.account", account).to_string())
        );
    }
    assert_eq!(sum.to_vec(), cell_values(total));
    let mut empty = options.clone();
    empty["date"] = json!({"preset":"custom", "from":"2020-01-01", "to":"2020-01-31"});
    let zero = runtime::report_lines(&env, &mut tx, &empty).await.unwrap();
    assert_eq!(zero["lines"], json!([]));
    // Provider options are refused by name: this report has none.
    let mut forged = options.clone();
    forged["provider"] = json!({"fiscal_opening": true});
    let error = runtime::report_lines(&env, &mut tx, &forged)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid Trial Balance option fiscal_opening. This report has no provider options."
    );
    drop(tx);
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let column_id = xml_id(&mut tx, "account_reporting", "trial_balance_column_debit").await;
    let debit = tx
        .env()
        .records("reporting.report.column")
        .unwrap()
        .browse([column_id])
        .unwrap();
    debit
        .write(
            &mut tx,
            RawVals::new().set_cell("sortable", Cell::Bool(true)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let mut sorted = options.clone();
    sorted["order"] = json!({"column":"debit", "direction":"desc"});
    let error = runtime::report_lines(&tx.env(), &mut tx, &sorted)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Trial Balance rows are ordered by account code."
    );
    drop(tx);
    // A column the provider cannot fill is refused by name, never shown blank. The definition is
    // changed in an administrator transaction that is rolled back; the reader asks in it.
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let env = tx.env().with_user(f.reader);
    debit
        .write(&mut tx, RawVals::new().set_cell("kind", text("decimal")))
        .await
        .unwrap();
    let error = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Trial Balance column debit is not supported; its columns are monetary opening, debit, credit, closing and balance."
    );
    tx.rollback().await.unwrap();
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    tx.env()
        .records("reporting.report.column")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("report_id", Cell::Id(report))
                    .set_cell("sequence", Cell::Int(50))
                    .set_cell("name", text("Share"))
                    .set_cell("label", text("share"))
                    .set_cell("kind", text("monetary")),
            ],
        )
        .await
        .unwrap();
    let env = tx.env().with_user(f.reader);
    let error = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Trial Balance column share is not supported; its columns are monetary opening, debit, credit, closing and balance."
    );
    tx.rollback().await.unwrap();
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    definition
        .write(
            &mut tx,
            RawVals::new().set_cell("date_mode", text("single")),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let error = runtime::report_lines(
        &tx.env(),
        &mut tx,
        &json!({"report":report,"companies":[f.company],
        "date":{"preset":"custom","to":"2024-03-31"}}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Trial Balance requires a date range with a start date."
    );
}

#[test]
fn partner_ledger_partitions_accounts_partners_and_dates() {
    big_stack(partner_ledger);
}

async fn partner_ledger() {
    let f = fixture().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let p = partner(&mut tx, "Customer P").await;
    let q = partner(&mut tx, "Supplier Q").await;
    let ar = (f.books.receivable, f.books.bank);
    let ap = (f.books.payable, f.books.bank);
    let prior = partner_entry(&mut tx, &f, ar, Some(p), "120", d(2024, 1, 31)).await;
    let current = partner_entry(&mut tx, &f, ar, Some(p), "-50", d(2024, 2, 15)).await;
    let supplier = partner_entry(&mut tx, &f, ap, Some(q), "-90", d(2024, 2, 15)).await;
    let unknown = partner_entry(&mut tx, &f, ar, None, "3.01", d(2024, 2, 29)).await;
    partner_entry(&mut tx, &f, ar, Some(p), "99", d(2024, 3, 1)).await;
    let expected = items_on(&mut tx, &[&prior, &current], ar.0).await.ids();
    let current_item = items_on(&mut tx, &[&current], ar.0).await.ids()[0];
    let supplier_item = items_on(&mut tx, &[&supplier], ap.0).await.ids()[0];
    let unknown_item = items_on(&mut tx, &[&unknown], ar.0).await.ids()[0];
    let accounts = env_accounts(&mut tx, f.company, &[ar.0, ap.0]).await;
    let report = xml_id(&mut tx, "account_reporting", "report_partner_ledger").await;
    assert!(xml_id(&mut tx, "account_reporting", "menu_partner_ledger").await > 0);
    tx.commit().await.unwrap();

    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let options = json!({"report":report, "companies":[f.company],
        "date":{"preset":"custom","from":"2024-02-01","to":"2024-02-29"}});
    let shown = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    let resolved = &shown["options"];
    let [debit, credit, balance, account, date] =
        ["debit", "credit", "balance", "account", "date"].map(|label| column(resolved, 0, label));
    let (partners, total) = split_total(shown["lines"].as_array().unwrap(), report);
    // Partners at the top level (no receivable/payable sections); the items without a partner last.
    let partner_line = |id: i64| group_line(report, "partner_id", "res.partner", id).to_string();
    assert_eq!(
        partners.iter().map(line_id).collect::<Vec<_>>(),
        [partner_line(p), partner_line(q), partner_line(0)]
    );
    // Feb only for debit and credit; the balance runs from the beginning to 29 Feb (March is out).
    for (line, expected) in
        partners
            .iter()
            .zip([["0", "50", "70"], ["0", "90", "-90"], ["3.01", "0", "3.01"]])
    {
        let values = cell_values(line);
        assert_eq!(
            [values[debit], values[credit], values[balance]],
            expected.map(num),
            "{}",
            line["name"]
        );
    }
    let mut sums = [Decimal::ZERO; 3];
    for line in partners {
        let values = cell_values(line);
        sums[0] += values[debit];
        sums[1] += values[credit];
        sums[2] += values[balance];
    }
    let totals = cell_values(total);
    assert_eq!(sums, [totals[debit], totals[credit], totals[balance]]);
    // The customer: the January item as initial balance, then the February item on the
    // receivable account with its running balance.
    let customer = &partners[0];
    let rows = runtime::report_expand(&env, &mut tx, &options, line_id(customer), 0)
        .await
        .unwrap();
    let rows = rows["lines"].as_array().unwrap();
    let customer_id = LineId::parse(line_id(customer)).unwrap();
    assert_eq!(
        rows.iter().map(line_id).collect::<Vec<_>>(),
        [
            customer_id.child(Segment::line("INITIAL")).to_string(),
            customer_id
                .child(Segment::group(
                    "id",
                    "account.move.line",
                    &current_item.to_string()
                ))
                .to_string(),
        ]
    );
    assert_eq!(cell_values(&rows[0])[balance], num("120"));
    assert_eq!(rows[1]["cells"][date]["value"], "2024-02-15");
    assert_eq!(rows[1]["cells"][account]["value"], accounts[0].as_str());
    assert_eq!(cell_values(&rows[1])[credit], num("50"));
    assert_eq!(
        cell_values(&rows[1])[balance],
        cell_values(customer)[balance]
    );
    // The supplier's and the unknown partner's items sit on their own accounts.
    for (line, item, code) in [
        (&partners[1], supplier_item, &accounts[1]),
        (&partners[2], unknown_item, &accounts[0]),
    ] {
        let rows = runtime::report_expand(&env, &mut tx, &options, line_id(line), 0)
            .await
            .unwrap();
        let rows = rows["lines"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(line_id(&rows[0]).ends_with(&format!("|group:id~account.move.line~{item}")));
        assert_eq!(rows[0]["cells"][account]["value"], code.as_str());
        assert_eq!(cell_values(&rows[0])[balance], cell_values(line)[balance]);
    }
    let mut expected = expected;
    expected.sort_unstable();
    assert_eq!(
        drilled_ids(&env, &mut tx, &options, line_id(customer), balance).await,
        expected
    );
    assert_eq!(
        drilled_ids(&env, &mut tx, &options, line_id(customer), credit).await,
        [current_item]
    );
}

/// The codes of `accounts`, as the account records hold them.
async fn env_accounts(tx: &mut Transaction, company: i64, accounts: &[i64]) -> Vec<String> {
    let mut codes = Vec::new();
    for account in accounts {
        let record = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("account.account")
            .unwrap()
            .browse([*account])
            .unwrap();
        codes.push(string(&record, tx, "code").await);
    }
    codes
}

/// A posted receivable item of `amount` for `partner`, with `due` as its recorded due date.
async fn due_entry(
    tx: &mut Transaction,
    f: &Fixture,
    partner: i64,
    amount: &str,
    date: Date,
    due: Option<Date>,
) -> Records {
    let amount = num(amount);
    let mut first = RawVals::new()
        .set_cell("account_id", Cell::Id(f.books.receivable))
        .set_cell("partner_id", Cell::Id(partner))
        .set_cell("balance", Cell::Decimal(amount));
    if let Some(due) = due {
        first.insert("date_maturity".into(), Cell::Date(due));
    }
    let created = moves(tx, f.company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(date))
                    .set_cell("journal_id", Cell::Id(f.statutory_journal))
                    .set_commands(
                        "line_ids",
                        vec![
                            Command::create(first),
                            Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(f.books.bank))
                                    .set_cell("balance", Cell::Decimal(-amount)),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap();
    created.call::<(), ()>(tx, "action_post", ()).await.unwrap();
    let item = items_on(tx, &[&created], f.books.receivable).await;
    match (item.cell(tx, "date_maturity").await.unwrap(), due) {
        (Cell::Date(recorded), Some(due)) => assert_eq!(recorded, due),
        (Cell::Null | Cell::Bool(false), None) => {}
        (recorded, due) => panic!("fixture due date {recorded:?}, wanted {due:?}"),
    }
    item
}

#[test]
fn partner_ledger_due_dates_are_shown_as_recorded_and_overdue_only_while_open() {
    big_stack(partner_ledger_due_dates);
}

async fn partner_ledger_due_dates() {
    let f = fixture().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let p = partner(&mut tx, "Due date customer").await;
    // All in January; the report covers January only, so "overdue" must follow the caller's today,
    // not the end of the reported period (each due date below is after 31 January).
    let overdue = due_entry(&mut tx, &f, p, "100", d(2024, 1, 10), Some(d(2024, 2, 15))).await;
    let undated = due_entry(&mut tx, &f, p, "30", d(2024, 1, 11), None).await;
    let future = due_entry(&mut tx, &f, p, "40", d(2024, 1, 12), Some(d(2099, 12, 31))).await;
    let settled = due_entry(&mut tx, &f, p, "25", d(2024, 1, 13), Some(d(2024, 3, 1))).await;
    let payment = due_entry(&mut tx, &f, p, "-25", d(2024, 1, 14), Some(d(2024, 3, 1))).await;
    let company = tx.env().with_company(f.company).unwrap();
    try_call(
        &settled.union(&payment).unwrap().with_env(company),
        &mut tx,
        "reconcile",
    )
    .await
    .unwrap();
    assert_ne!(
        settled.cell(&mut tx, "full_reconcile_id").await.unwrap(),
        Cell::Null
    );
    let report = xml_id(&mut tx, "account_reporting", "report_partner_ledger").await;
    tx.commit().await.unwrap();

    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let today = env.context_today().unwrap();
    assert!(d(2024, 2, 15) < today && today < d(2099, 12, 31));
    let options = json!({"report":report, "companies":[f.company],
        "date":{"preset":"custom","from":"2024-01-01","to":"2024-01-31"}});
    let resolved = runtime::report_options(&env, &mut tx, report, &options)
        .await
        .unwrap()["options"]
        .clone();
    let due = column(&resolved, 0, "due_date");
    let customer = group_line(report, "partner_id", "res.partner", p);
    let rows = runtime::report_expand(&env, &mut tx, &resolved, &customer.to_string(), 0)
        .await
        .unwrap();
    let rows = rows["lines"].as_array().unwrap();
    let row = |item: &Records| {
        let id = customer
            .child(Segment::group(
                "id",
                "account.move.line",
                &item.ids()[0].to_string(),
            ))
            .to_string();
        rows.iter()
            .find(|row| line_id(row) == id)
            .unwrap_or_else(|| panic!("no row {id}"))
    };
    for (item, shown, danger) in [
        (&overdue, Some("2024-02-15"), true),
        (&undated, None, false),
        (&future, Some("2099-12-31"), false),
        (&settled, Some("2024-03-01"), false),
        (&payment, Some("2024-03-01"), false),
    ] {
        let line = row(item);
        let cell = &line["cells"][due];
        assert_eq!(cell["value"].as_str(), shown, "{line}");
        assert_eq!(
            cell.get("tone").and_then(Json::as_str),
            danger.then_some("danger"),
            "{line}"
        );
        for (index, other) in line["cells"].as_array().unwrap().iter().enumerate() {
            if index != due {
                assert!(other.get("tone").is_none(), "{line}");
            }
        }
    }
    // A page past what one screen may hold is refused before any query.
    let bounds = erp_addon_reporting::settings::read(&env, &mut tx)
        .await
        .unwrap();
    let offset = i64::try_from(bounds.max_screen_lines).unwrap();
    let error = runtime::report_expand(&env, &mut tx, &resolved, &customer.to_string(), offset)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Partner Ledger paging exceeds reporting.max_screen_lines; narrow the scope."
    );
}

#[test]
fn partner_ledger_unreconciled_uses_the_requested_cutoff() {
    big_stack(partner_ledger_unreconciled);
}

async fn partner_ledger_unreconciled() {
    let f = fixture().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let p = partner(&mut tx, "Historical customer").await;
    let ar = (f.books.receivable, f.books.bank);
    let invoice = partner_entry(&mut tx, &f, ar, Some(p), "120", d(2024, 1, 31)).await;
    let first = partner_entry(&mut tx, &f, ar, Some(p), "-50", d(2024, 2, 2)).await;
    let second = partner_entry(&mut tx, &f, ar, None, "-70", d(2024, 2, 10)).await;
    let invoice = items_on(&mut tx, &[&invoice], ar.0).await;
    let first = items_on(&mut tx, &[&first], ar.0).await;
    let second = items_on(&mut tx, &[&second], ar.0).await;
    // Matching is done in the company, as a user of that company does it.
    let company = tx.env().with_company(f.company).unwrap();
    for payment in [&first, &second] {
        try_call(
            &invoice.union(payment).unwrap().with_env(company.clone()),
            &mut tx,
            "reconcile",
        )
        .await
        .unwrap();
    }
    try_call(
        &second.with_env(company.clone()),
        &mut tx,
        "remove_move_reconcile",
    )
    .await
    .unwrap();
    let removed = erp_addon_base::company_calendar::business_date(&mut tx, f.company)
        .await
        .unwrap();
    let (year, month, day) = removed.ymd();
    let removed = format!("{year:04}-{month:02}-{day:02}");
    let report = xml_id(&mut tx, "account_reporting", "report_partner_ledger").await;
    tx.commit().await.unwrap();

    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let initial = runtime::report_options(&env, &mut tx, report, &json!({}))
        .await
        .unwrap();
    assert_eq!(initial["options"]["provider"]["unreconciled"], false);
    let denied = json!({"report":report, "companies":[f.other], "provider":{"unreconciled":true}});
    let error = runtime::report_lines(&env, &mut tx, &denied)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert!(error.message().contains("not one of the companies"));
    for (cutoff, mut expected) in [
        ("2024-02-01", invoice.ids()),
        ("2024-02-02", invoice.ids()),
        ("2024-02-10", vec![]),
        (removed.as_str(), invoice.union(&second).unwrap().ids()),
    ] {
        let options = json!({"report":report, "companies":[f.company],
            "date":{"preset":"custom","from":"2024-01-01","to":cutoff},
            "provider":{"unreconciled":true}});
        let shown = runtime::report_lines(&env, &mut tx, &options)
            .await
            .unwrap();
        let balance = column(&shown["options"], 0, "balance");
        let lines = shown["lines"].as_array().unwrap();
        let expected_closing = if cutoff == "2024-02-10" {
            "0"
        } else if cutoff == removed {
            "50"
        } else {
            "120"
        };
        // The closing of the open items as of the cutoff, and the items behind it, partner by partner.
        let mut closing = Decimal::ZERO;
        let mut actual = Vec::new();
        if !lines.is_empty() {
            let (partners, total) = split_total(lines, report);
            for line in partners {
                closing += cell_values(line)[balance];
                actual.extend(drilled_ids(&env, &mut tx, &options, line_id(line), balance).await);
            }
            assert_eq!(cell_values(total)[balance], closing, "total at {cutoff}");
        }
        assert_eq!(
            closing,
            num(expected_closing),
            "filtered booked balance at {cutoff}"
        );
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected, "unreconciled closing drill at {cutoff}");
    }

    drop(tx);
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let env = tx.env();
    let items = env.records("account.move.line").unwrap();
    let replacement = items
        .browse(invoice.union(&second).unwrap().ids())
        .unwrap()
        .with_env(env.with_company(f.company).unwrap());
    try_call(&replacement, &mut tx, "reconcile").await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let historical = json!({"report":report, "companies":[f.company], "date":{"preset":"custom","from":"2024-01-01","to":"2024-02-10"}, "provider":{"unreconciled":true}});
    let error = runtime::report_lines(&env, &mut tx, &historical)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert!(
        error.message().contains("overlapping re-matches"),
        "{error}"
    );
}

#[test]
fn partner_ledger_refuses_dates_before_existing_ledger_capture() {
    big_stack(|| async {
        let f = fixture().await;
        let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
        let report = xml_id(&mut tx, "account_reporting", "report_partner_ledger").await;
        let env = tx.env().with_user(f.reader);
        // Transaction-local simulation of a predecessor without the coverage table. Real module
        // metadata and posted journal items remain in place. No prior binary is used.
        tx.execute("CREATE TEMP TABLE account_reconcile_coverage (LIKE public.account_reconcile_coverage INCLUDING ALL) ON COMMIT DROP", &[]).await.unwrap();
        let historical = json!({"report":report, "companies":[f.company], "date":{"preset":"custom","from":"2024-01-01","to":"2024-02-10"}, "provider":{"unreconciled":true}});
        let missing = runtime::report_lines(&env, &mut tx, &historical)
            .await
            .unwrap_err();
        assert!(
            missing
                .message()
                .contains("coverage has not been established"),
            "{missing}"
        );
        let establish = include_str!("../../../addons/account/src/reconcile/coverage.sql");
        tx.execute(establish, &[]).await.unwrap();
        tx.execute(establish, &[]).await.unwrap();
        let rows = tx.query("SELECT safe_from=((statement_timestamp() AT TIME ZONE 'UTC')::date+1) FROM account_reconcile_coverage", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].get::<_, bool>(0));
        let earlier = runtime::report_lines(&env, &mut tx, &historical)
            .await
            .unwrap_err();
        assert!(earlier.message().contains("history is available from"));
    });
}

// The report must propagate the complete source-model refusal, including permitted groups.
// Compare with an ordinary ORM read under the same principal, not a duplicated policy formatter.
async fn assert_source_denial(tx: &mut Transaction, error: &Error) {
    let direct = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .search(tx, &Domain::TRUE)
        .await
        .unwrap_err();
    assert_eq!(direct.kind(), erp_sdk::ErrorKind::Access);
    assert_eq!(error.kind(), direct.kind());
    assert_eq!(error.message(), direct.message());
}

async fn drill_values(env: &Env, tx: &mut Transaction, options: &Json, line: &Json) -> Vec<i64> {
    let mut ids = Vec::new();
    for (column, field) in ["debit", "credit", "balance"].iter().enumerate() {
        let action = runtime::report_drill(
            env,
            tx,
            options,
            line["id"].as_str().unwrap(),
            &json!({"cell":column}),
        )
        .await
        .unwrap();
        assert_eq!(action["res_model"], "account.move.line");
        let items = env
            .records("account.move.line")
            .unwrap()
            .search(tx, &Domain::from_json(&action["domain"]).unwrap())
            .await
            .unwrap();
        let mut current = items.ids();
        current.sort_unstable();
        if column == 0 {
            ids = current;
        } else {
            assert_eq!(current, ids);
        }
        let source = items
            .read_with(tx, &[*field], erp_sdk::Load::Raw)
            .await
            .unwrap();
        assert_eq!(source.len(), ids.len());
        let total: Decimal = source.iter().map(|row| booked_amount(&row[*field])).sum();
        assert_eq!(numbers(line)[column], total);
    }
    ids
}

// Explicit journal membership is part of this fixture: the default journal can be the
// alternate ledger after fixture() creates it (journals order by sequence, type and code).
async fn posted_items(
    tx: &mut Transaction,
    company: i64,
    journal: i64,
    date: Date,
    amounts: &[(i64, &str)],
) -> Records {
    let entry = moves(tx, company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(date))
                    .set_cell("journal_id", Cell::Id(journal))
                    .set_commands(
                        "line_ids",
                        amounts
                            .iter()
                            .map(|(account, amount)| {
                                Command::create(
                                    RawVals::new()
                                        .set_cell("account_id", Cell::Id(*account))
                                        .set_cell("balance", Cell::Decimal(num(amount))),
                                )
                            })
                            .collect::<Vec<_>>(),
                    ),
            ],
        )
        .await
        .unwrap();
    entry.call::<(), ()>(tx, "action_post", ()).await.unwrap();
    assert_eq!(entry.cell(tx, "date").await.unwrap(), Cell::Date(date));
    assert_eq!(
        entry.related(tx, "journal_id").await.unwrap().ids(),
        [journal]
    );
    entry
}

#[test]
fn account_movements_boundaries_and_all_cells_reconcile() {
    big_stack(boundaries);
}

async fn boundaries() {
    let f = fixture().await;
    let mut setup = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let b = &f.books;
    let j = journal(&mut setup, f.company, "BOUND", "general", None).await;
    let entry = posted_items(
        &mut setup,
        f.company,
        j,
        d(2024, 3, 31),
        &[(b.bank, "3"), (b.bank, "4.75"), (b.income, "-7.75")],
    )
    .await;
    let mut expected = entry.related(&mut setup, "line_ids").await.unwrap().ids();
    for (debit, credit) in [(b.expense, b.income), (b.income, b.expense)] {
        let e = posted_items(
            &mut setup,
            f.company,
            j,
            d(2024, 3, 31),
            &[(debit, "5"), (credit, "-5")],
        )
        .await;
        expected.extend(e.related(&mut setup, "line_ids").await.unwrap().ids());
    }
    for date in [d(2024, 3, 30), d(2024, 4, 1)] {
        posted_items(
            &mut setup,
            f.company,
            j,
            date,
            &[(b.bank, "900"), (b.income, "-900")],
        )
        .await;
    }
    setup.commit().await.unwrap();
    expected.sort_unstable();
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let report = xml_id(&mut tx, "account_reporting", "report_account_movements").await;
    let options = json!({"report":report,"companies":[f.company],
        "date":{"preset":"custom","from":"2024-03-31","to":"2024-03-31"}});
    let env = tx.env();
    let shown = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    assert_eq!(shown["lines"].as_array().unwrap().len(), 1);
    let root = &shown["lines"][0];
    assert_eq!(numbers(root), [num("17.75"), num("17.75"), num("0")]);
    assert_eq!(drill_values(&env, &mut tx, &options, root).await, expected);
    let accounts = runtime::report_expand(&env, &mut tx, &options, root["id"].as_str().unwrap(), 0)
        .await
        .unwrap();
    let accounts = accounts["lines"].as_array().unwrap();
    assert_eq!(accounts.len(), 3);
    assert!(
        accounts
            .iter()
            .any(|a| numbers(a) == [num("5"), num("5"), num("0")])
    );
    let mut all = Vec::new();
    let mut saw_multi_item_entry = false;
    for account in accounts {
        let account_ids = drill_values(&env, &mut tx, &options, account).await;
        let entries =
            runtime::report_expand(&env, &mut tx, &options, account["id"].as_str().unwrap(), 0)
                .await
                .unwrap();
        let mut entry_ids = Vec::new();
        for entry in entries["lines"].as_array().unwrap() {
            let ids = drill_values(&env, &mut tx, &options, entry).await;
            saw_multi_item_entry |= ids.len() == 2;
            entry_ids.extend(ids);
        }
        entry_ids.sort_unstable();
        assert_eq!(entry_ids, account_ids);
        all.extend(entry_ids);
    }
    all.sort_unstable();
    assert_eq!(all, expected);
    assert!(saw_multi_item_entry);
    let mut empty = options.clone();
    empty["date"] = json!({"preset":"custom","from":"2024-04-02","to":"2024-04-02"});
    let shown = runtime::report_lines(&env, &mut tx, &empty).await.unwrap();
    let root = &shown["lines"][0];
    assert_eq!(numbers(root), [Decimal::ZERO; 3]);
    assert!(drill_values(&env, &mut tx, &empty, root).await.is_empty());
    let children = runtime::report_expand(&env, &mut tx, &empty, root["id"].as_str().unwrap(), 0)
        .await
        .unwrap();
    assert!(children["lines"].as_array().unwrap().is_empty());
}

#[test]
fn account_movements_preserve_source_rules_and_company_selection() {
    big_stack(source_rules);
}

async fn source_rules() {
    let f = fixture().await;
    let mut setup = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let env = setup.env();
    env.records("res.users")
        .unwrap()
        .browse([f.reader.get()])
        .unwrap()
        .write(
            &mut setup,
            RawVals::new().set_commands(
                "company_ids",
                vec![Command::set(
                    [f.company, f.other].map(|id| RecId::from_real(id).unwrap()),
                )],
            ),
        )
        .await
        .unwrap();
    let peer_entry = posted_entry(
        &mut setup,
        &f.other_books,
        f.other_books.bank,
        f.other_books.income,
        None,
        "91",
        d(2024, 3, 6),
    )
    .await;
    let mut peer_ids = peer_entry
        .related(&mut setup, "line_ids")
        .await
        .unwrap()
        .ids();
    peer_ids.sort_unstable();
    // Hide the original February entry while leaving the March reversal visible.
    let model: i64 = setup
        .query(
            "SELECT id FROM ir_model WHERE model='account.move.line'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    env.records("ir.access")
        .unwrap()
        .create(
            &mut setup,
            vec![
                RawVals::new()
                    .set_cell("name", text("Only later report items"))
                    .set_cell("model_id", Cell::Id(model))
                    .set_cell("operation", text("r"))
                    .set_cell("domain", text("[('date','>=','2024-03-01')]")),
            ],
        )
        .await
        .unwrap();
    setup.commit().await.unwrap();
    let secured = policy_orm(&f._db, &f.orm).await;
    let mut tx = secured.begin(f.reader).await.unwrap();
    let env = tx.env().with_context([(
        "allowed_company_ids",
        Value::list([Value::Int(f.company), Value::Int(f.other)]),
    )]);
    let report = xml_id(&mut tx, "account_reporting", "report_account_movements").await;
    let options = json!({"report":report,"companies":[f.company],
        "date":{"preset":"custom","from":"2024-02-01","to":"2024-03-31"}});
    let shown = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    let root = &shown["lines"][0];
    assert_eq!(numbers(root), [num("11.05"), num("11.05"), num("0")]);
    let mut expected: Vec<i64> = f
        .period_items
        .iter()
        .copied()
        .filter(|id| !f.february_items.contains(id))
        .collect();
    expected.sort_unstable();
    assert_eq!(drill_values(&env, &mut tx, &options, root).await, expected);
    let accounts = runtime::report_expand(&env, &mut tx, &options, root["id"].as_str().unwrap(), 0)
        .await
        .unwrap();
    assert_eq!(accounts["lines"].as_array().unwrap().len(), 2);
    for account in accounts["lines"].as_array().unwrap() {
        let ids = drill_values(&env, &mut tx, &options, account).await;
        assert_eq!(ids.len(), 1);
        assert!(expected.contains(&ids[0]));
    }
    let mut peer = options.clone();
    peer["companies"] = json!([f.other]);
    let shown = runtime::report_lines(&env, &mut tx, &peer).await.unwrap();
    let root = &shown["lines"][0];
    assert_eq!(numbers(root), [num("91"), num("91"), Decimal::ZERO]);
    assert_eq!(drill_values(&env, &mut tx, &peer, root).await, peer_ids);
    let mut both = options.clone();
    both["companies"] = json!([f.company, f.other]);
    let error = runtime::report_lines(&env, &mut tx, &both)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "options.companies: this report covers one company."
    );
}
