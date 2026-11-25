// SPDX-License-Identifier: MIT OR Apache-2.0
//! The HR actions and RPCs on an installed `hr`: the org and department charts with their row
//! limit and depth parameters, the end-of-collaboration wizard and its reversal, badge codes, the
//! manager recursion check, the contract history company, the grouped count fields, the officer-only
//! fields and the default time zone.
#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]

use std::{path::Path, sync::Arc};

use erp_fields::{Cell, Date, ErrorKind, RecId, Value};
use erp_modules::{AddonIndex, InitOptions, ModuleName, ModuleSystem};
use erp_orm::{Command, Context, Orm, RawVals, Records, SUPERUSER_ID, Transaction};
use erp_security::{resolver::Resolver, source::PolicySource};
use erp_testkit::{TemplateDb, TestDb};

const STACK_SIZE: usize = 16 * 1024 * 1024;

fn system_for_pool(pool: erp_testkit::deadpool_postgres::Pool) -> ModuleSystem {
    let mut index = AddonIndex::new(true);
    index.add_dev_path(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../addons")
            .canonicalize()
            .unwrap(),
    );
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
    ModuleSystem::builder(pool, index)
        .native_artifact_identities(identities)
        .view_types(erp_addon_web::view_type_contributions())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_mail::ADDON,
            &erp_addon_uom::ADDON,
            &erp_addon_hr::ADDON,
        ])
        .build()
        .unwrap()
}

/// A clone of the installed `hr` template and an Orm that enforces access rows and memberships.
async fn cloned_installed() -> (TestDb, Orm) {
    let db = TemplateDb::clone_or_init("erp-suite-hr:hr:v2", |pool| async move {
        let system = system_for_pool(pool);
        let init = Box::pin(system.initialize(InitOptions::new().with_skip_auto_install(true)))
            .await
            .map_err(|e| e.to_string())?;
        let env = init.orm.env(SUPERUSER_ID, Context::new());
        let outcome =
            Box::pin(system.button_immediate_install(&env, &[ModuleName::new("hr").unwrap()]))
                .await
                .map_err(|e| e.to_string())?;
        if !outcome.post_commit.is_clean() {
            return Err(format!("post_commit not clean: {:?}", outcome.post_commit));
        }
        Ok::<(), String>(())
    })
    .await
    .unwrap();
    let system = system_for_pool(db.pool().clone());
    let loaded = Box::pin(system.load()).await.unwrap();
    let registry = Arc::clone(loaded.orm.registry());
    let source = PolicySource::build(Arc::clone(&registry), db.pool().clone())
        .await
        .unwrap();
    let orm = Orm::builder(Arc::clone(&registry), db.pool().clone())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .resolver(Resolver::for_registry(&registry).unwrap())
        .policy(Arc::new(source) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .build()
        .unwrap();
    (db, orm)
}

async fn xid(db: &TestDb, module: &str, name: &str) -> i64 {
    db.client()
        .await
        .unwrap()
        .query_one(
            "SELECT res_id::int8 FROM ir_model_data WHERE module = $1 AND name = $2",
            &[&module, &name],
        )
        .await
        .unwrap()
        .get(0)
}

/// Store a system parameter before the transactions that read it begin.
async fn set_parameter(orm: &Orm, key: &str, value: i64) {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.execute(
        "WITH updated AS (UPDATE ir_config_parameter SET value = $2 WHERE key = $1 RETURNING 1) \
         INSERT INTO ir_config_parameter (key, value) SELECT $1, $2 \
         WHERE NOT EXISTS (SELECT 1 FROM updated)",
        &[&key, &format!("{value}")],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn records(tx: &Transaction, model: &str, id: i64) -> Records {
    tx.env().records(model).unwrap().browse([id]).unwrap()
}

async fn create(tx: &mut Transaction, model: &str, vals: RawVals) -> i64 {
    let created = tx
        .env()
        .records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap();
    created.ids()[0]
}

async fn cell(tx: &mut Transaction, model: &str, id: i64, field: &str) -> Cell {
    records(tx, model, id).cell(tx, field).await.unwrap()
}

async fn call(
    tx: &mut Transaction,
    model: &str,
    id: i64,
    method: &'static str,
) -> erp_fields::Result<Value> {
    let rec = records(tx, model, id);
    tx.savepoint(async move |tx| rec.call::<(), Value>(tx, method, ()).await)
        .await
}

fn named(name: &str) -> RawVals {
    RawVals::new().set_cell("name", Cell::Str(name.into()))
}

fn employee(name: &str, company: i64, manager: Option<i64>) -> RawVals {
    let vals = named(name).set_cell("company_id", Cell::Id(company));
    match manager {
        Some(id) => vals.set_cell("parent_id", Cell::Id(id)),
        None => vals,
    }
}

fn key<'a>(value: &'a Value, name: &str) -> &'a Value {
    match value {
        Value::Map(map) => map.get(name).unwrap(),
        other => panic!("not a map: {other:?}"),
    }
}

fn list(value: &Value) -> &[Value] {
    match value {
        Value::List(items) => items,
        other => panic!("not a list: {other:?}"),
    }
}

fn int(value: &Value) -> i64 {
    match value {
        Value::Int(n) => *n,
        other => panic!("not an integer: {other:?}"),
    }
}

fn count(cell: Cell) -> i64 {
    match cell {
        Cell::Int(n) => i64::from(n),
        other => panic!("not a count: {other:?}"),
    }
}

fn date(year: i32, month: u32, day: u32) -> Date {
    Date::from_ymd(year, month, day).unwrap()
}

fn run<F: Future<Output = ()> + 'static>(test: impl FnOnce() -> F + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(test());
        })
        .unwrap()
        .join()
        .unwrap();
}

/// `get_org_chart` lists the manager chain and at most `hr.max_org_chart_rows` direct reports,
/// and reports how many there are and whether the list was cut.
#[test]
fn org_chart_reports_its_total_and_truncation() {
    run(|| async {
        let (db, orm) = cloned_installed().await;
        let company = xid(&db, "base", "main_company").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let boss = create(&mut tx, "hr.employee", employee("Boss", company, None)).await;
        let lead = create(
            &mut tx,
            "hr.employee",
            employee("Lead", company, Some(boss)),
        )
        .await;
        let mut reports = Vec::new();
        for name in ["Report A", "Report B", "Report C"] {
            reports.push(create(&mut tx, "hr.employee", employee(name, company, Some(lead))).await);
        }
        tx.commit().await.unwrap();
        let reports_len = i64::try_from(reports.len()).unwrap();

        set_parameter(&orm, erp_addon_hr::ORG_CHART_ROWS_KEY, 2).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let chart = call(&mut tx, "hr.employee", lead, "get_org_chart")
            .await
            .unwrap();
        let managers = list(key(&chart, "managers"));
        assert_eq!(managers.len(), 1);
        assert_eq!(int(key(&managers[0], "id")), boss);
        assert_eq!(list(key(&chart, "subordinates")).len(), 2);
        assert_eq!(int(key(&chart, "subordinates_total")), reports_len);
        assert_eq!(key(&chart, "subordinates_truncated"), &Value::Bool(true));
        let direct = count(cell(&mut tx, "hr.employee", lead, "subordinate_count").await);
        assert_eq!(direct, reports_len);
        assert_eq!(int(key(key(&chart, "self"), "direct_sub_count")), direct);
        tx.rollback().await.unwrap();

        set_parameter(&orm, erp_addon_hr::ORG_CHART_ROWS_KEY, 100).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let chart = call(&mut tx, "hr.employee", lead, "get_org_chart")
            .await
            .unwrap();
        let mut listed: Vec<i64> = list(key(&chart, "subordinates"))
            .iter()
            .map(|node| int(key(node, "id")))
            .collect();
        listed.sort_unstable();
        assert_eq!(listed, reports);
        assert_eq!(key(&chart, "subordinates_truncated"), &Value::Bool(false));
        tx.rollback().await.unwrap();
    });
}

/// `get_department_chart` cuts the sub-departments the same way; the employee count of a
/// department covers its whole subtree and the child count its direct sub-departments.
#[test]
fn department_chart_and_counts_cover_the_subtree() {
    run(|| async {
        let (db, orm) = cloned_installed().await;
        let company = xid(&db, "base", "main_company").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let top = create(&mut tx, "hr.department", named("Top")).await;
        let dept = |name: &str, parent: i64| named(name).set_cell("parent_id", Cell::Id(parent));
        let middle = create(&mut tx, "hr.department", dept("Middle", top)).await;
        let left = create(&mut tx, "hr.department", dept("Left", middle)).await;
        let right = create(&mut tx, "hr.department", dept("Right", middle)).await;
        let mut people = Vec::new();
        for (name, department) in [
            ("In Middle", middle),
            ("In Left", left),
            ("In Right", right),
        ] {
            let vals =
                employee(name, company, None).set_cell("department_id", Cell::Id(department));
            people.push(create(&mut tx, "hr.employee", vals).await);
        }
        records(&tx, "hr.department", middle)
            .write(
                &mut tx,
                RawVals::new().set_cell("manager_id", Cell::Id(people[0])),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let people_len = i64::try_from(people.len()).unwrap();

        set_parameter(&orm, erp_addon_hr::ORG_CHART_ROWS_KEY, 1).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        assert_eq!(
            count(cell(&mut tx, "hr.department", middle, "total_employee").await),
            people_len
        );
        assert_eq!(
            count(cell(&mut tx, "hr.department", top, "total_employee").await),
            people_len
        );
        assert_eq!(
            count(cell(&mut tx, "hr.department", left, "total_employee").await),
            1
        );
        assert_eq!(
            count(cell(&mut tx, "hr.department", middle, "child_count").await),
            2
        );
        let chart = call(&mut tx, "hr.department", middle, "get_department_chart")
            .await
            .unwrap();
        let ancestors = list(key(&chart, "ancestors"));
        assert_eq!(ancestors.len(), 1);
        assert_eq!(int(key(&ancestors[0], "id")), top);
        assert_eq!(list(key(&chart, "children")).len(), 1);
        assert_eq!(int(key(&chart, "children_total")), 2);
        assert_eq!(key(&chart, "children_truncated"), &Value::Bool(true));
        let own = key(&chart, "self");
        assert_eq!(int(key(own, "total_employee")), people_len);
        let manager_name = cell(&mut tx, "hr.employee", people[0], "name").await;
        assert_eq!(
            key(own, "manager_name"),
            &Value::from_cell(&manager_name),
            "the manager's name"
        );
        tx.rollback().await.unwrap();
    });
}

/// The wizard archives the employee and stores the departure date as the contract end; restoring
/// reactivates the employee and clears the departure. A wizard without a date is refused.
#[test]
fn departure_registers_and_cancels() {
    run(|| async {
        let (db, orm) = cloned_installed().await;
        let company = xid(&db, "base", "main_company").await;
        let reason = xid(&db, "hr", "hr_departure_reason_resigned").await;
        let start = date(2025, 1, 1);
        let leaving = date(2026, 6, 30);
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let person = create(
            &mut tx,
            "hr.employee",
            employee("Leaving", company, None).set_cell("contract_date_start", Cell::Date(start)),
        )
        .await;

        let wizard = |date: Option<Date>| {
            let vals = RawVals::new()
                .set_cell("employee_id", Cell::Id(person))
                .set_cell("departure_reason_id", Cell::Id(reason));
            match date {
                Some(day) => vals.set_cell("departure_date", Cell::Date(day)),
                None => vals,
            }
        };
        let refused = tx
            .savepoint(async |tx| {
                tx.env()
                    .records("hr.departure.wizard")?
                    .create(tx, vec![wizard(None)])
                    .await?;
                tx.flush().await
            })
            .await
            .unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Validation, "{refused}");
        assert_eq!(
            cell(&mut tx, "hr.employee", person, "contract_date_end").await,
            Cell::Null
        );

        let wiz = create(&mut tx, "hr.departure.wizard", wizard(Some(leaving))).await;
        call(
            &mut tx,
            "hr.departure.wizard",
            wiz,
            "action_register_departure",
        )
        .await
        .unwrap();
        assert_eq!(
            cell(&mut tx, "hr.employee", person, "active").await,
            Cell::Bool(false)
        );
        for field in ["departure_date", "contract_date_end"] {
            assert_eq!(
                cell(&mut tx, "hr.employee", person, field).await,
                Cell::Date(leaving),
                "{field}"
            );
        }
        assert_eq!(
            cell(&mut tx, "hr.employee", person, "departure_reason_id").await,
            Cell::Id(reason)
        );

        call(&mut tx, "hr.employee", person, "action_cancel_departure")
            .await
            .unwrap();
        assert_eq!(
            cell(&mut tx, "hr.employee", person, "active").await,
            Cell::Bool(true)
        );
        for field in ["departure_date", "departure_reason_id"] {
            assert_eq!(
                cell(&mut tx, "hr.employee", person, field).await,
                Cell::Null,
                "{field}"
            );
        }
        tx.rollback().await.unwrap();
    });
}

/// Generated badge codes are 12 digits, differ between employees and cannot repeat within one
/// company; another company may use the same code.
#[test]
fn badge_codes_are_random_and_unique_per_company() {
    run(|| async {
        let (db, orm) = cloned_installed().await;
        let company = xid(&db, "base", "main_company").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let currency = cell(&mut tx, "res.company", company, "currency_id").await;
        let other = create(
            &mut tx,
            "res.company",
            named("Badge Company").set_cell("currency_id", currency),
        )
        .await;
        let first = create(&mut tx, "hr.employee", employee("First", company, None)).await;
        let second = create(&mut tx, "hr.employee", employee("Second", company, None)).await;
        let abroad = create(&mut tx, "hr.employee", employee("Abroad", other, None)).await;
        let mut codes = Vec::new();
        for id in [first, second] {
            call(&mut tx, "hr.employee", id, "action_generate_barcode")
                .await
                .unwrap();
            let Cell::Str(code) = cell(&mut tx, "hr.employee", id, "barcode").await else {
                panic!("no badge code");
            };
            assert_eq!(code.len(), 12, "{code}");
            assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code}");
            codes.push(code);
        }
        assert_ne!(codes[0], codes[1]);

        let taken = RawVals::new().set_cell("barcode", Cell::Str(codes[0].clone()));
        let rec = records(&tx, "hr.employee", second);
        let duplicate = {
            let taken = taken.clone();
            tx.savepoint(async move |tx| {
                rec.write(tx, taken).await?;
                tx.flush().await
            })
            .await
            .unwrap_err()
        };
        assert_eq!(duplicate.kind(), ErrorKind::Validation, "{duplicate}");
        assert!(
            duplicate
                .message()
                .contains("This Badge ID is already used by another employee of this company."),
            "{duplicate}"
        );
        records(&tx, "hr.employee", abroad)
            .write(&mut tx, taken)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
    });
}

/// A manager cycle is refused, and so is a chain deeper than `hr.max_hierarchy_depth`.
#[test]
fn manager_chain_refuses_cycles_and_excess_depth() {
    run(|| async {
        let (db, orm) = cloned_installed().await;
        let company = xid(&db, "base", "main_company").await;
        set_parameter(&orm, erp_addon_hr::HIERARCHY_DEPTH_KEY, 2).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let first = create(&mut tx, "hr.employee", employee("Level 0", company, None)).await;
        let second = create(
            &mut tx,
            "hr.employee",
            employee("Level 1", company, Some(first)),
        )
        .await;
        let third = create(
            &mut tx,
            "hr.employee",
            employee("Level 2", company, Some(second)),
        )
        .await;

        let rec = records(&tx, "hr.employee", first);
        let cycle = tx
            .savepoint(async move |tx| {
                rec.write(tx, RawVals::new().set_cell("parent_id", Cell::Id(third)))
                    .await?;
                tx.flush().await
            })
            .await
            .unwrap_err();
        assert_eq!(cycle.kind(), ErrorKind::Validation, "{cycle}");
        assert!(
            cycle.message().contains("recursive manager hierarchy"),
            "{cycle}"
        );

        let too_deep = tx
            .savepoint(async |tx| {
                tx.env()
                    .records("hr.employee")?
                    .create(tx, vec![employee("Level 3", company, Some(third))])
                    .await?;
                tx.flush().await
            })
            .await
            .unwrap_err();
        assert_eq!(too_deep.kind(), ErrorKind::Validation, "{too_deep}");
        assert!(
            too_deep
                .message()
                .contains(erp_addon_hr::HIERARCHY_DEPTH_KEY),
            "{too_deep}"
        );

        let chart = call(&mut tx, "hr.employee", third, "get_org_chart")
            .await
            .unwrap();
        let managers: Vec<i64> = list(key(&chart, "managers"))
            .iter()
            .map(|node| int(key(node, "id")))
            .collect();
        assert_eq!(managers, [first, second]);
        tx.rollback().await.unwrap();
    });
}

/// A contract history row takes the company of its employee and follows it when it changes.
#[test]
fn contract_history_company_follows_the_employee() {
    run(|| async {
        let (db, orm) = cloned_installed().await;
        let company = xid(&db, "base", "main_company").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let currency = cell(&mut tx, "res.company", company, "currency_id").await;
        let other = create(
            &mut tx,
            "res.company",
            named("Wage Company").set_cell("currency_id", currency),
        )
        .await;
        let person = create(&mut tx, "hr.employee", employee("Contracted", other, None)).await;
        let version = create(
            &mut tx,
            "hr.employee.version",
            named("CTR-1")
                .set_cell("employee_id", Cell::Id(person))
                .set_cell("date_start", Cell::Date(date(2025, 1, 1))),
        )
        .await;
        assert_eq!(
            cell(&mut tx, "hr.employee.version", version, "company_id").await,
            Cell::Id(other)
        );
        records(&tx, "hr.employee", person)
            .write(
                &mut tx,
                RawVals::new().set_cell("company_id", Cell::Id(company)),
            )
            .await
            .unwrap();
        assert_eq!(
            cell(&mut tx, "hr.employee.version", version, "company_id").await,
            Cell::Id(company)
        );
        assert_eq!(
            count(cell(&mut tx, "hr.employee", person, "versions_count").await),
            1
        );
        tx.rollback().await.unwrap();
    });
}

/// An internal user without the officer group reads an employee's name but not the note, the
/// contract dates or a certificate; an officer's new employee takes the officer's time zone, and
/// without one the company's.
#[test]
fn officer_fields_and_default_time_zone() {
    run(|| async {
        let (db, orm) = cloned_installed().await;
        let company = xid(&db, "base", "main_company").await;
        let user_group = xid(&db, "base", "group_user").await;
        let admin_group = xid(&db, "base", "group_system").await;
        let officer_group = xid(&db, "hr", "group_hr_user").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        records(&tx, "res.company", company)
            .write(
                &mut tx,
                RawVals::new().set_cell("tz", Cell::Str("Asia/Jayapura".into())),
            )
            .await
            .unwrap();
        let user = |login: &str, group: i64| {
            named(login)
                .set_cell("login", Cell::Str(login.into()))
                .set_cell("company_id", Cell::Id(company))
                .set_commands(
                    "company_ids",
                    [Command::Set(vec![RecId::from_real(company).unwrap()])],
                )
                .set_commands(
                    "group_ids",
                    [Command::Set(vec![RecId::from_real(group).unwrap()])],
                )
        };
        // The base addon refuses a user table without an administrator, so one comes first.
        let users = tx
            .env()
            .records("res.users")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    user("hr-admin", admin_group),
                    user("hr-reader", user_group),
                    user("hr-officer", officer_group)
                        .set_cell("tz", Cell::Str("Asia/Makassar".into())),
                    user("hr-officer-no-tz", officer_group),
                ],
            )
            .await
            .unwrap();
        let [_, reader, officer, plain_officer] = users.ids()[..] else {
            panic!("four users");
        };
        let person = create(
            &mut tx,
            "hr.employee",
            employee("Private", company, None)
                .set_cell("additional_note", Cell::Str("internal".into())),
        )
        .await;
        create(
            &mut tx,
            "hr.resume.line",
            named("Course")
                .set_cell("employee_id", Cell::Id(person))
                .set_cell("date_start", Cell::Date(date(2024, 1, 1)))
                .set_cell("certificate_filename", Cell::Str("course.pdf".into())),
        )
        .await;
        let company_tz = cell(&mut tx, "res.company", company, "tz").await;
        // a new employee takes the creating user's zone when it has one, else the company's
        let mut expected = Vec::new();
        for uid in [officer, plain_officer] {
            let zone = match cell(&mut tx, "res.users", uid, "tz").await {
                Cell::Str(zone) if !zone.is_empty() => Cell::Str(zone),
                _ => company_tz.clone(),
            };
            expected.push((uid, zone));
        }
        assert_ne!(
            expected[0].1, company_tz,
            "the officer has a zone of its own"
        );
        tx.commit().await.unwrap();

        let mut tx = orm
            .begin(erp_sdk::UserId::new(reader).unwrap())
            .await
            .unwrap();
        let rec = records(&tx, "hr.employee", person);
        rec.read(&mut tx, &["name"]).await.unwrap();
        for field in [
            "additional_note",
            "contract_date_start",
            "contract_date_end",
            "contract_period_label",
            "employee_type_id",
        ] {
            let error = rec.read(&mut tx, &[field]).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Access, "{field}: {error}");
        }
        let lines = tx
            .env()
            .records("hr.resume.line")
            .unwrap()
            .search(&mut tx, &erp_sdk::prelude::Domain::TRUE)
            .await
            .unwrap();
        assert_eq!(lines.ids().len(), 1);
        for field in ["certificate_file", "certificate_filename"] {
            let error = lines.read(&mut tx, &[field]).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Access, "{field}: {error}");
        }
        tx.rollback().await.unwrap();

        for (uid, zone) in expected {
            let mut tx = orm.begin(erp_sdk::UserId::new(uid).unwrap()).await.unwrap();
            let hired = create(&mut tx, "hr.employee", employee("Hired", company, None)).await;
            assert_eq!(
                cell(&mut tx, "hr.employee", hired, "tz").await,
                zone,
                "user {uid}"
            );
            tx.rollback().await.unwrap();
        }
    });
}
