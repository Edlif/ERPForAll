// SPDX-License-Identifier: MIT OR Apache-2.0
//! Posting homogeneity and caller-scoped historical Balance Sheet admission.
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
    journal: i64,
    report: i64,
    reader: erp_sdk::UserId,
}
async fn setup() -> Fixture {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let system = system(&db, None);
    let initial = system
        .initialize(InitOptions::new().with_skip_auto_install(true))
        .await
        .unwrap();
    let installed = system
        .button_immediate_install(
            &initial.orm.env(SUPERUSER_ID, Context::new()),
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
    let journal = journal(&mut tx, company, "OFFCHK", "general", None).await;
    let group = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "off.guard.admin", company, &[group]).await;
    let group = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader =
        erp_sdk::UserId::new(user(&mut tx, "off.guard.reader", company, &[group]).await).unwrap();
    let report = xml_id(&mut tx, "account_reporting", "report_balance_sheet").await;
    tx.install_audit_schema().await.unwrap();
    tx.commit().await.unwrap();
    Fixture {
        db,
        orm,
        company,
        books,
        journal,
        report,
        reader,
    }
}
struct Entry {
    debit: i64,
    credit: i64,
    date: Date,
    value: &'static str,
    journal: i64,
}
async fn entry(tx: &mut Transaction, company: i64, e: Entry) -> Records {
    moves(tx, company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(e.date))
                    .set_cell("journal_id", Cell::Id(e.journal))
                    .set_commands(
                        "line_ids",
                        vec![
                            erp_sdk::Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(e.debit))
                                    .set_cell("balance", Cell::Decimal(num(e.value))),
                            ),
                            erp_sdk::Command::create(
                                RawVals::new()
                                    .set_cell("account_id", Cell::Id(e.credit))
                                    .set_cell("balance", Cell::Decimal(-num(e.value))),
                            ),
                            erp_sdk::Command::create(
                                RawVals::new()
                                    .set_cell("display_type", text("line_note"))
                                    .set_cell("name", text("Non-accounting note")),
                            ),
                        ],
                    ),
            ],
        )
        .await
        .unwrap()
}
async fn reclassify(tx: &mut Transaction, id: i64) {
    tx.env()
        .records("account.account")
        .unwrap()
        .browse([id])
        .unwrap()
        .write(
            tx,
            RawVals::new().set_cell("account_type", text("off_balance")),
        )
        .await
        .unwrap();
}
fn input(f: &Fixture, date: &str) -> Json {
    json!({"report":f.report,"companies":[f.company],"date":{"preset":"custom","to":date}})
}
fn amount(output: &Json, name: &str) -> Decimal {
    let row = output["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap();
    num(row["cells"][0]["value"].as_str().unwrap())
}
fn refused(error: Error) {
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Balance Sheet cannot include a posted entry mixing ordinary and off-balance accounts. Correct the ledger classification before continuing."
    );
}

#[test]
fn posting_allows_homogeneous_books_and_refuses_a_mixed_entry_atomically() {
    big_stack(posting);
}
async fn posting() {
    let f = setup().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let off1 = account(&mut tx, f.company, "OFF101", "off_balance").await;
    let off2 = account(&mut tx, f.company, "OFF102", "off_balance").await;
    for (debit, credit) in [(f.books.bank, f.books.income), (off1, off2)] {
        let row = entry(
            &mut tx,
            f.company,
            Entry {
                debit,
                credit,
                date: d(2024, 2, 1),
                value: "7",
                journal: f.journal,
            },
        )
        .await;
        row.call::<(), ()>(&mut tx, "action_post", ())
            .await
            .unwrap();
        assert_eq!(
            row.values(&mut tx, "state").await.unwrap(),
            [Value::str("posted")]
        );
    }
    let row = entry(
        &mut tx,
        f.company,
        Entry {
            debit: f.books.bank,
            credit: off1,
            date: d(2024, 2, 1),
            value: "9",
            journal: f.journal,
        },
    )
    .await;
    let error = row
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::UserError);
    assert_eq!(
        error.message(),
        "Off-balance accounts cannot be mixed with ordinary accounts in one journal entry."
    );
    assert_eq!(
        row.values(&mut tx, "state").await.unwrap(),
        [Value::str("draft")]
    );
}

#[test]
fn balance_sheet_historical_guard_obeys_company_date_ledger_and_source_rules() {
    big_stack(historical);
}
async fn historical() {
    let f = setup().await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let off1 = account(&mut tx, f.company, "OFF201", "off_balance").await;
    let off2 = account(&mut tx, f.company, "OFF202", "off_balance").await;
    for (debit, credit) in [(f.books.bank, f.books.income), (off1, off2)] {
        let row = entry(
            &mut tx,
            f.company,
            Entry {
                debit,
                credit,
                date: d(2024, 2, 1),
                value: "7",
                journal: f.journal,
            },
        )
        .await;
        row.call::<(), ()>(&mut tx, "action_post", ())
            .await
            .unwrap();
    }
    let legacy = account(&mut tx, f.company, "LEGACY", "equity").await;
    let row = entry(
        &mut tx,
        f.company,
        Entry {
            debit: f.books.bank,
            credit: legacy,
            date: d(2024, 3, 1),
            value: "9",
            journal: f.journal,
        },
    )
    .await;
    row.call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap();
    // A genuine posted entry becomes historically incompatible through supported account editing.
    reclassify(&mut tx, legacy).await;
    entry(
        &mut tx,
        f.company,
        Entry {
            debit: f.books.bank,
            credit: off1,
            date: d(2024, 2, 1),
            value: "77",
            journal: f.journal,
        },
    )
    .await;
    let group = tx
        .env()
        .records("account.journal.group")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("name", text("Guard management book"))],
        )
        .await
        .unwrap()
        .ids()[0];
    let alt = journal(&mut tx, f.company, "OFFALT", "general", None).await;
    tx.env()
        .records("account.journal")
        .unwrap()
        .browse([alt])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("journal_group_id", Cell::Id(group)),
        )
        .await
        .unwrap();
    let alt_credit = account(&mut tx, f.company, "ALTC", "equity").await;
    let row = entry(
        &mut tx,
        f.company,
        Entry {
            debit: f.books.bank,
            credit: alt_credit,
            date: d(2024, 2, 1),
            value: "55",
            journal: alt,
        },
    )
    .await;
    row.call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap();
    reclassify(&mut tx, alt_credit).await;
    let other = other_company(&mut tx, f.company).await;
    tx.commit().await.unwrap();
    periods(&f.orm, other).await;
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let b2 = books(&mut tx, other).await;
    let legacy2 = account(&mut tx, other, "LEGACY2", "equity").await;
    posted_entry(&mut tx, &b2, b2.bank, legacy2, None, "88", d(2024, 2, 1)).await;
    reclassify(&mut tx, legacy2).await;
    tx.commit().await.unwrap();
    let orm = policy_orm(&f.db, &f.orm).await;
    let mut tx = orm.begin(f.reader).await.unwrap();
    let february = runtime::report_lines(&tx.env(), &mut tx, &input(&f, "2024-02-29"))
        .await
        .unwrap();
    assert_eq!(amount(&february, "Assets"), num("7"));
    assert_eq!(amount(&february, "Liabilities and equity"), num("7"));
    let asset_id = format!("root~reporting.report~{}|line:ASSET~~", f.report);
    let accounts =
        runtime::report_expand(&tx.env(), &mut tx, &input(&f, "2024-02-29"), &asset_id, 0)
            .await
            .unwrap();
    let account_id = accounts["lines"][0]["id"].as_str().unwrap();
    let details =
        runtime::report_expand(&tx.env(), &mut tx, &input(&f, "2024-02-29"), account_id, 0)
            .await
            .unwrap();
    let leaf = details["lines"][0]["id"].as_str().unwrap();
    let mut stale = input(&f, "2024-02-29");
    stale["unfolded"] = json!([asset_id, account_id, leaf]);
    stale["loaded_pages"] = json!({leaf: 1});
    let error = runtime::report_lines(&tx.env(), &mut tx, &stale)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    assert_eq!(
        error.message(),
        format!("options.loaded_pages: {leaf} has no grouped children to page.")
    );
    let reset = runtime::report_options(&tx.env(), &mut tx, f.report, &stale)
        .await
        .unwrap();
    assert_eq!(reset["options"]["loaded_pages"], json!({}));
    let mut comparison = input(&f, "2024-02-29");
    comparison["comparison"] = json!({"mode":"custom","to":"2024-03-01"});
    refused(
        runtime::report_lines(&tx.env(), &mut tx, &comparison)
            .await
            .unwrap_err(),
    );
    let march = input(&f, "2024-03-01");
    refused(
        runtime::report_lines(&tx.env(), &mut tx, &march)
            .await
            .unwrap_err(),
    );
    let asset = format!("root~reporting.report~{}|line:ASSET~~", f.report);
    refused(
        runtime::report_expand(&tx.env(), &mut tx, &march, &asset, 0)
            .await
            .unwrap_err(),
    );
    refused(
        runtime::report_drill(&tx.env(), &mut tx, &march, &asset, &json!({"cell":0}))
            .await
            .unwrap_err(),
    );
    tx.commit().await.unwrap();

    // Hidden counterpart lines must not be inspected through a bypassing relation predicate.
    let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
    let model = xml_id(&mut tx, "account", "model_account_move_line").await;
    tx.env()
        .records("ir.access")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Guard cash-only source"))
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
    let orm = policy_orm(&f.db, &f.orm).await;
    let mut tx = orm.begin(f.reader).await.unwrap();
    let visible = runtime::report_lines(&tx.env(), &mut tx, &march)
        .await
        .unwrap();
    assert_eq!(amount(&visible, "Assets"), num("16"));
    assert_eq!(amount(&visible, "Liabilities and equity"), Decimal::ZERO);
}

// These are genuine predecessor XML/manifest files from the provenance pins, exercised with
// the current binary. This is a data-upgrade fixture, not a previous-binary/schema claim.
fn copy_artifact(name: &str, source: &std::path::Path, destination: &std::path::Path) {
    let snapshot = erp_modules::artifact_identity::ArtifactSnapshot::capture(name, source).unwrap();
    for (file, bytes) in snapshot.files() {
        let path = destination.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
fn predecessor(
    db: &TestDb,
    version: &str,
) -> (
    tempfile::TempDir,
    erp_modules::system::ModuleSystem,
    std::collections::BTreeMap<ModuleName, String>,
) {
    use std::path::Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../addons")
        .canonicalize()
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    for item in std::fs::read_dir(&root).unwrap() {
        let item = item.unwrap();
        if item.file_name() != "account_reporting" && item.path().join("manifest.toml").is_file() {
            copy_artifact(
                item.file_name().to_str().unwrap(),
                &item.path(),
                &temporary.path().join(item.file_name()),
            );
        }
    }
    let origin = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/account_reporting_previous")
        .join(version)
        .join("account_reporting");
    copy_artifact(
        "account_reporting",
        &origin,
        &temporary.path().join("account_reporting"),
    );
    let mut index = erp_modules::AddonIndex::new(true);
    index.add_dev_path(temporary.path().canonicalize().unwrap());
    let identities: std::collections::BTreeMap<_, _> = index
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
    let mut code = vec![
        &erp_addon_base::ADDON,
        &erp_addon_web::ADDON,
        &erp_addon_uom::ADDON,
        &erp_addon_product::ADDON,
        &erp_addon_analytic::ADDON,
        &erp_addon_account::ADDON,
        &erp_addon_reporting::ADDON,
    ];
    // 1.4 was data-only and did not declare the native descriptor's explicit base dependency.
    if version == "1.5.0" {
        code.push(&erp_addon_account_reporting::ADDON);
    }
    let system = erp_modules::system::ModuleSystem::builder(db.pool().clone(), index)
        .view_types(erp_addon_web::view_type_contributions())
        .native_artifact_identities(identities.clone())
        .code(code)
        .require_sql_objects([&erp_addon_account_reporting::schema::MANIFEST])
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .require_core_tables(erp_security::auth::core_tables())
        .build()
        .unwrap();
    (temporary, system, identities)
}
#[test]
fn balance_sheet_provider_upgrade_preserves_14_and_15_xml_customizations() {
    big_stack(|| async {
        upgrade("1.4.0").await;
        upgrade("1.5.0").await;
    });
}
#[test]
fn partner_ledger_upgrade_from_16_adds_definition_and_preserves_settings() {
    big_stack(|| upgrade("1.6.0"));
}

async fn upgrade(version: &str) {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let (_sources, old_system, previous) = predecessor(&db, version);
    let initial = old_system
        .initialize(InitOptions::new().with_skip_auto_install(true))
        .await
        .unwrap();
    let installed = old_system
        .button_immediate_install(
            &initial.orm.env(SUPERUSER_ID, Context::new()),
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
    posted_entry(&mut tx, &b, b.bank, b.income, None, "40", d(2024, 2, 1)).await;
    posted_entry(&mut tx, &b, b.income, b.bank, None, "10", d(2024, 2, 2)).await;
    let report = xml_id(&mut tx, "account_reporting", "report_balance_sheet").await;
    let line = xml_id(&mut tx, "account_reporting", "position_asset").await;
    let expression = xml_id(&mut tx, "account_reporting", "position_asset_booked").await;
    let column = xml_id(&mut tx, "account_reporting", "position_column").await;
    let group = xml_id(&mut tx, "base", "group_system").await;
    let admin = erp_sdk::UserId::new(user(&mut tx, "upgrade.guard.admin", company, &[group]).await)
        .unwrap();
    let group = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader =
        erp_sdk::UserId::new(user(&mut tx, "upgrade.guard.reader", company, &[group]).await)
            .unwrap();
    tx.install_audit_schema().await.unwrap();
    tx.commit().await.unwrap();
    let orm = policy_orm(&db, orm).await;
    let mut tx = orm.begin(admin).await.unwrap();
    for (model, id, name) in [
        ("reporting.report", report, "Configured statement"),
        ("reporting.report.line", line, "Configured assets"),
        ("reporting.report.column", column, "Configured amount"),
    ] {
        tx.env()
            .records(model)
            .unwrap()
            .browse([id])
            .unwrap()
            .write(&mut tx, RawVals::new().set_cell("name", text(name)))
            .await
            .unwrap();
    }
    tx.env()
        .records("reporting.report.expression")
        .unwrap()
        .browse([expression])
        .unwrap()
        .write(&mut tx, RawVals::new().set_cell("field", text("debit")))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = orm.begin(reader).await.unwrap();
    let options =
        json!({"report":report,"companies":[company],"date":{"preset":"custom","to":"2024-02-29"}});
    let before = runtime::report_lines(&tx.env(), &mut tx, &options)
        .await
        .unwrap();
    assert_eq!(amount(&before, "Configured assets"), num("40"));
    assert_eq!(amount(&before, "Liabilities and equity"), num("30"));
    tx.commit().await.unwrap();
    let target = system(&db, Some(previous));
    let upgraded = target
        .button_immediate_upgrade(
            &target.system_env(Context::new()),
            &[ModuleName::new("account_reporting").unwrap()],
        )
        .await
        .unwrap();
    assert!(upgraded.post_commit.is_clean());
    let orm = policy_orm(&db, &upgraded.orm).await;
    let mut tx = orm.begin(reader).await.unwrap();
    let partner_report = xml_id(&mut tx, "account_reporting", "report_partner_ledger").await;
    let partner_action = xml_id(&mut tx, "account_reporting", "action_partner_ledger").await;
    assert!(partner_action > 0);
    let ledger_options = json!({"report":partner_report,"companies":[company],
            "date":{"preset":"custom","from":"2024-02-01","to":"2024-02-29"}});
    let ledger = runtime::report_lines(&tx.env(), &mut tx, &ledger_options)
        .await
        .unwrap();
    assert_eq!(ledger["lines"].as_array().unwrap().len(), 2);

    let after = runtime::report_lines(&tx.env(), &mut tx, &options)
        .await
        .unwrap();
    assert_eq!(amount(&after, "Configured assets"), num("40"));
    assert_eq!(amount(&after, "Liabilities and equity"), num("30"));
    assert_eq!(after["options"]["columns"][0]["name"], "Configured amount");
    let report_row = tx
        .env()
        .records("reporting.report")
        .unwrap()
        .browse([report])
        .unwrap()
        .read_with(&mut tx, &["name", "provider"], erp_sdk::Load::Raw)
        .await
        .unwrap();
    assert_eq!(report_row[0]["name"], Value::str("Configured statement"));
    assert_eq!(
        report_row[0]["provider"],
        Value::str("account_reporting.balance_sheet")
    );
    assert_eq!(
        xml_id(&mut tx, "account_reporting", "report_balance_sheet").await,
        report
    );
    assert_eq!(
        xml_id(&mut tx, "account_reporting", "position_asset").await,
        line
    );
    assert_eq!(
        xml_id(&mut tx, "account_reporting", "position_asset_booked").await,
        expression
    );
    assert_eq!(
        xml_id(&mut tx, "account_reporting", "position_column").await,
        column
    );
}
