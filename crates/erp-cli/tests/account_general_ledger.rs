// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only GL over real posted books, as an accounting reader.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]
#[path = "support/accounting.rs"]
mod accounting;
use accounting::*;
use erp_addon_reporting::runtime;
use erp_modules::{InitOptions, ModuleName};
use erp_sdk::prelude::*;
use erp_sdk::{Context, SUPERUSER_ID};
use erp_testkit::TestDb;
use serde_json::{Value as Json, json};

struct Fixture {
    db: TestDb,
    orm: erp_sdk::Orm,
    company: i64,
    books: Books,
    reader: erp_sdk::UserId,
    report: i64,
    journal: i64,
    allowance: std::time::Duration,
}
async fn setup() -> Fixture {
    let started = std::time::Instant::now();
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let system = system(&db, None);
    let initialized = system
        .initialize(InitOptions::new().with_skip_auto_install(true))
        .await
        .unwrap();
    let installed = system
        .button_immediate_install(
            &initialized.orm.env(SUPERUSER_ID, Context::new()),
            &[ModuleName::new("account_reporting").unwrap()],
        )
        .await
        .unwrap();
    let orm = installed.orm;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let equity = account(&mut tx, company, "900000", "equity").await;
    let admin = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "gl.admin", company, &[admin]).await;
    let read = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader = erp_sdk::UserId::new(user(&mut tx, "gl.reader", company, &[read]).await).unwrap();
    // Original ledger L (2024 dates): insert payment before receipt to test chronological sorting.
    let opening = posted_entry(&mut tx, &b, b.bank, equity, None, "1000", d(2024, 1, 1)).await;
    let journal = opening.related(&mut tx, "journal_id").await.unwrap().ids()[0];
    posted_entry(&mut tx, &b, b.payable, b.bank, None, "10", d(2024, 2, 20)).await;
    posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.income,
        Some(b.customer),
        "120",
        d(2024, 2, 2),
    )
    .await;
    posted_entry(
        &mut tx,
        &b,
        b.bank,
        b.receivable,
        Some(b.customer),
        "50",
        d(2024, 2, 10),
    )
    .await;
    posted_entry(
        &mut tx,
        &b,
        b.expense,
        b.payable,
        Some(b.vendor),
        "40",
        d(2024, 2, 3),
    )
    .await;
    let report = xml_id(&mut tx, "account_reporting", "report_general_ledger").await;
    tx.install_audit_schema().await.unwrap();
    tx.commit().await.unwrap();
    let orm = policy_orm(&db, &orm).await;
    Fixture {
        db,
        orm,
        company,
        books: b,
        reader,
        report,
        journal,
        allowance: started.elapsed().saturating_mul(2),
    }
}

fn amount(row: &Json, column: usize) -> Decimal {
    num(row["cells"][column]["value"].as_str().unwrap())
}

#[test]
fn general_ledger_orders_real_items_and_reconciles_opening_running_and_closing() {
    big_stack(oracle);
}
async fn oracle() {
    let f = setup().await;
    let (orm, company, b, reader, report) = (&f.orm, f.company, &f.books, f.reader, f.report);
    let mut tx = orm.begin(reader).await.unwrap();
    let env = tx.env();
    let input = json!({"report":report,"companies":[company],"date":{"preset":"custom","from":"2024-02-01","to":"2024-02-29"}});
    let options = runtime::report_options(&env, &mut tx, report, &input)
        .await
        .unwrap()["options"]
        .clone();
    let top = runtime::report_lines(&env, &mut tx, &options)
        .await
        .unwrap();
    assert_eq!(amount(&top["lines"][0], 2), num("220"));
    assert_eq!(amount(&top["lines"][0], 3), num("220"));
    assert_eq!(amount(&top["lines"][0], 4), Decimal::ZERO);
    let root = top["lines"][0]["id"].as_str().unwrap();
    let accounts = runtime::report_expand(&env, &mut tx, &options, root, 0)
        .await
        .unwrap();
    let suffix = format!("group:account_id~account.account~{}", b.bank);
    let cash = accounts["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"].as_str().unwrap().ends_with(&suffix))
        .unwrap();
    let items = runtime::report_expand(&env, &mut tx, &options, cash["id"].as_str().unwrap(), 0)
        .await
        .unwrap();
    let rows = items["lines"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter().map(|r| amount(r, 4)).collect::<Vec<_>>(),
        [num("1000"), num("1050"), num("1040"), num("1040")]
    );
    assert_eq!(rows[1]["cells"][0]["kind"], "date");
    assert_eq!(rows[1]["cells"][0]["value"], "2024-02-10");
    assert_eq!(rows[2]["cells"][0]["value"], "2024-02-20");
    for row in rows {
        for (column, field) in [(2, "debit"), (3, "credit"), (4, "balance")] {
            if row["cells"][column]["value"].is_null() {
                continue;
            }
            let action = runtime::report_drill(
                &env,
                &mut tx,
                &options,
                row["id"].as_str().unwrap(),
                &json!({"cell":column}),
            )
            .await
            .unwrap();
            let source = env
                .records("account.move.line")
                .unwrap()
                .search(&mut tx, &Domain::from_json(&action["domain"]).unwrap())
                .await
                .unwrap();
            let values = source
                .read_with(&mut tx, &[field], erp_sdk::Load::Raw)
                .await
                .unwrap();
            let sum: Decimal = values
                .iter()
                .map(|r| match r[field] {
                    Value::Decimal(n) => n,
                    ref other => panic!("{other:?}"),
                })
                .sum();
            assert_eq!(sum, amount(row, column));
        }
    }
    tx.commit().await.unwrap();
    erp_addon_account::fiscal::ensure_periods_with(
        orm,
        erp_sdk::BeginOptions::new(SUPERUSER_ID),
        &erp_addon_account::fiscal::settings::FiscalCalendar::new(
            erp_addon_account::fiscal::calendar::FiscalYearEnd::new(31, 12).unwrap(),
            12,
        )
        .unwrap(),
        company,
        d(2023, 1, 1),
        d(2023, 12, 31),
    )
    .await
    .unwrap();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    post(&mut tx, &f, "3", d(2023, 12, 31)).await;
    tx.commit().await.unwrap();
    let mut tx = orm.begin(reader).await.unwrap();
    let opts = ledger_options(&mut tx, &f).await;
    let cash = runtime::report_expand(&tx.env(), &mut tx, &opts, &cash_id(&f), 0)
        .await
        .unwrap();
    assert_eq!(amount(&cash["lines"][0], 4), num("1003"));
    assert_eq!(
        amount(cash["lines"].as_array().unwrap().last().unwrap(), 4),
        num("1043")
    );
    let income_id = format!(
        "root~reporting.report~{}|line:GL~~|group:account_id~account.account~{}",
        report, b.income
    );
    let income = runtime::report_expand(&tx.env(), &mut tx, &opts, &income_id, 0)
        .await
        .unwrap();
    assert_eq!(amount(&income["lines"][0], 4), num("-3"));
    assert_eq!(
        amount(income["lines"].as_array().unwrap().last().unwrap(), 4),
        num("-123")
    );
    let crossing = json!({"report":report,"companies":[company],"date":{"preset":"custom","from":"2023-12-01","to":"2024-02-29"}});
    let opts = runtime::report_options(&tx.env(), &mut tx, report, &crossing)
        .await
        .unwrap()["options"]
        .clone();
    let cash = runtime::report_expand(&tx.env(), &mut tx, &opts, &cash_id(&f), 0)
        .await
        .unwrap();
    assert_eq!(amount(&cash["lines"][0], 4), Decimal::ZERO);
    assert_eq!(
        amount(cash["lines"].as_array().unwrap().last().unwrap(), 4),
        num("1043")
    );
    let empty = json!({"report":report,"companies":[company],"date":{"preset":"custom","from":"2022-01-01","to":"2022-01-31"}});
    let opts = runtime::report_options(&tx.env(), &mut tx, report, &empty)
        .await
        .unwrap()["options"]
        .clone();
    let top = runtime::report_lines(&tx.env(), &mut tx, &opts)
        .await
        .unwrap();
    assert_eq!(amount(&top["lines"][0], 4), Decimal::ZERO);
    let empty = runtime::report_expand(
        &tx.env(),
        &mut tx,
        &opts,
        top["lines"][0]["id"].as_str().unwrap(),
        0,
    )
    .await
    .unwrap();
    assert_eq!(empty["lines"], json!([]));
}

async fn post(tx: &mut Transaction, f: &Fixture, value: &str, date: Date) {
    let value = num(value);
    let record = moves(tx, f.company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(date))
                    .set_cell("journal_id", Cell::Id(f.journal))
                    .set_commands(
                        "line_ids",
                        vec![
                            erp_sdk::Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(f.books.bank))
                                    .set_cell("balance", Cell::Decimal(value)),
                            ),
                            erp_sdk::Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(f.books.income))
                                    .set_cell("balance", Cell::Decimal(-value)),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap();
    record.call::<(), ()>(tx, "action_post", ()).await.unwrap();
}
fn input(f: &Fixture) -> Json {
    json!({"report":f.report,"companies":[f.company],"date":{"preset":"custom","from":"2024-02-01","to":"2024-02-29"}})
}
async fn ledger_options(tx: &mut Transaction, f: &Fixture) -> Json {
    runtime::report_options(&tx.env(), tx, f.report, &input(f))
        .await
        .unwrap()["options"]
        .clone()
}
fn cash_id(f: &Fixture) -> String {
    format!(
        "root~reporting.report~{}|line:GL~~|group:account_id~account.account~{}",
        f.report, f.books.bank
    )
}

#[test]
fn general_ledger_pages_reconcile_and_late_commit_or_rollback_has_exact_staleness() {
    big_stack(pages);
}
async fn pages() {
    let f = setup().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    for _ in 0..13 {
        post(&mut tx, &f, "1.01", d(2024, 2, 15)).await;
    }
    post(&mut tx, &f, "-2.02", d(2024, 2, 15)).await;
    tx.env()
        .records("reporting.report")
        .unwrap()
        .browse([f.report])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("load_more_limit", Cell::Int(10)),
        )
        .await
        .unwrap();
    let other = other_company(&mut tx, f.company).await;
    tx.commit().await.unwrap();
    periods(&f.orm, other).await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let other_books = books(&mut tx, other).await;
    tx.commit().await.unwrap();

    // A lower/earlier transaction remains uncommitted while another company commits.
    // An UPDATE of one shared generation row would hold a posting-wide lock and fail this.
    let mut late = f.orm.begin(SUPERUSER_ID).await.unwrap();
    post(&mut late, &f, "0.03", d(2024, 2, 19)).await;
    tokio::time::timeout(f.allowance, async {
        let mut independent = f.orm.begin(SUPERUSER_ID).await.unwrap();
        posted_entry(
            &mut independent,
            &other_books,
            other_books.bank,
            other_books.income,
            None,
            "7",
            d(2024, 2, 16),
        )
        .await;
        independent.commit().await.unwrap();
    })
    .await
    .expect("independent signal insert must commit while the earlier posting remains open");
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let opts = ledger_options(&mut tx, &f).await;
    let first = runtime::report_expand(&env, &mut tx, &opts, &cash_id(&f), 0)
        .await
        .unwrap();
    let second = runtime::report_expand(&env, &mut tx, &opts, &cash_id(&f), 10)
        .await
        .unwrap();
    let mut actual = Vec::new();
    for page in [&first, &second] {
        for row in page["lines"].as_array().unwrap() {
            if row["cells"][0]["kind"] == "date" {
                actual.push(row);
            }
        }
    }
    assert_eq!(actual.len(), 16);
    let source = env
        .records("account.move.line")
        .unwrap()
        .search(
            &mut tx,
            &Domain::from_json(&json!([
                ["company_id", "=", f.company],
                ["account_id", "=", f.books.bank],
                ["parent_state", "=", "posted"],
                ["journal_id.journal_group_id", "=", false],
                ["date", ">=", "2024-02-01"],
                ["date", "<=", "2024-02-29"]
            ]))
            .unwrap(),
        )
        .await
        .unwrap();
    let raw = source
        .read_with(&mut tx, &["id", "date", "balance"], erp_sdk::Load::Raw)
        .await
        .unwrap();
    let mut expected: Vec<(Date, i64, Decimal)> = raw
        .iter()
        .map(|row| {
            let Value::Date(date) = row["date"] else {
                panic!("no source date")
            };
            let Value::Int(id) = row["id"] else {
                panic!("no source id")
            };
            let Value::Decimal(balance) = row["balance"] else {
                panic!("no source amount")
            };
            (date, id, balance)
        })
        .collect();
    expected.sort_by_key(|(date, id, _)| (*date, *id));
    let mut running = num("1000");
    let mut seen = std::collections::BTreeSet::new();
    for (row, (date, id, balance)) in actual.iter().zip(expected) {
        assert!(seen.insert(id));
        running += balance;
        assert_eq!(
            row["cells"][0]["value"],
            format!(
                "{:04}-{:02}-{:02}",
                date.ymd().0,
                date.ymd().1,
                date.ymd().2
            )
        );
        assert!(
            row["id"]
                .as_str()
                .unwrap()
                .ends_with(&format!("group:id~account.move.line~{id}"))
        );
        assert_eq!(amount(row, 4), running);
    }
    assert_eq!(running, num("1051.11"));
    assert_eq!(
        amount(second["lines"].as_array().unwrap().last().unwrap(), 4),
        running
    );
    tx.commit().await.unwrap();
    late.commit().await.unwrap();
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let error = runtime::report_expand(&tx.env(), &mut tx, &opts, &cash_id(&f), 10)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "The ledger source or access changed. Refresh the General Ledger before continuing."
    );
    let refreshed = ledger_options(&mut tx, &f).await;
    let page = runtime::report_expand(&tx.env(), &mut tx, &refreshed, &cash_id(&f), 10)
        .await
        .unwrap();
    assert_eq!(
        amount(page["lines"].as_array().unwrap().last().unwrap(), 4),
        num("1051.14")
    );
    tx.commit().await.unwrap();
    let mut rolled_back = f.orm.begin(SUPERUSER_ID).await.unwrap();
    post(&mut rolled_back, &f, "0.05", d(2024, 2, 17)).await;
    rolled_back.rollback().await.unwrap();
    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let page = runtime::report_expand(&tx.env(), &mut tx, &refreshed, &cash_id(&f), 10)
        .await
        .unwrap();
    assert_eq!(
        amount(page["lines"].as_array().unwrap().last().unwrap(), 4),
        num("1051.14")
    );
}

#[test]
fn general_ledger_honors_source_rules_and_refuses_foreign_or_forged_drills() {
    big_stack(rights);
}
async fn rights() {
    let f = setup().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let alternate = tx
        .env()
        .records("account.journal.group")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("name", text("GL management ledger"))],
        )
        .await
        .unwrap()
        .ids()[0];
    let alt_journal = journal(&mut tx, f.company, "GLALT", "general", None).await;
    tx.env()
        .records("account.journal")
        .unwrap()
        .browse([alt_journal])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("journal_group_id", Cell::Id(alternate)),
        )
        .await
        .unwrap();
    for (journal, posted) in [(f.journal, false), (alt_journal, true)] {
        let entry = moves(&tx, f.company)
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("date", Cell::Date(d(2024, 2, 12)))
                        .set_cell("journal_id", Cell::Id(journal))
                        .set_commands(
                            "line_ids",
                            vec![
                                erp_sdk::Command::create(
                                    RawVals::new()
                                        .set_cell("account_id", Cell::Id(f.books.bank))
                                        .set_cell("balance", Cell::Decimal(num("77"))),
                                ),
                                erp_sdk::Command::create(
                                    RawVals::new()
                                        .set_cell("account_id", Cell::Id(f.books.income))
                                        .set_cell("balance", Cell::Decimal(num("-77"))),
                                ),
                            ],
                        ),
                ],
            )
            .await
            .unwrap();
        if posted {
            entry
                .call::<(), ()>(&mut tx, "action_post", ())
                .await
                .unwrap();
        }
    }
    let other = other_company(&mut tx, f.company).await;
    tx.commit().await.unwrap();
    periods(&f.orm, other).await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let b2 = books(&mut tx, other).await;
    posted_entry(
        &mut tx,
        &b2,
        b2.bank,
        b2.income,
        None,
        "888",
        d(2024, 2, 12),
    )
    .await;
    let model = xml_id(&mut tx, "account", "model_account_move_line").await;
    tx.env()
        .records("ir.access")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("GL cash source restriction"))
                    .set_cell("model_id", Cell::Id(model))
                    .set_cell("operation", text("r"))
                    .set_cell(
                        "domain",
                        text(&format!("[('account_id', '=', {})]", f.books.bank)),
                    ),
            ],
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let restricted = policy_orm(&f.db, &f.orm).await;
    let mut tx = restricted.begin(f.reader).await.unwrap();
    let opts = ledger_options(&mut tx, &f).await;
    let top = runtime::report_lines(&tx.env(), &mut tx, &opts)
        .await
        .unwrap();
    assert_eq!(amount(&top["lines"][0], 2), num("50"));
    assert_eq!(amount(&top["lines"][0], 3), num("10"));
    assert_eq!(amount(&top["lines"][0], 4), num("1040"));
    let accounts = runtime::report_expand(
        &tx.env(),
        &mut tx,
        &opts,
        top["lines"][0]["id"].as_str().unwrap(),
        0,
    )
    .await
    .unwrap();
    assert_eq!(accounts["lines"].as_array().unwrap().len(), 1);
    let forged = format!(
        "root~reporting.report~{}|line:GL~~|group:account_id~account.account~{}",
        f.report, f.books.income
    );
    let error = runtime::report_expand(&tx.env(), &mut tx, &opts, &forged, 0)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "This account has no visible posted items in the General Ledger scope."
    );
    let error = runtime::report_drill(
        &tx.env(),
        &mut tx,
        &opts,
        &format!(
            "{}|group:id~account.move.line~9223372036854775807",
            cash_id(&f)
        ),
        &json!({"item":"open"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "This journal item is not visible in the General Ledger scope."
    );
}
