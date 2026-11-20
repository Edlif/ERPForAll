// SPDX-License-Identifier: MIT OR Apache-2.0
//! A full install of `account` and small books (accounts, journals, partners, posted invoices and
//! entries), shared by the reconciliation and payment tests.
#![allow(
    dead_code,
    reason = "shared fixture helpers used by different test binaries"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use erp_addon_account::fiscal::calendar::FiscalYearEnd;
use erp_addon_account::fiscal::ensure_periods_with;
use erp_addon_account::fiscal::settings::FiscalCalendar;
use erp_modules::system::{InitOptions, ModuleSystem};
use erp_modules::{AddonIndex, ModuleName};
use erp_sdk::prelude::*;
use erp_sdk::{BeginOptions, Command, Context, Orm, SUPERUSER_ID};
use erp_testkit::TestDb;

/// Run a test future on a thread with a 16 MiB stack (the install overflows the default one).
pub fn big_stack<F: std::future::Future<Output = ()> + 'static>(test: fn() -> F) {
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

/// The artifact identity of every addon folder.
pub fn identities() -> std::collections::BTreeMap<ModuleName, String> {
    let mut index = AddonIndex::new(true);
    index.add_dev_path(addons_dir());
    index
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
        .collect()
}

/// The module system of `account` and its dependencies on `db`; `previous` are the identities of
/// the release an upgrade starts from.
pub fn system(
    db: &TestDb,
    previous: Option<std::collections::BTreeMap<ModuleName, String>>,
) -> ModuleSystem {
    let mut index = AddonIndex::new(true);
    index.add_dev_path(addons_dir());
    let mut builder = ModuleSystem::builder(db.pool().clone(), index)
        .view_types(erp_addon_web::view_type_contributions())
        .native_artifact_identities(identities())
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_uom::ADDON,
            &erp_addon_product::ADDON,
            &erp_addon_analytic::ADDON,
            &erp_addon_account::ADDON,
            &erp_addon_reporting::ADDON,
            &erp_addon_account_reporting::ADDON,
        ])
        .require_sql_objects([&erp_addon_account_reporting::schema::MANIFEST])
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .require_core_tables(erp_security::auth::core_tables());
    if let Some(previous) = previous {
        builder = builder.identity_previous(previous);
    }
    builder.build().unwrap()
}

/// A fresh database with `account` installed.
pub async fn install() -> (TestDb, Orm) {
    install_with_demo(false).await
}

/// A fresh database with `account` installed, with or without demo data.
pub async fn install_with_demo(demo: bool) -> (TestDb, Orm) {
    // room for two units at once next to the connections the access policy keeps
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let system = system(&db, None);
    let init = Box::pin(
        system.initialize(
            InitOptions::new()
                .with_skip_auto_install(true)
                .with_demo(demo),
        ),
    )
    .await
    .unwrap();
    let env = init.orm.env(SUPERUSER_ID, Context::new());
    let outcome =
        Box::pin(system.button_immediate_install(&env, &[ModuleName::new("account").unwrap()]))
            .await
            .unwrap();
    (db, outcome.orm)
}

/// An ORM on the same database that asks the installed access policy, so a test can act as a
/// user who is not the superuser.
pub async fn policy_orm(db: &TestDb, installed: &Orm) -> Orm {
    let source = erp_security::source::PolicySource::build(
        Arc::clone(installed.registry()),
        db.pool().clone(),
    )
    .await
    .unwrap();
    Orm::builder(Arc::clone(installed.registry()), db.pool().clone())
        .generation_attachment(erp_orm::GenerationAttachment::new(
            Arc::clone(installed.registry()),
            installed
                .env(SUPERUSER_ID, Context::new())
                .generation_attachment::<erp_views::PublishedViewTypes>()
                .unwrap(),
        ))
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .resolver(erp_security::resolver::Resolver::for_registry(installed.registry()).unwrap())
        .policy(Arc::new(source) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap()
}

pub fn d(y: i32, m: u32, day: u32) -> Date {
    Date::from_ymd(y, m, day).unwrap()
}

pub fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

pub fn num(value: &str) -> Decimal {
    value.parse().unwrap()
}

pub fn rec(id: i64) -> RecId {
    RecId::from_real(id).unwrap()
}

pub async fn xml_id(tx: &mut Transaction, module: &str, name: &str) -> i64 {
    tx.query(
        "SELECT res_id::int8 FROM ir_model_data WHERE module = $1 AND name = $2",
        &[&module, &name],
    )
    .await
    .unwrap()[0]
        .get(0)
}

pub async fn main_company(tx: &mut Transaction) -> i64 {
    xml_id(tx, "base", "main_company").await
}

/// Another root company with the main company's currency.
pub async fn other_company(tx: &mut Transaction, main: i64) -> i64 {
    let currency: i64 = tx
        .query(
            "SELECT currency_id::int8 FROM res_company WHERE id = $1",
            &[&main],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.env()
        .records("res.company")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Other company"))
                    .set_cell("currency_id", Cell::Id(currency)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// Monthly periods of 2024 for `company`, committed.
pub async fn periods(orm: &Orm, company: i64) {
    ensure_periods_with(
        orm,
        BeginOptions::new(SUPERUSER_ID),
        &FiscalCalendar::new(FiscalYearEnd::new(31, 12).unwrap(), 12).unwrap(),
        company,
        d(2024, 1, 1),
        d(2024, 12, 31),
    )
    .await
    .unwrap();
}

/// A journal with monthly numbers of four digits, written directly so its numbering is fixed.
pub async fn journal(
    tx: &mut Transaction,
    company: i64,
    code: &str,
    kind: &str,
    default_account: Option<i64>,
) -> i64 {
    tx.query(
        "INSERT INTO account_journal (name, code, active, type, company_id, sequence, \
         refund_sequence, payment_sequence, sequence_reset, sequence_padding, color, \
         show_on_dashboard, default_account_id) \
         VALUES (jsonb_build_object('en_US', $2::text), $2::text, true, $3::text, $1, 10, \
         false, false, 'month', 4, 0, true, $4) RETURNING id::int8",
        &[&company, &code, &kind, &default_account],
    )
    .await
    .unwrap()[0]
        .get(0)
}

pub async fn account(tx: &mut Transaction, company: i64, code: &str, kind: &str) -> i64 {
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

pub async fn partner(tx: &mut Transaction, name: &str) -> i64 {
    tx.env()
        .records("res.partner")
        .unwrap()
        .create(tx, vec![RawVals::new().set_cell("name", text(name))])
        .await
        .unwrap()
        .ids()[0]
}

/// The accounts and journals of one company.
pub struct Books {
    pub company: i64,
    pub receivable: i64,
    pub payable: i64,
    pub income: i64,
    pub expense: i64,
    pub bank: i64,
    pub other_receivable: i64,
    pub customer: i64,
    pub vendor: i64,
}

/// Accounts, a sale, purchase and miscellaneous journal, a customer and a vendor of `company`;
/// the periods must exist (see [`periods`]).
pub async fn books(tx: &mut Transaction, company: i64) -> Books {
    let receivable = account(tx, company, "121000", "asset_receivable").await;
    let payable = account(tx, company, "211000", "liability_payable").await;
    let income = account(tx, company, "400000", "income").await;
    let expense = account(tx, company, "600000", "expense").await;
    let bank = account(tx, company, "101401", "asset_current").await;
    let other_receivable = account(tx, company, "121100", "asset_current").await;
    journal(tx, company, "INV", "sale", Some(income)).await;
    journal(tx, company, "BILL", "purchase", Some(expense)).await;
    journal(tx, company, "MISC", "general", None).await;
    Books {
        company,
        receivable,
        payable,
        income,
        expense,
        bank,
        other_receivable,
        customer: partner(tx, &format!("Customer {company}")).await,
        vendor: partner(tx, &format!("Vendor {company}")).await,
    }
}

pub fn moves(tx: &Transaction, company: i64) -> Records {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move")
        .unwrap()
}

/// A posted invoice (`out_invoice`, `in_invoice`, a credit note) of one untaxed line of `amount`.
pub async fn posted_invoice(
    tx: &mut Transaction,
    b: &Books,
    move_type: &str,
    partner: i64,
    amount: &str,
    date: Date,
) -> Records {
    let line = Command::create(
        RawVals::new()
            .set_cell("name", text("Service"))
            .set_cell("quantity", Cell::Decimal(num("1")))
            .set_cell("price_unit", Cell::Decimal(num(amount))),
    );
    let created = moves(tx, b.company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("move_type", text(move_type))
                    .set_cell("partner_id", Cell::Id(partner))
                    .set_cell("invoice_date", Cell::Date(date))
                    .set_cell("date", Cell::Date(date))
                    .set_commands("invoice_line_ids", vec![line]),
            ],
        )
        .await
        .unwrap();
    created.call::<(), ()>(tx, "action_post", ()).await.unwrap();
    created
}

/// A posted entry of two lines: `amount` (positive: a debit) on `account` for `partner`, the
/// opposite on `against`.
pub async fn posted_entry(
    tx: &mut Transaction,
    b: &Books,
    account: i64,
    against: i64,
    partner: Option<i64>,
    amount: &str,
    date: Date,
) -> Records {
    let amount = num(amount);
    let mut first = RawVals::new()
        .set_cell("account_id", Cell::Id(account))
        .set_cell("balance", Cell::Decimal(amount));
    if let Some(partner) = partner {
        first.insert("partner_id".into(), Cell::Id(partner));
    }
    let second = RawVals::new()
        .set_cell("account_id", Cell::Id(against))
        .set_cell("balance", Cell::Decimal(-amount));
    let created = moves(tx, b.company)
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(date))
                    .set_commands(
                        "line_ids",
                        vec![Command::create(first), Command::create(second)],
                    ),
            ],
        )
        .await
        .unwrap();
    created.call::<(), ()>(tx, "action_post", ()).await.unwrap();
    created
}

/// The items of `entries` on `account`.
pub async fn items_on(tx: &mut Transaction, entries: &[&Records], account: i64) -> Records {
    let ids: Vec<i64> = entries.iter().flat_map(|e| e.ids()).collect();
    let found: Vec<i64> = tx
        .query(
            "SELECT id::int8 FROM account_move_line WHERE move_id = ANY($1) AND account_id = $2 \
             ORDER BY move_id, id",
            &[&ids, &account],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    tx.env()
        .records("account.move.line")
        .unwrap()
        .browse(found)
        .unwrap()
}

pub async fn decimal(record: &Records, tx: &mut Transaction, field: &str) -> Decimal {
    match record.cell(tx, field).await.unwrap() {
        Cell::Decimal(value) => value,
        Cell::Null | Cell::Bool(false) => Decimal::ZERO,
        other => panic!("{field} is {other:?}"),
    }
}

pub async fn string(record: &Records, tx: &mut Transaction, field: &str) -> String {
    match record.cell(tx, field).await.unwrap() {
        Cell::Str(value) => value.to_string(),
        Cell::Null | Cell::Bool(false) => String::new(),
        other => panic!("{field} is {other:?}"),
    }
}

/// An internal user of `company` only, with `groups`.
pub async fn user(tx: &mut Transaction, login: &str, company: i64, groups: &[i64]) -> i64 {
    let internal = xml_id(tx, "base", "group_user").await;
    let vals = RawVals::new()
        .set_cell("name", text(login))
        .set_cell("login", text(login))
        .set_cell("password", text("accounting-fixture-password"))
        .set_cell("company_id", Cell::Id(company))
        .set_commands("company_ids", vec![Command::set([rec(company)])])
        .set_commands(
            "group_ids",
            vec![Command::set(
                groups.iter().chain([&internal]).map(|id| rec(*id)),
            )],
        );
    tx.env()
        .records("res.users")
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

/// Calls `method` without arguments in a savepoint, so a refusal leaves the transaction usable.
pub async fn try_call(records: &Records, tx: &mut Transaction, method: &'static str) -> Result<()> {
    let records = records.clone();
    tx.savepoint(async move |tx| records.call::<(), ()>(tx, method, ()).await)
        .await
}
