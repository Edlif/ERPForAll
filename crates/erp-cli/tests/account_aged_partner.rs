// SPDX-License-Identifier: MIT OR Apache-2.0
//! Aged Receivable over real posted books, as an accounting reader: buckets at their boundaries,
//! per-item line ids, cell drills that match their cells, option and line id refusals, paging and
//! record rules.
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
    today: Date,
}

async fn setup() -> Fixture {
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
    let books = books(&mut tx, company).await;
    // the first user created must be an administrator
    let admin = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "aged.admin", company, &[admin]).await;
    let read = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader =
        erp_sdk::UserId::new(user(&mut tx, "aged.reader", company, &[read]).await).unwrap();
    let report = xml_id(&mut tx, "account_reporting", "report_aged_receivable").await;
    tx.commit().await.unwrap();
    let orm = policy_orm(&db, &orm).await;
    let tx = orm.begin(reader).await.unwrap();
    let today = tx.env().context_today().unwrap();
    drop(tx);
    Fixture {
        db,
        orm,
        company,
        books,
        reader,
        report,
        today,
    }
}

fn ago(f: &Fixture, days: i32) -> Date {
    Date::from_days(f.today.days() - days)
}

fn day(date: Date) -> String {
    let (y, m, d) = date.ymd();
    format!("{y:04}-{m:02}-{d:02}")
}

/// One posted entry of 2024 with a receivable item per `(partner, amount, due date)`.
async fn receivables(
    tx: &mut Transaction,
    f: &Fixture,
    items: &[(Option<i64>, &str, Date)],
) -> Records {
    let mut commands = Vec::new();
    let mut total = Decimal::ZERO;
    for (partner, amount, due) in items {
        let amount = num(amount);
        total += amount;
        let mut vals = RawVals::new()
            .set_cell("account_id", Cell::Id(f.books.receivable))
            .set_cell("balance", Cell::Decimal(amount))
            .set_cell("date_maturity", Cell::Date(*due));
        if let Some(partner) = partner {
            vals.insert("partner_id".into(), Cell::Id(*partner));
        }
        commands.push(erp_sdk::Command::create(vals));
    }
    commands.push(erp_sdk::Command::create(
        RawVals::new()
            .set_cell("account_id", Cell::Id(f.books.income))
            .set_cell("balance", Cell::Decimal(-total)),
    ));
    let created = moves(tx, f.company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(d(2024, 2, 1)))
                    .set_commands("line_ids", commands),
            ],
        )
        .await
        .unwrap();
    created.call::<(), ()>(tx, "action_post", ()).await.unwrap();
    created
}

async fn options(tx: &mut Transaction, f: &Fixture, extra: Json) -> Result<Json> {
    let mut input = json!({"report": f.report, "companies": [f.company]});
    for (key, value) in extra.as_object().unwrap() {
        input[key] = value.clone();
    }
    Ok(runtime::report_options(&tx.env(), tx, f.report, &input).await?["options"].clone())
}

fn col(options: &Json, label: &str) -> usize {
    options["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c["label"] == label)
        .unwrap()
}

fn amount(row: &Json, column: usize) -> Decimal {
    num(row["cells"][column]["value"].as_str().unwrap())
}

fn id_of(row: &Json) -> &str {
    row["id"].as_str().unwrap()
}

fn rows(answer: &Json) -> Vec<Json> {
    answer["lines"].as_array().unwrap().clone()
}

fn partner_row(lines: &[Json], partner: i64) -> Json {
    let suffix = format!("|group:partner_id~res.partner~{partner}");
    lines
        .iter()
        .find(|r| id_of(r).ends_with(&suffix))
        .unwrap()
        .clone()
}

/// The residual sum of the items an action's domain opens, read as the caller.
async fn drilled(tx: &mut Transaction, action: &Json) -> (Decimal, Vec<i64>) {
    let found = tx
        .env()
        .records("account.move.line")
        .unwrap()
        .search(tx, &Domain::from_json(&action["domain"]).unwrap())
        .await
        .unwrap();
    let values = found
        .read_with(tx, &["amount_residual"], erp_sdk::Load::Raw)
        .await
        .unwrap();
    let sum = values
        .iter()
        .map(|r| match r["amount_residual"] {
            Value::Decimal(n) => n,
            ref other => panic!("{other:?}"),
        })
        .sum();
    (sum, found.ids())
}

async fn refusal(result: Result<Json>) -> String {
    let error = result.unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation, "{error:?}");
    error.message().to_owned()
}

#[test]
fn aged_receivable_buckets_line_ids_drills_and_refusals() {
    big_stack(buckets);
}
async fn buckets() {
    let f = setup().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let other = partner(&mut tx, "Second customer").await;
    let customer = f.books.customer;
    let first = receivables(
        &mut tx,
        &f,
        &[
            (Some(customer), "100", f.today),
            (Some(customer), "40", ago(&f, 30)),
        ],
    )
    .await;
    receivables(
        &mut tx,
        &f,
        &[
            (Some(customer), "25", ago(&f, 31)),
            (Some(customer), "9", ago(&f, 8)),
            (Some(customer), "10", ago(&f, 150)),
            (Some(other), "7", ago(&f, 1)),
            (None, "3", ago(&f, -5)),
        ],
    )
    .await;
    let first_items = items_on(&mut tx, &[&first], f.books.receivable).await.ids();
    let gl = xml_id(&mut tx, "account_reporting", "report_general_ledger").await;
    tx.commit().await.unwrap();

    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let opts = options(&mut tx, &f, json!({})).await.unwrap();
    assert_eq!(opts["provider"]["interval_days"], 30);
    let [not_due, d1, d2, older, total, due_col] =
        ["not_due", "d1_30", "d31_60", "older", "total", "due_date"].map(|l| col(&opts, l));
    let top = rows(&runtime::report_lines(&env, &mut tx, &opts).await.unwrap());

    // due exactly on the cutoff, 30 and 31 days late with the default width
    let cust = partner_row(&top, customer);
    assert_eq!(amount(&cust, not_due), num("100"));
    assert_eq!(amount(&cust, d1), num("49"));
    assert_eq!(amount(&cust, d2), num("25"));
    assert_eq!(amount(&cust, older), num("10"));
    assert_eq!(amount(&cust, total), num("184"));

    // the total is the sum of the partner lines, column by column
    let (partners, grand): (Vec<_>, Vec<_>) = top
        .iter()
        .partition(|r| id_of(r).contains("|group:partner_id~"));
    let [grand] = grand.as_slice() else {
        panic!("one total line: {top:?}")
    };
    assert_eq!(partners.len(), 3);
    for label in [
        "not_due", "d1_30", "d31_60", "d61_90", "d91_120", "older", "total",
    ] {
        let c = col(&opts, label);
        let sum: Decimal = partners.iter().map(|r| amount(r, c)).sum();
        assert_eq!(amount(grand, c), sum, "{label}");
    }

    // every bucket cell of every partner line opens exactly its own items
    for row in &partners {
        for label in [
            "not_due", "d1_30", "d31_60", "d61_90", "d91_120", "older", "total",
        ] {
            let c = col(&opts, label);
            let action =
                runtime::report_drill(&env, &mut tx, &opts, id_of(row), &json!({"cell": c}))
                    .await
                    .unwrap();
            assert_eq!(drilled(&mut tx, &action).await.0, amount(row, c), "{label}");
        }
    }

    // the no-partner group drills to the items without a partner only
    let unknown = partner_row(&top, 0);
    let action = runtime::report_drill(
        &env,
        &mut tx,
        &opts,
        id_of(&unknown),
        &json!({"item": "journal_items"}),
    )
    .await
    .unwrap();
    let (sum, ids) = drilled(&mut tx, &action).await;
    assert_eq!(ids.len(), 1);
    assert_eq!(sum, amount(&unknown, total));

    // two receivable items of one entry are two lines with their own ids
    let children = rows(
        &runtime::report_expand(&env, &mut tx, &opts, id_of(&cust), 0)
            .await
            .unwrap(),
    );
    assert_eq!(children.len(), 5);
    let mut seen = std::collections::BTreeSet::new();
    for child in &children {
        assert!(seen.insert(id_of(child).to_owned()));
    }
    for item in &first_items {
        let suffix = format!("|group:id~account.move.line~{item}");
        let child = children
            .iter()
            .find(|c| id_of(c).ends_with(&suffix))
            .unwrap();
        for c in [not_due, d1, total] {
            let action =
                runtime::report_drill(&env, &mut tx, &opts, id_of(child), &json!({"cell": c}))
                    .await
                    .unwrap();
            assert_eq!(drilled(&mut tx, &action).await.0, amount(child, c));
        }
    }
    let on_cutoff = children
        .iter()
        .find(|c| c["cells"][due_col]["value"] == day(f.today))
        .unwrap();
    assert_eq!(amount(on_cutoff, not_due), num("100"));
    // a due date before the cutoff is marked, one on the cutoff is not
    assert!(on_cutoff["cells"][due_col].get("tone").is_none());
    let late = children
        .iter()
        .find(|c| c["cells"][due_col]["value"] == day(ago(&f, 30)))
        .unwrap();
    assert_eq!(late["cells"][due_col]["tone"], "danger");
    let entry = runtime::report_drill(
        &env,
        &mut tx,
        &opts,
        id_of(on_cutoff),
        &json!({"item": "entry"}),
    )
    .await
    .unwrap();
    assert_eq!(entry["res_id"], first.ids()[0]);

    // another width moves the same items
    let weekly = options(&mut tx, &f, json!({"provider": {"interval_days": 7}}))
        .await
        .unwrap();
    let top = rows(&runtime::report_lines(&env, &mut tx, &weekly).await.unwrap());
    let cust = partner_row(&top, customer);
    assert_eq!(amount(&cust, not_due), num("100"));
    assert_eq!(amount(&cust, d1), Decimal::ZERO);
    assert_eq!(amount(&cust, d2), num("9"));
    assert_eq!(amount(&cust, older), num("75"));
    let action = runtime::report_drill(&env, &mut tx, &weekly, id_of(&cust), &json!({"cell": d2}))
        .await
        .unwrap();
    assert_eq!(drilled(&mut tx, &action).await.0, num("9"));

    // option refusals
    for (provider, message) in [
        (
            json!({"bogus": 1}),
            "Invalid Aged Receivable option bogus. Use interval_days.",
        ),
        (
            json!({"interval_days": 0}),
            "Aged Receivable option interval_days must be a whole number of days from 1 to 365.",
        ),
        (
            json!({"interval_days": 366}),
            "Aged Receivable option interval_days must be a whole number of days from 1 to 365.",
        ),
    ] {
        let got = refusal(options(&mut tx, &f, json!({"provider": provider})).await).await;
        assert_eq!(got, message);
    }

    // a past cutoff is refused, never answered with today's residuals: the report is current
    // only, so opening it falls back to today and a request for a past date is refused by name
    let mut past = options(
        &mut tx,
        &f,
        json!({"date": {"preset": "custom", "to": "2024-02-28"}}),
    )
    .await
    .unwrap();
    assert_eq!(past["date"]["preset"], "today", "{past}");
    past["date"] = json!({"mode": "single", "preset": "custom", "from": null, "to": "2024-02-28"});
    let got = refusal(runtime::report_lines(&env, &mut tx, &past).await).await;
    assert!(got.contains("reads current stock or balances"), "{got}");

    // line id and target refusals
    let root = format!("root~reporting.report~{}", f.report);
    for (line, target, message) in [
        (
            format!("{root}|group:partner_id~res.partner~abc"),
            json!({"item": "journal_items"}),
            "This line does not belong to Aged Receivable.".to_owned(),
        ),
        (
            format!("{root}|group:move_id~account.move~{}", first.ids()[0]),
            json!({"item": "journal_items"}),
            "This line does not belong to Aged Receivable.".to_owned(),
        ),
        (
            format!("root~reporting.report~{gl}|group:partner_id~res.partner~{customer}"),
            json!({"item": "journal_items"}),
            "This line is not part of this Aged Receivable.".to_owned(),
        ),
        (
            id_of(&cust).to_owned(),
            json!({"item": "bogus"}),
            "This Aged Receivable line has no action bogus.".to_owned(),
        ),
        (
            id_of(&cust).to_owned(),
            json!({"cell": 99}),
            "Aged Receivable has no column 99.".to_owned(),
        ),
        (
            id_of(&cust).to_owned(),
            json!({"cell": due_col}),
            "The Aged Receivable column due_date has no amount to open.".to_owned(),
        ),
    ] {
        let got = refusal(runtime::report_drill(&env, &mut tx, &opts, &line, &target).await).await;
        assert_eq!(got, message, "{line}");
    }
    let got = refusal(
        runtime::report_expand(
            &env,
            &mut tx,
            &opts,
            &format!("{root}|group:partner_id~res.partner~-1"),
            0,
        )
        .await,
    )
    .await;
    assert_eq!(got, "This line does not belong to Aged Receivable.");
    assert!(
        runtime::report_expand(&env, &mut tx, &opts, "garbage", 0)
            .await
            .is_err()
    );
}

#[test]
fn aged_receivable_pages_items_and_honors_record_rules() {
    big_stack(pages_and_rules);
}
async fn pages_and_rules() {
    let f = setup().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let pager = partner(&mut tx, "Pager").await;
    let many: Vec<(Option<i64>, &str, Date)> =
        (0..12).map(|i| (Some(pager), "1.5", ago(&f, i))).collect();
    let entry = receivables(&mut tx, &f, &many).await;
    receivables(&mut tx, &f, &[(Some(f.books.customer), "20", f.today)]).await;
    let items = items_on(&mut tx, &[&entry], f.books.receivable).await.ids();
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
    tx.commit().await.unwrap();

    let mut tx = f.orm.begin(f.reader).await.unwrap();
    let env = tx.env();
    let opts = options(&mut tx, &f, json!({})).await.unwrap();
    let total = col(&opts, "total");
    let top = rows(&runtime::report_lines(&env, &mut tx, &opts).await.unwrap());
    let line = partner_row(&top, pager);
    let first = rows(
        &runtime::report_expand(&env, &mut tx, &opts, id_of(&line), 0)
            .await
            .unwrap(),
    );
    assert_eq!(first.len(), 11);
    let more = first.last().unwrap();
    assert_eq!(more["kind"], "more");
    assert_eq!(more["remaining"], 2);
    assert_eq!(more["next_offset"], 10);
    let second = rows(
        &runtime::report_expand(&env, &mut tx, &opts, id_of(&line), 10)
            .await
            .unwrap(),
    );
    assert_eq!(second.len(), 2);
    let mut shown: Vec<String> = first[..10]
        .iter()
        .chain(&second)
        .map(|r| id_of(r).to_owned())
        .collect();
    let paged_sum: Decimal = first[..10]
        .iter()
        .chain(&second)
        .map(|r| amount(r, total))
        .sum();
    assert_eq!(paged_sum, amount(&line, total));
    shown.sort();
    shown.dedup();
    assert_eq!(shown.len(), items.len());
    for item in &items {
        let suffix = format!("|group:id~account.move.line~{item}");
        assert!(shown.iter().any(|s| s.ends_with(&suffix)));
    }
    tx.commit().await.unwrap();

    // a read rule on the journal items: the reader sees only the customer's
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let model = xml_id(&mut tx, "account", "model_account_move_line").await;
    tx.env()
        .records("ir.access")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Aged customer restriction"))
                    .set_cell("model_id", Cell::Id(model))
                    .set_cell("operation", text("r"))
                    .set_cell(
                        "domain",
                        text(&format!("[('partner_id', '=', {})]", f.books.customer)),
                    ),
            ],
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let restricted = policy_orm(&f.db, &f.orm).await;
    let mut tx = restricted.begin(f.reader).await.unwrap();
    let env = tx.env();
    let top = rows(&runtime::report_lines(&env, &mut tx, &opts).await.unwrap());
    assert_eq!(top.len(), 2, "{top:?}");
    let cust = partner_row(&top, f.books.customer);
    assert_eq!(amount(&cust, total), num("20"));
    assert_eq!(amount(&top[1], total), amount(&cust, total));
    let hidden = rows(
        &runtime::report_expand(&env, &mut tx, &opts, id_of(&line), 0)
            .await
            .unwrap(),
    );
    assert!(hidden.is_empty());
}
