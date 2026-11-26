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
    assert_eq!(amount(&february, "Liabilities and Equity"), num("7"));
    // the bank account of these books is a current asset, grouped by account since 1.13
    let asset_id = format!(
        "root~reporting.report~{}|line:ASSET~~|line:CURRENT_ASSETS~~|line:OTHER_CURRENT_ASSETS~~",
        f.report
    );
    let accounts =
        runtime::report_expand(&tx.env(), &mut tx, &input(&f, "2024-02-29"), &asset_id, 0)
            .await
            .unwrap();
    // the account group is the last level of that line (grouped by account only)
    let leaf = accounts["lines"][0]["id"].as_str().unwrap();
    let mut stale = input(&f, "2024-02-29");
    stale["unfolded"] = json!([asset_id, leaf]);
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
    assert_eq!(amount(&visible, "Liabilities and Equity"), Decimal::ZERO);
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
    // Releases before 1.13 predate the native descriptor's dependencies (analytic).
    if version == "1.13.0" {
        code.push(&erp_addon_account_reporting::ADDON);
    }
    let system = erp_modules::system::ModuleSystem::builder(db.pool().clone(), index)
        .view_types(erp_addon_web::view_type_contributions())
        .native_artifact_identities(identities.clone())
        .code(code)
        .require_sql_objects([
            &erp_addon_account::reconcile::history_guard::MANIFEST,
            &erp_addon_account_reporting::schema::MANIFEST,
        ])
        .require_sql_uninstall_guards([&erp_addon_account::reconcile::history_guard::UNINSTALL])
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
    let existing_gl_menu = if version == "1.6.0" {
        Some(xml_id(&mut tx, "account_reporting", "menu_general_ledger").await)
    } else {
        None
    };
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
    // Since 1.13 the ledger lists partners; no posted item of this test names one.
    assert_eq!(ledger["lines"].as_array().unwrap().len(), 0);

    if let Some(menu_id) = existing_gl_menu {
        assert_eq!(
            xml_id(&mut tx, "account_reporting", "menu_general_ledger").await,
            menu_id
        );
        let menu = tx
            .query("SELECT web_icon FROM ir_ui_menu WHERE id = $1", &[&menu_id])
            .await
            .unwrap();
        assert_eq!(
            menu[0].get::<_, Option<String>>(0).as_deref(),
            Some("lucide:book-open")
        );
    }
    let after = runtime::report_lines(&tx.env(), &mut tx, &options)
        .await
        .unwrap();
    assert_eq!(amount(&after, "Configured assets"), num("40"));
    // 1.13 renamed the shipped line; the changed asset expression keeps its own figure.
    assert_eq!(amount(&after, "Liabilities and Equity"), num("30"));
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

#[test]
fn installed_reporting_1_0_upgrades_to_movements_idempotently() {
    big_stack(upgrade_account_movements);
}

async fn upgrade_account_movements() {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let (_sources, old_system, previous) = predecessor(&db, "1.0.0");
    let module = ModuleName::new("account_reporting").unwrap();
    let modules = std::slice::from_ref(&module);
    let initial = Box::pin(old_system.initialize(InitOptions::new().with_skip_auto_install(true)))
        .await
        .unwrap();
    let installed = Box::pin(
        old_system
            .button_immediate_install(&initial.orm.env(SUPERUSER_ID, Context::new()), modules),
    )
    .await
    .unwrap();
    assert!(
        installed.post_commit.is_clean(),
        "{:?}",
        installed.post_commit
    );
    let previous_ids = reporting_ids(&installed.orm).await;
    assert!(!previous_ids.contains_key("report_account_movements"));
    assert!(!previous_ids.contains_key("action_account_movements"));
    assert!(!previous_ids.contains_key("menu_account_movements"));
    let old_report = previous_ids["report_posted_journal_activity"].1;
    let old_receipt = reporting_receipt(&installed.orm).await;
    assert_eq!(old_receipt, ("1.0.0".to_owned(), previous[&module].clone()));
    let mut tx = installed.orm.begin(SUPERUSER_ID).await.unwrap();
    tx.env()
        .records("reporting.report")
        .unwrap()
        .browse([old_report])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("name", text("Configured journal activity")),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let current = system(&db, Some(previous.clone()));
    let upgraded =
        Box::pin(current.button_immediate_upgrade(&current.system_env(Context::new()), modules))
            .await
            .unwrap();
    assert!(
        upgraded.post_commit.is_clean(),
        "{:?}",
        upgraded.post_commit
    );
    let upgraded_ids = reporting_ids(&upgraded.orm).await;
    for (name, identity) in &previous_ids {
        assert_eq!(
            upgraded_ids.get(name),
            Some(identity),
            "previous identity {name}"
        );
    }
    assert_movement_definition(&upgraded.orm, &upgraded_ids).await;

    // The old release receipt remains until the explicit activation step.
    let manifest = erp_modules::Manifest::from_toml(
        module.as_str(),
        include_str!("../../../addons/account_reporting/manifest.toml"),
    )
    .unwrap();
    let target_version = manifest.version().raw();
    assert_eq!(
        reporting_receipt(&upgraded.orm).await,
        (target_version.to_owned(), previous[&module].clone())
    );
    current
        .identity_activate(
            &previous,
            "upgrade-test",
            "Activate the Account Movements data upgrade",
        )
        .await
        .unwrap();
    let current_identity = identities()[&module].clone();
    assert_eq!(
        reporting_receipt(&upgraded.orm).await,
        (target_version.to_owned(), current_identity.clone())
    );

    let current = system(&db, None);
    let repeated =
        Box::pin(current.button_immediate_upgrade(&current.system_env(Context::new()), modules))
            .await
            .unwrap();
    assert!(
        repeated.post_commit.is_clean(),
        "{:?}",
        repeated.post_commit
    );
    assert_eq!(reporting_ids(&repeated.orm).await, upgraded_ids);
    assert_eq!(
        reporting_receipt(&repeated.orm).await,
        (target_version.to_owned(), current_identity)
    );
    assert_movement_definition(&repeated.orm, &upgraded_ids).await;
}

type ReportingIds = std::collections::BTreeMap<String, (String, i64)>;

async fn reporting_ids(orm: &erp_sdk::Orm) -> ReportingIds {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.query(
        "SELECT name, model, res_id::int8 FROM ir_model_data WHERE module='account_reporting'",
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), (row.get(1), row.get(2))))
    .collect()
}

async fn reporting_receipt(orm: &erp_sdk::Orm) -> (String, String) {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let rows = tx
        .query(
            "SELECT m.latest_version, i.content_hash FROM ir_module_module m
         JOIN installed_addon_identity i ON i.name = m.name WHERE m.name = 'account_reporting'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    (rows[0].get(0), rows[0].get(1))
}

async fn assert_movement_definition(orm: &erp_sdk::Orm, ids: &ReportingIds) {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let env = tx.env();
    let report = ids["report_account_movements"].1;
    let action_id = ids["action_account_movements"].1;
    let action = env
        .records("ir.actions.client")
        .unwrap()
        .browse([action_id])
        .unwrap();
    assert_eq!(
        action.cell(&mut tx, "tag").await.unwrap(),
        text("reporting.report")
    );
    assert_eq!(
        action.cell(&mut tx, "params").await.unwrap(),
        Cell::Json(std::sync::Arc::new(json!({"report": report})))
    );
    let menu = env
        .records("ir.ui.menu")
        .unwrap()
        .browse([ids["menu_account_movements"].1])
        .unwrap();
    let readonly = xml_id(&mut tx, "account", "group_account_readonly").await;
    assert_eq!(
        menu.cell(&mut tx, "parent_id").await.unwrap(),
        Cell::Id(ids["menu_account_reporting"].1)
    );
    assert_eq!(
        menu.cell(&mut tx, "action").await.unwrap(),
        text(&format!("ir.actions.client,{action_id}"))
    );
    assert_eq!(
        menu.related(&mut tx, "group_ids").await.unwrap().ids(),
        [readonly]
    );

    let opened = runtime::report_options(&env, &mut tx, report, &Json::Null)
        .await
        .unwrap();
    assert_eq!(opened["name"], "Account Movements");
    let columns = opened["options"]["columns"].as_array().unwrap();
    assert_eq!(
        columns
            .iter()
            .map(|column| column["label"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["debit", "credit", "net"]
    );
    let report = env
        .records("reporting.report")
        .unwrap()
        .browse([report])
        .unwrap();
    let line = report.related(&mut tx, "line_ids").await.unwrap();
    assert_eq!(line.ids(), [ids["movements_by_account"].1]);
    assert_eq!(
        line.cell(&mut tx, "groupby").await.unwrap(),
        text("account_id,move_id")
    );
    let mut expressions = line.related(&mut tx, "expression_ids").await.unwrap().ids();
    expressions.sort_unstable();
    let mut expected =
        ["debit", "credit", "net"].map(|label| ids[&format!("movements_expression_{label}")].1);
    expected.sort_unstable();
    assert_eq!(expressions, expected);

    let previous = env
        .records("reporting.report")
        .unwrap()
        .browse([ids["report_posted_journal_activity"].1])
        .unwrap();
    assert_eq!(
        previous.cell(&mut tx, "name").await.unwrap(),
        text("Configured journal activity")
    );
}

// Upgrades of the report definitions from 1.10 (before the classified layouts) and from 1.13 (the
// first release with them) to the current version.

type Previous = std::collections::BTreeMap<ModuleName, String>;

async fn install_previous(version: &str) -> (TestDb, erp_sdk::Orm, Previous) {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let (_sources, old_system, previous) = predecessor(&db, version);
    let initial = Box::pin(old_system.initialize(InitOptions::new().with_skip_auto_install(true)))
        .await
        .unwrap();
    let installed = Box::pin(old_system.button_immediate_install(
        &initial.orm.env(SUPERUSER_ID, Context::new()),
        &[ModuleName::new("account_reporting").unwrap()],
    ))
    .await
    .unwrap();
    assert!(installed.post_commit.is_clean());
    (db, installed.orm, previous)
}

async fn upgrade_current(
    db: &TestDb,
    previous: Previous,
) -> std::result::Result<erp_sdk::Orm, erp_modules::Error> {
    let target = system(db, Some(previous));
    let upgraded = Box::pin(target.button_immediate_upgrade(
        &target.system_env(Context::new()),
        &[ModuleName::new("account_reporting").unwrap()],
    ))
    .await?;
    assert!(upgraded.post_commit.is_clean());
    Ok(upgraded.orm)
}

/// Every report record of this addon by external id: its values (ids replaced by the external ids
/// they point at), `DANGLING` for an id without a record, and the report records without an id.
async fn definitions(orm: &erp_sdk::Orm) -> std::collections::BTreeMap<String, String> {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let mut out = std::collections::BTreeMap::new();
    let xid = |fk: &str, model: &str| {
        format!(
            "COALESCE((SELECT string_agg(x.name, ',' ORDER BY x.name) FROM ir_model_data x \
             WHERE x.module = 'account_reporting' AND x.model = '{model}' AND x.res_id = r.{fk}), '')"
        )
    };
    for (model, table, refs) in [
        ("reporting.report", "reporting_report", "''".to_owned()),
        (
            "reporting.report.line",
            "reporting_report_line",
            format!(
                "{} || ' ' || {}",
                xid("report_id", "reporting.report"),
                xid("parent_id", "reporting.report.line")
            ),
        ),
        (
            "reporting.report.column",
            "reporting_report_column",
            xid("report_id", "reporting.report"),
        ),
        (
            "reporting.report.expression",
            "reporting_report_expression",
            xid("line_id", "reporting.report.line"),
        ),
    ] {
        let sql = format!(
            "SELECT d.model || ' ' || d.name, CASE WHEN r.id IS NULL THEN 'DANGLING' ELSE \
             (to_jsonb(r) - '{{id,create_date,write_date,create_uid,write_uid,report_id,parent_id,line_id}}'::text[])::text \
             || ' ' || {refs} END \
             FROM ir_model_data d LEFT JOIN {table} r ON r.id = d.res_id \
             WHERE d.module = 'account_reporting' AND d.model = $1"
        );
        for row in tx.query(&sql, &[&model]).await.unwrap() {
            out.insert(row.get(0), row.get(1));
        }
    }
    for (what, sql) in [
        (
            "lines without an external id",
            "SELECT count(*) FROM reporting_report_line r WHERE NOT EXISTS (SELECT 1 FROM ir_model_data x \
             WHERE x.model = 'reporting.report.line' AND x.res_id = r.id)",
        ),
        (
            "columns without an external id",
            "SELECT count(*) FROM reporting_report_column r WHERE NOT EXISTS (SELECT 1 FROM ir_model_data x \
             WHERE x.model = 'reporting.report.column' AND x.res_id = r.id)",
        ),
        (
            "expressions without an external id",
            "SELECT count(*) FROM reporting_report_expression r WHERE NOT EXISTS (SELECT 1 FROM ir_model_data x \
             WHERE x.model = 'reporting.report.expression' AND x.res_id = r.id)",
        ),
    ] {
        let count: i64 = tx.query(sql, &[]).await.unwrap()[0].get(0);
        out.insert(what.to_owned(), count.to_string());
    }
    out
}

fn assert_same_definitions(
    fresh: &std::collections::BTreeMap<String, String>,
    upgraded: &std::collections::BTreeMap<String, String>,
    from: &str,
) {
    assert!(
        !fresh.values().any(|v| v == "DANGLING"),
        "fresh install has dangling ids"
    );
    for key in fresh.keys().chain(upgraded.keys()) {
        assert_eq!(
            fresh.get(key),
            upgraded.get(key),
            "{key}: fresh install vs upgrade from {from}"
        );
    }
}

#[test]
fn report_definitions_upgraded_from_1_10_and_1_13_equal_a_fresh_install() {
    big_stack(|| async {
        let fresh = definitions(&setup().await.orm).await;
        for version in ["1.10.0", "1.13.0"] {
            let (db, _, previous) = install_previous(version).await;
            let orm = upgrade_current(&db, previous).await.unwrap();
            assert_same_definitions(&fresh, &definitions(&orm).await, version);
        }
    });
}

async fn write(tx: &mut Transaction, model: &str, id: i64, vals: RawVals) {
    tx.env()
        .records(model)
        .unwrap()
        .browse([id])
        .unwrap()
        .write(tx, vals)
        .await
        .unwrap();
}

async fn cell_of(tx: &mut Transaction, model: &str, id: i64, field: &str) -> Cell {
    tx.env()
        .records(model)
        .unwrap()
        .browse([id])
        .unwrap()
        .cell(tx, field)
        .await
        .unwrap()
}

#[test]
fn report_upgrade_from_1_10_keeps_administrator_changes() {
    big_stack(|| async {
        let (db, orm, previous) = install_previous("1.10.0").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let expense = xml_id(&mut tx, "account_reporting", "profit_loss_expense_booked").await;
        write(
            &mut tx,
            "reporting.report.expression",
            expense,
            RawVals::new().set_cell("field", text("debit")),
        )
        .await;
        let date_column = xml_id(&mut tx, "account_reporting", "gl_column_date").await;
        write(
            &mut tx,
            "reporting.report.column",
            date_column,
            RawVals::new().set_cell("name", text("Booked on")),
        )
        .await;
        let ledger = xml_id(&mut tx, "account_reporting", "report_general_ledger").await;
        write(
            &mut tx,
            "reporting.report",
            ledger,
            RawVals::new().set_cell("date_default", text("last_year")),
        )
        .await;
        let partner_ledger = xml_id(&mut tx, "account_reporting", "report_partner_ledger").await;
        let added = tx
            .env()
            .records("reporting.report.line")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("report_id", Cell::Id(partner_ledger))
                        .set_cell("name", text("Disputed"))
                        .set_cell("code", text("DISPUTED")),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.commit().await.unwrap();

        let orm = upgrade_current(&db, previous).await.unwrap();
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        // The changed expression stays and stands in for the new total of its line.
        assert_eq!(
            cell_of(&mut tx, "reporting.report.expression", expense, "field").await,
            text("debit")
        );
        assert_eq!(
            xml_id(&mut tx, "account_reporting", "profit_loss_expense_total").await,
            expense
        );
        assert_eq!(
            cell_of(&mut tx, "reporting.report.column", date_column, "name").await,
            text("Booked on")
        );
        assert_eq!(
            cell_of(&mut tx, "reporting.report", ledger, "date_default").await,
            text("last_year")
        );
        // the flag the administrator did not change still moves to the new default
        assert_eq!(
            cell_of(&mut tx, "reporting.report", ledger, "search_bar").await,
            Cell::Bool(true)
        );
        assert_eq!(
            cell_of(&mut tx, "reporting.report.line", added, "code").await,
            text("DISPUTED")
        );
        let shipped = tx
            .query(
                "SELECT count(*) FROM ir_model_data WHERE module = 'account_reporting' \
                 AND name IN ('partner_ledger_receivable', 'partner_ledger_payable')",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(shipped[0].get::<_, i64>(0), 0);
        let profit_loss = xml_id(&mut tx, "account_reporting", "report_profit_loss").await;
        runtime::report_options(&tx.env(), &mut tx, profit_loss, &Json::Null)
            .await
            .unwrap();
    });
}

#[test]
fn report_upgrade_from_1_10_stops_on_a_changed_equity_formula() {
    big_stack(|| async {
        let (db, orm, previous) = install_previous("1.10.0").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let equity = xml_id(&mut tx, "account_reporting", "position_equity_display").await;
        write(
            &mut tx,
            "reporting.report.expression",
            equity,
            RawVals::new().set_cell("formula", text("CAPITAL.amount")),
        )
        .await;
        tx.commit().await.unwrap();
        let Err(error) = upgrade_current(&db, previous).await else {
            panic!("the upgrade must stop on the changed Equity formula");
        };
        let message = format!("{error:?}");
        assert!(
            message.contains("account_reporting.position_equity_display")
                && message.contains("'CAPITAL.amount'")
                && message.contains("0 rows changed, 1 expected"),
            "{message}"
        );
    });
}

#[test]
fn report_upgrade_from_1_13_keeps_a_changed_invoice_expression_and_aged_flag() {
    big_stack(|| async {
        let (db, orm, previous) = install_previous("1.13.0").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let total = xml_id(&mut tx, "account_reporting", "inv_an_out_inv_tot").await;
        write(
            &mut tx,
            "reporting.report.expression",
            total,
            RawVals::new().set_cell("field", text("amount_untaxed")),
        )
        .await;
        let receivable = xml_id(&mut tx, "account_reporting", "report_aged_receivable").await;
        write(
            &mut tx,
            "reporting.report",
            receivable,
            RawVals::new().set_cell("comparison", Cell::Bool(true)),
        )
        .await;
        tx.commit().await.unwrap();
        let orm = upgrade_current(&db, previous).await.unwrap();
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        assert_eq!(
            cell_of(&mut tx, "reporting.report.expression", total, "field").await,
            text("amount_untaxed")
        );
        let untaxed = xml_id(&mut tx, "account_reporting", "inv_an_out_inv_u").await;
        assert_eq!(
            cell_of(&mut tx, "reporting.report.expression", untaxed, "engine").await,
            text("aggregate")
        );
        // the unchanged Customer Credit Notes line took the new layout
        let refunds = xml_id(&mut tx, "account_reporting", "inv_an_out_ref_u").await;
        assert_eq!(
            cell_of(&mut tx, "reporting.report.expression", refunds, "engine").await,
            text("formula")
        );
        assert_eq!(
            cell_of(&mut tx, "reporting.report", receivable, "current_only").await,
            Cell::Bool(false)
        );
        let payable = xml_id(&mut tx, "account_reporting", "report_aged_payable").await;
        assert_eq!(
            cell_of(&mut tx, "reporting.report", payable, "current_only").await,
            Cell::Bool(true)
        );
    });
}

async fn one(
    tx: &mut Transaction,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> i64 {
    tx.query(sql, params).await.unwrap()[0].get(0)
}

/// A 10 % tax of `use_` (`sale`, `purchase` or `none`) booked on `account`, written directly.
async fn percent_tax(
    tx: &mut Transaction,
    company: i64,
    name: &str,
    use_: &str,
    account: i64,
) -> i64 {
    let group = one(
        tx,
        "INSERT INTO account_tax_group (name, sequence, company_id) \
         VALUES (jsonb_build_object('en_US', $2::text), 10, $1) RETURNING id::int8",
        &[&company, &name],
    )
    .await;
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
         VALUES (jsonb_build_object('en_US', $4::text), jsonb_build_object('en_US', $4::text), \
         $5::text, 'percent', true, $1, $1, 1, 10, false, true, $2, $3, 0, false) RETURNING id::int8",
        &[&company, &group, &country, &name, &use_],
    )
    .await;
    tx.execute(
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

struct Document<'a> {
    move_type: &'a str,
    partner: i64,
    journal: i64,
    currency: Option<i64>,
    price: &'a str,
    taxes: &'a [i64],
}

async fn document(tx: &mut Transaction, company: i64, doc: Document<'_>) -> i64 {
    let line = erp_sdk::Command::create(
        RawVals::new()
            .set_cell("name", text("Service"))
            .set_cell("quantity", Cell::Decimal(num("1")))
            .set_cell("price_unit", Cell::Decimal(num(doc.price)))
            .set_commands(
                "tax_ids",
                [erp_sdk::Command::set(doc.taxes.iter().map(|t| rec(*t)))],
            ),
    );
    let mut vals = RawVals::new()
        .set_cell("move_type", text(doc.move_type))
        .set_cell("partner_id", Cell::Id(doc.partner))
        .set_cell("journal_id", Cell::Id(doc.journal))
        .set_cell("invoice_date", Cell::Date(d(2024, 3, 5)))
        .set_cell("date", Cell::Date(d(2024, 3, 5)))
        .set_commands("invoice_line_ids", vec![line]);
    if let Some(currency) = doc.currency {
        vals.insert("currency_id".into(), Cell::Id(currency));
    }
    let created = moves(tx, company).create(tx, vec![vals]).await.unwrap();
    created.call::<(), ()>(tx, "action_post", ()).await.unwrap();
    created.ids()[0]
}

/// The company-currency balance of the `kind` items of entry `id`, negated (credit positive).
async fn credit_of(tx: &mut Transaction, id: i64, kind: &str) -> Decimal {
    let rows = tx
        .query(
            "SELECT COALESCE(-SUM(balance), 0)::text FROM account_move_line \
             WHERE move_id = $1 AND display_type = $2",
            &[&id, &kind],
        )
        .await
        .unwrap();
    num(&rows[0].get::<_, String>(0))
}

fn cells(output: &Json, name: &str) -> Vec<Decimal> {
    let row = output["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("no line {name}: {output}"));
    row["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cell| cell["value"].as_str().map_or(Decimal::ZERO, num))
        .collect()
}

#[test]
fn invoice_analysis_and_tax_report_read_company_currency_journal_items() {
    big_stack(|| async {
        let f = setup().await;
        let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
        let sale_tax_account = account(&mut tx, f.company, "251000", "liability_current").await;
        let sales_tax = percent_tax(&mut tx, f.company, "Sales 10", "sale", sale_tax_account).await;
        let member_tax =
            percent_tax(&mut tx, f.company, "Member 10", "none", sale_tax_account).await;
        let sales = one(
            &mut tx,
            "SELECT id::int8 FROM account_journal WHERE code = 'INV' AND company_id = $1",
            &[&f.company],
        )
        .await;
        let bills = one(
            &mut tx,
            "SELECT id::int8 FROM account_journal WHERE code = 'BILL' AND company_id = $1",
            &[&f.company],
        )
        .await;
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
                        .set_cell("rounding", Cell::Decimal(num("0.01")))
                        .set_cell("active", Cell::Bool(true)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.env()
            .with_company(f.company)
            .unwrap()
            .records("res.currency.rate")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("currency_id", Cell::Id(foreign))
                        .set_cell("company_id", Cell::Id(f.company))
                        .set_cell("name", Cell::Date(d(2024, 1, 1)))
                        .set_cell("rate", Cell::Decimal(num("2"))),
                ],
            )
            .await
            .unwrap();
        let abroad = document(
            &mut tx,
            f.company,
            Document {
                move_type: "out_invoice",
                partner: f.books.customer,
                journal: sales,
                currency: Some(foreign),
                price: "1000",
                taxes: &[sales_tax],
            },
        )
        .await;
        let refund = document(
            &mut tx,
            f.company,
            Document {
                move_type: "in_refund",
                partner: f.books.vendor,
                journal: bills,
                currency: None,
                price: "300",
                taxes: &[],
            },
        )
        .await;
        tx.commit().await.unwrap();

        let reader_orm = policy_orm(&f.db, &f.orm).await;
        let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
        let analysis = xml_id(&mut tx, "account_reporting", "report_invoice_analysis").await;
        let mut read = reader_orm.begin(f.reader).await.unwrap();
        let options = |report: i64| {
            json!({"report":report,"companies":[f.company],
                   "date":{"preset":"custom","from":"2024-01-01","to":"2024-12-31"}})
        };
        let output = runtime::report_lines(&read.env(), &mut read, &options(analysis))
            .await
            .unwrap();
        read.commit().await.unwrap();
        let untaxed = credit_of(&mut tx, abroad, "product").await;
        let tax = credit_of(&mut tx, abroad, "tax").await;
        let receivable = -credit_of(&mut tx, abroad, "payment_term").await;
        let document_untaxed = num(&tx
            .query(
                "SELECT amount_untaxed::text FROM account_move WHERE id = $1",
                &[&abroad],
            )
            .await
            .unwrap()[0]
            .get::<_, String>(0));
        assert_ne!(
            untaxed, document_untaxed,
            "the invoice must be in another currency"
        );
        assert_eq!(
            cells(&output, "Customer Invoices"),
            [untaxed, tax, untaxed + tax]
        );
        assert_eq!(untaxed + tax, receivable);
        // a vendor credit note credits its expense line
        let refunded = credit_of(&mut tx, refund, "product").await;
        assert!(refunded > Decimal::ZERO);
        assert_eq!(
            cells(&output, "Vendor Credit Notes"),
            [refunded, Decimal::ZERO, refunded]
        );
        tx.commit().await.unwrap();

        // A management-ledger invoice stays out of the tax report; a group member tax is counted.
        let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
        let group = tx
            .env()
            .records("account.journal.group")
            .unwrap()
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", text("Management book"))],
            )
            .await
            .unwrap()
            .ids()[0];
        let management = journal(&mut tx, f.company, "MGT", "sale", Some(f.books.income)).await;
        write(
            &mut tx,
            "account.journal",
            management,
            RawVals::new().set_cell("journal_group_id", Cell::Id(group)),
        )
        .await;
        let managed = document(
            &mut tx,
            f.company,
            Document {
                move_type: "out_invoice",
                partner: f.books.customer,
                journal: management,
                currency: None,
                price: "500",
                taxes: &[sales_tax],
            },
        )
        .await;
        let member = document(
            &mut tx,
            f.company,
            Document {
                move_type: "out_invoice",
                partner: f.books.customer,
                journal: sales,
                currency: None,
                price: "200",
                taxes: &[member_tax],
            },
        )
        .await;
        tx.commit().await.unwrap();
        let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
        let report = xml_id(&mut tx, "account_reporting", "report_tax_report").await;
        let mut read = reader_orm.begin(f.reader).await.unwrap();
        let output = runtime::report_lines(&read.env(), &mut read, &options(report))
            .await
            .unwrap();
        read.commit().await.unwrap();
        assert!(credit_of(&mut tx, managed, "tax").await > Decimal::ZERO);
        let other = credit_of(&mut tx, member, "tax").await;
        assert!(other > Decimal::ZERO);
        assert_eq!(cells(&output, "Sales Taxes")[2], tax);
        assert_eq!(cells(&output, "Other Taxes")[2], other);
        assert_eq!(cells(&output, "Net Tax Payable")[2], tax + other);
    });
}
