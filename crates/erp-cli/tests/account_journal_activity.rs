// SPDX-License-Identifier: MIT OR Apache-2.0
//! Posted activity through the installed report definition and caller ORM.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration assertions"
)]

#[allow(
    clippy::panic,
    reason = "shared fixture readers assert the expected field type"
)]
#[path = "support/accounting.rs"]
mod accounting;

use accounting::*;
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
    assert!(
        error
            .message()
            .contains("not one of the companies you may use"),
        "{error:?}"
    );
    drop(tx);
    let mut tx = f.orm.begin(f.outsider).await.unwrap();
    let error = runtime::report_lines(&tx.env(), &mut tx, &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Access, "{error:?}");
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
    for (label, expected) in [("Income", "120"), ("Expenses", "40"), ("Net profit", "80")] {
        let line = lines.iter().find(|line| line["name"] == label).unwrap();
        assert_eq!(numbers(line), [num(expected)], "{label}");
    }
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
    let root = &shown["lines"][0];
    assert_eq!(
        numbers(root),
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
    let expanded = runtime::report_expand(&env, &mut tx, &options, root["id"].as_str().unwrap(), 0)
        .await
        .unwrap();
    let rows = expanded["lines"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        5,
        "opening-only and current-only accounts must remain visible"
    );
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
        let suffix = format!("group:account_id~account.account~{account}");
        let row = rows
            .iter()
            .find(|r| r["id"].as_str().unwrap().ends_with(&suffix))
            .unwrap();
        let actual = numbers(row);
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
    assert_eq!(totals.to_vec(), numbers(root));
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
    assert_eq!(
        numbers(&shown["lines"][0]),
        [
            num("81.01"),
            num("37.25"),
            num("11.05"),
            num("107.21"),
            num("8.91"),
            num("72.10"),
            num("0"),
            num("81.01")
        ]
    );
    let restricted_rows = runtime::report_expand(
        &env,
        &mut tx,
        &options,
        shown["lines"][0]["id"].as_str().unwrap(),
        0,
    )
    .await
    .unwrap();
    assert_eq!(restricted_rows["lines"].as_array().unwrap().len(), 1);
    let row = &restricted_rows["lines"][0];
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
fn ledger_trial_balance_paging_preserves_each_scope_and_refuses_ambiguous_dates() {
    big_stack(trial_balance_pages);
}

async fn trial_balance_pages() {
    let f = fixture().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    for i in 0..12 {
        let account = account(&mut tx, f.company, &format!("6200{i:02}"), "expense").await;
        trial_entry(&mut tx, &f, account, f.books.bank, "1.01", d(2024, 1, 10)).await;
    }
    let report = xml_id(&mut tx, "account_reporting", "report_ledger_trial_balance").await;
    let definition = tx
        .env()
        .records("reporting.report")
        .unwrap()
        .browse([report])
        .unwrap();
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
    let root = &top["lines"][0];
    let root_id = root["id"].as_str().unwrap();
    let page = runtime::report_expand(&env, &mut tx, &options, root_id, 0)
        .await
        .unwrap();
    let rows = page["lines"].as_array().unwrap();
    assert_eq!(rows.len(), 11);
    let more = rows.last().unwrap();
    assert_eq!(more["remaining"], 4);
    let mut first = [Decimal::ZERO; 4];
    let mut ids = std::collections::BTreeSet::new();
    for row in &rows[..10] {
        assert!(ids.insert(row["id"].as_str().unwrap().to_owned()));
        for (sum, value) in first.iter_mut().zip(numbers(row)) {
            *sum += value;
        }
    }
    let mut with_more = first;
    for (sum, value) in with_more.iter_mut().zip(numbers(more)) {
        *sum += value;
    }
    assert_eq!(with_more.to_vec(), numbers(root));
    let second = runtime::report_expand(&env, &mut tx, &options, root_id, 10)
        .await
        .unwrap();
    let second = second["lines"].as_array().unwrap();
    assert_eq!(second.len(), 4);
    let mut remainder = [Decimal::ZERO; 4];
    for row in second {
        assert!(ids.insert(row["id"].as_str().unwrap().to_owned()));
        for (sum, value) in remainder.iter_mut().zip(numbers(row)) {
            *sum += value;
        }
    }
    assert_eq!(remainder.to_vec(), numbers(more));
    let mut empty = options.clone();
    empty["date"] = json!({"preset":"custom", "from":"2020-01-01", "to":"2020-01-31"});
    let zero = runtime::report_lines(&env, &mut tx, &empty).await.unwrap();
    assert_eq!(numbers(&zero["lines"][0]), [Decimal::ZERO; 4]);
    let zero = runtime::report_expand(&env, &mut tx, &empty, root_id, 0)
        .await
        .unwrap();
    assert!(zero["lines"].as_array().unwrap().is_empty());
    drop(tx);
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let column = xml_id(&mut tx, "account_reporting", "trial_balance_column_debit").await;
    tx.env()
        .records("reporting.report.column")
        .unwrap()
        .browse([column])
        .unwrap()
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
    let error = runtime::report_expand(&tx.env(), &mut tx, &sorted, root_id, 0)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "options.order: column debit is not sortable."
    );
    drop(tx);
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
        "The before_period date scope requires a date range with a start date."
    );
    drop(tx);
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let expression = xml_id(
        &mut tx,
        "account_reporting",
        "trial_balance_expression_opening",
    )
    .await;
    let error = tx
        .env()
        .records("reporting.report.expression")
        .unwrap()
        .browse([expression])
        .unwrap()
        .write(&mut tx, RawVals::new().set_cell("date_field", Cell::Null))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Report 'Ledger Trial Balance': line TRIAL_BALANCE, opening: The before_period date scope requires a date field."
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
    partner_entry(&mut tx, &f, ap, Some(q), "-90", d(2024, 2, 15)).await;
    partner_entry(&mut tx, &f, ar, None, "3.01", d(2024, 2, 29)).await;
    partner_entry(&mut tx, &f, ar, Some(p), "99", d(2024, 3, 1)).await;
    let expected = items_on(&mut tx, &[&prior, &current], ar.0).await.ids();
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
    let roots = shown["lines"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    for (root, expected) in roots
        .iter()
        .zip([["120", "3.01", "50", "73.01"], ["0", "0", "90", "-90"]])
    {
        assert_eq!(numbers(root), expected.map(num));
    }
    let expanded =
        runtime::report_expand(&env, &mut tx, &options, roots[0]["id"].as_str().unwrap(), 0)
            .await
            .unwrap();
    let partners = expanded["lines"].as_array().unwrap();
    assert_eq!(partners.len(), 2);
    let suffix = format!("group:partner_id~res.partner~{p}");
    let customer = partners
        .iter()
        .find(|line| line["id"].as_str().unwrap().ends_with(&suffix))
        .unwrap();
    assert_eq!(numbers(customer), ["120", "0", "50", "70"].map(num));
    let accounts =
        runtime::report_expand(&env, &mut tx, &options, customer["id"].as_str().unwrap(), 0)
            .await
            .unwrap();
    let accounts = accounts["lines"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(numbers(&accounts[0]), numbers(customer));
    let drill = runtime::report_drill(
        &env,
        &mut tx,
        &options,
        accounts[0]["id"].as_str().unwrap(),
        &json!({"cell":3}),
    )
    .await
    .unwrap();
    let mut items = env
        .records("account.move.line")
        .unwrap()
        .search(&mut tx, &Domain::from_json(&drill["domain"]).unwrap())
        .await
        .unwrap()
        .ids();
    items.sort_unstable();
    let mut expected = expected;
    expected.sort_unstable();
    assert_eq!(items, expected);
}
