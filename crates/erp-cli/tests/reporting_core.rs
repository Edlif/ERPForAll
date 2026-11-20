// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dynamic reports (`addons/reporting`) installed with a fixture addon of test models and two
//! report definitions (`tests/fixtures/reporting/reporting_fixture`), computed as an administrator and as an
//! internal user whose access row hides some records.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]
use std::path::Path;
use std::sync::Arc;

use erp_addon_reporting::export;
use erp_addon_reporting::runtime;
use erp_fields::{Cell, ErrorKind, RecId, Value};
use erp_modules::{AddonIndex, InitOptions, ModuleName, ModuleSystem};
use erp_orm::rpc::{Exposure, RpcArgs};
use erp_orm::{Command, Context, Orm, RawVals, SUPERUSER_ID, Transaction, UserId};
use erp_sdk::Domain;
use erp_security::{resolver::Resolver, source::PolicySource};
use erp_testkit::TestDb;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

#[path = "support/report_frame.rs"]
mod report_frame;

fn system(db: &TestDb) -> ModuleSystem {
    let mut index = AddonIndex::new(true);
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    index.add_dev_path(here.join("../../addons").canonicalize().unwrap());
    index.add_dev_path(
        here.join("tests/fixtures/reporting")
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
    ModuleSystem::builder(db.pool().clone(), index)
        .native_artifact_identities(identities)
        .view_types(erp_addon_web::view_type_contributions())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        // the catalogue holds every addon of `addons/`: these carry the native hooks of the ones discovered
        .code([
            &erp_addon_uom::ADDON,
            &erp_addon_acceptance_notes::ADDON,
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_reporting::ADDON,
        ])
        .build()
        .unwrap()
}

/// Run `body` on a thread with a large stack: a full install chain exceeds the default one.
fn run(body: impl std::future::Future<Output = ()> + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(body);
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn xmlid(tx: &mut Transaction, module: &str, name: &str) -> i64 {
    let rows = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module = $1 AND name = $2",
            &[&module, &name],
        )
        .await
        .unwrap();
    rows[0].get::<_, i64>(0)
}

struct Fixture {
    // the database lives as long as the fixture
    _db: TestDb,
    orm: Orm,
    admin: UserId,
    clerk: UserId,
    stock: i64,
    secret: i64,
    partners: i64,
    bins: Vec<i64>,
}

fn day(m: u32, d: u32) -> Cell {
    Cell::Date(erp_sdk::Date::from_ymd(2026, m, d).unwrap())
}

async fn fixture() -> Fixture {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let system = system(&db);
    let initialized = system.initialize(InitOptions::new()).await.unwrap();
    assert!(initialized.post_commit.is_clean());
    let installed = system
        .button_immediate_install(
            &system.system_env(Context::new()),
            &[ModuleName::new("reporting_fixture").unwrap()],
        )
        .await
        .unwrap();
    assert!(
        installed.post_commit.is_clean(),
        "{:?}",
        installed.post_commit
    );
    let installed = installed.orm;
    // the compiled policy of the installed access data, as the server builds it
    let source = PolicySource::build(Arc::clone(installed.registry()), db.pool().clone())
        .await
        .unwrap();
    let orm = Orm::builder(Arc::clone(installed.registry()), db.pool().clone())
        .generation_attachment(erp_orm::GenerationAttachment::new(
            Arc::clone(installed.registry()),
            installed
                .env(SUPERUSER_ID, Context::new())
                .generation_attachment::<erp_views::PublishedViewTypes>()
                .unwrap(),
        ))
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .resolver(Resolver::for_registry(installed.registry()).unwrap())
        .policy(Arc::new(source) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = xmlid(&mut tx, "base", "main_company").await;
    let mut users = Vec::new();
    for (login, group) in [
        ("report-admin", "group_system"),
        ("report-clerk", "group_user"),
    ] {
        let group = xmlid(&mut tx, "base", group).await;
        let created = tx
            .env()
            .records("res.users")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str(login.into()))
                        .set_cell("login", Cell::Str(login.into()))
                        .set_cell(
                            "password",
                            Cell::Str(format!("{login}-fixture-password").into()),
                        )
                        .set_cell("company_id", Cell::Id(company))
                        .set_commands(
                            "company_ids",
                            [Command::Set(vec![RecId::from_real(company).unwrap()])],
                        )
                        .set_commands(
                            "group_ids",
                            [Command::Set(vec![RecId::from_real(group).unwrap()])],
                        ),
                ],
            )
            .await
            .unwrap();
        users.push(UserId::new(created.ids()[0]).unwrap());
    }
    let names: Vec<RawVals> = (1..=12)
        .map(|i| RawVals::new().set_cell("name", Cell::Str(format!("B{i:02}").into())))
        .collect();
    let bins: Vec<i64> = tx
        .env()
        .records("reporting_fixture.bin")
        .unwrap()
        .create(&mut tx, names)
        .await
        .unwrap()
        .ids()
        .to_vec();
    let item = |name: &str,
                bin: i64,
                kind: &str,
                qty: f64,
                reserved: f64,
                date: Cell,
                restricted: bool| {
        RawVals::new()
            .set_cell("name", Cell::Str(name.into()))
            .set_cell("bin_id", Cell::Id(bin))
            .set_cell("kind", Cell::Str(kind.into()))
            .set_cell("qty", Cell::Float(qty))
            .set_cell("reserved", Cell::Float(reserved))
            .set_cell("day", date)
            .set_cell("restricted", Cell::Bool(restricted))
    };
    let mut items: Vec<RawVals> = (0..12)
        .map(|i| item("P1", bins[i], "in", (i + 1) as f64, 1.0, day(11, 10), false))
        .collect();
    items.push(item("P2", bins[0], "out", 2.0, 0.0, day(11, 15), false));
    items.push(item("P3", bins[1], "in", 7.0, 0.0, day(10, 5), false));
    items.push(item("PX", bins[0], "in", 100.0, 0.0, day(11, 5), true));
    tx.env()
        .records("reporting_fixture.item")
        .unwrap()
        .create(&mut tx, items)
        .await
        .unwrap();
    tx.env()
        .records("reporting_fixture.secret")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("amount", Cell::Float(5.0))],
        )
        .await
        .unwrap();
    let stock = xmlid(&mut tx, "reporting_fixture", "stock").await;
    let secret = xmlid(&mut tx, "reporting_fixture", "secret").await;
    let partners = xmlid(&mut tx, "reporting", "report_partner_country").await;
    tx.install_audit_schema().await.unwrap();
    tx.commit().await.unwrap();
    Fixture {
        _db: db,
        orm,
        admin: users[0],
        clerk: users[1],
        stock,
        secret,
        partners,
        bins,
    }
}

fn november(report: i64) -> Json {
    json!({"report": report, "date": {"preset": "custom", "from": "2026-11-01", "to": "2026-11-30"}})
}

fn with(mut options: Json, key: &str, value: Json) -> Json {
    options[key] = value;
    options
}

fn line<'a>(answer: &'a Json, id: &str) -> &'a Json {
    answer["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == id)
        .unwrap_or_else(|| panic!("no line {id} in {answer}"))
}

fn cells(line: &Json) -> Vec<Json> {
    line["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["value"].clone())
        .collect()
}

fn names(answer: &Json) -> Vec<String> {
    answer["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap().to_owned())
        .collect()
}

async fn lines(orm: &Orm, user: UserId, input: &Json) -> erp_sdk::Result<Json> {
    let mut tx = orm.begin(user).await.unwrap();
    let env = tx.env().clone();
    runtime::report_lines(&env, &mut tx, input).await
}

async fn expand(
    orm: &Orm,
    user: UserId,
    input: &Json,
    id: &str,
    offset: i64,
) -> erp_sdk::Result<Json> {
    let mut tx = orm.begin(user).await.unwrap();
    let env = tx.env().clone();
    runtime::report_expand(&env, &mut tx, input, id, offset).await
}

fn refused(result: erp_sdk::Result<Json>, kind: ErrorKind, text: &str) {
    let error = result.unwrap_err();
    assert_eq!(error.kind(), kind, "{}", error.message());
    assert!(error.message().contains(text), "{}", error.message());
}

#[test]
fn open_definition_returns_the_selected_report() {
    run(async {
        let f = fixture().await;
        let mut tx = f.orm.begin(f.clerk).await.unwrap();
        let env = tx.env();
        let form = xmlid(&mut tx, "reporting", "reporting_report_form").await;
        let views = erp_addon_web::metadata_test_support::test_get_views(
            &env,
            &mut tx,
            "reporting.report",
            vec![Value::list([Value::Int(form), Value::str("form")])],
            32 * 1024 * 1024,
        )
        .await
        .unwrap();
        assert!(
            serde_json::to_string(&views)
                .unwrap()
                .contains("action_open_report")
        );
        // The relation editor supplies the parent link on save and hides its input.
        // Forwarding a required inverse field in the child form prevents creating rows.
        for (model, inverse) in [
            ("reporting.report.line", "report_id"),
            ("reporting.report.column", "report_id"),
            ("reporting.report.expression", "line_id"),
        ] {
            let child = erp_addon_web::metadata_test_support::test_get_views(
                &env,
                &mut tx,
                model,
                vec![Value::list([Value::False, Value::str("form")])],
                32 * 1024 * 1024,
            )
            .await
            .unwrap();
            let wire = serde_json::to_value(child).unwrap();
            let fields: Vec<_> = wire["views"][0]["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|node| node["field"]["name"].as_str())
                .collect();
            assert!(!fields.is_empty());
            assert!(!fields.contains(&inverse), "{model}: {fields:?}");
        }
        for id in [f.stock, f.partners] {
            let records = tx
                .env()
                .records("reporting.report")
                .unwrap()
                .browse([id])
                .unwrap();
            let Exposure::Exposed(method) = tx
                .exposure()
                .get(records.model_id(), "action_open_report")
                .unwrap()
                .clone()
            else {
                panic!("action_open_report must be exposed")
            };
            let result = method
                .call(&mut tx, records, RpcArgs::new(Default::default()))
                .await
                .unwrap();
            let action = erp_fields::convert::value_to_json(&result).unwrap();
            assert_eq!(action["type"], "ir.actions.client");
            assert_eq!(action["tag"], "reporting.report");
            assert_eq!(action["params"]["report"], id);
        }
    });
}

#[test]
fn definition_reports_compute_unfold_page_and_compare() {
    run(async {
        let f = fixture().await;
        let root = format!("root~reporting.report~{}", f.stock);
        let on_hand = format!("{root}|line:ON_HAND~~");
        let bin = |i: usize| format!("{on_hand}|group:bin_id~reporting_fixture.bin~{}", f.bins[i]);

        // erp_report_options through the RPC: one column group, the report's columns, no order
        let mut tx = f.orm.begin(f.clerk).await.unwrap();
        let records = tx.env().records("reporting.report").unwrap();
        let Exposure::Exposed(method) = tx
            .exposure()
            .get(records.model_id(), "erp_report_options")
            .unwrap()
            .clone()
        else {
            panic!("erp_report_options is exposed")
        };
        let args = erp_fields::ValueMap::from_iter([
            ("report".into(), Value::Int(f.stock)),
            ("previous".into(), Value::Json(Arc::new(json!({})))),
        ]);
        let answer = method
            .call(&mut tx, records, RpcArgs::new(args))
            .await
            .unwrap();
        let answer = erp_fields::convert::value_to_json(&answer).unwrap();
        let options = &answer["options"];
        assert_eq!(options["date"]["preset"], "this_month");
        assert_eq!(options["comparison"]["mode"], "none");
        assert_eq!(
            (options["unfold_all"].clone(), options["hide_zero"].clone()),
            (json!(false), json!(false))
        );
        assert_eq!(options["order"], Json::Null);
        assert_eq!(options["column_groups"].as_array().unwrap().len(), 1);
        let labels: Vec<&str> = options["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["qty", "reserved", "available"]);
        drop(tx);

        // top level: aggregate, same-line formula, sum_children, other-line formula
        let top = lines(&f.orm, f.clerk, &november(f.stock)).await.unwrap();
        assert_eq!(names(&top), ["On Hand", "Moves", "Net"]);
        assert_eq!(
            cells(line(&top, &on_hand)),
            [json!("87.00"), json!("12.00"), json!("75.00")]
        );
        assert_eq!(
            cells(line(&top, &format!("{root}|line:MOVES~~")))[0],
            "80.00"
        );
        assert_eq!(cells(line(&top, &format!("{root}|line:NET~~")))[0], "76.00");
        assert!(line(&top, &on_hand)["unfoldable"].as_bool().unwrap());
        assert_eq!(
            line(&top, &format!("{root}|line:NET~~"))["unfoldable"],
            false
        );

        // unfolded on reload: ten bins, the load-more line with the rest, the total below
        let unfolded = with(november(f.stock), "unfolded", json!([on_hand]));
        let answer = lines(&f.orm, f.clerk, &unfolded).await.unwrap();
        let shown = names(&answer);
        assert_eq!(&shown[..2], ["On Hand", "B01"]);
        assert_eq!(shown[10], "B10");
        let more = &answer["lines"][11];
        assert_eq!(
            (
                more["kind"].clone(),
                more["remaining"].clone(),
                more["next_offset"].clone()
            ),
            (json!("more"), json!(2), json!(10))
        );
        assert_eq!(cells(more), [json!("23.00"), json!("2.00"), json!("21.00")]);
        let total = &answer["lines"][12];
        assert_eq!(
            (
                total["id"].as_str().unwrap(),
                total["kind"].as_str().unwrap()
            ),
            (format!("{on_hand}|total~~").as_str(), "total")
        );
        assert_eq!(cells(total), cells(line(&answer, &on_hand)));
        assert_eq!(answer["lines"][13]["name"], "Moves");
        assert_eq!(
            cells(line(&answer, &bin(0))),
            [json!("3.00"), json!("1.00"), json!("2.00")]
        );
        assert_eq!(line(&answer, &bin(0))["level"], 1);

        // the next page, then the last level (detail lines, no further unfolding)
        let page = expand(&f.orm, f.clerk, &november(f.stock), &on_hand, 10)
            .await
            .unwrap();
        assert_eq!(names(&page), ["B11", "B12"]);
        let details = expand(&f.orm, f.clerk, &november(f.stock), &bin(0), 0)
            .await
            .unwrap();
        assert_eq!(names(&details), ["P1", "P2"]);
        assert_eq!(details["lines"][0]["kind"], "detail");
        assert_eq!(details["lines"][0]["unfoldable"], false);
        let sum: Vec<f64> = details["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["cells"][0]["value"].as_str().unwrap().parse().unwrap())
            .collect();
        assert_eq!(sum.iter().sum::<f64>(), 3.0);

        // a previous-period comparison: November then October, columns per group
        let compared = with(
            november(f.stock),
            "comparison",
            json!({"mode": "previous_period", "periods": 1}),
        );
        let answer = lines(&f.orm, f.clerk, &compared).await.unwrap();
        assert_eq!(
            answer["options"]["column_groups"],
            json!([{"from": "2026-11-01", "to": "2026-11-30"}, {"from": "2026-10-01", "to": "2026-10-31"}])
        );
        assert_eq!(answer["options"]["columns"].as_array().unwrap().len(), 6);
        let net = cells(line(&answer, &format!("{root}|line:NET~~")));
        assert_eq!(
            (net[0].clone(), net[3].clone()),
            (json!("76.00"), json!("7.00"))
        );
        let ascending = with(
            november(f.stock),
            "comparison",
            json!({"mode": "previous_period", "periods": 2, "order": "ascending"}),
        );
        let answer = lines(&f.orm, f.clerk, &ascending).await.unwrap();
        assert_eq!(
            answer["options"]["column_groups"][2],
            json!({"from": "2026-11-01", "to": "2026-11-30"})
        );
        let too_many = with(
            november(f.stock),
            "comparison",
            json!({"mode": "previous_period", "periods": 13}),
        );
        refused(
            lines(&f.orm, f.clerk, &too_many).await,
            ErrorKind::Validation,
            "from 1 to 12 (setting reporting.max_comparison_periods)",
        );

        // the default report of the addon: contacts per country, the count a read_group gives
        let answer = lines(&f.orm, f.admin, &json!({"report": f.partners}))
            .await
            .unwrap();
        let mut tx = f.orm.begin(f.admin).await.unwrap();
        let env = tx.env().clone();
        let companies: Vec<Json> = tx
            .companies(&env)
            .await
            .unwrap()
            .iter()
            .map(|c| json!(c.get()))
            .chain([json!(false)])
            .collect();
        let count = tx
            .env()
            .records("res.partner")
            .unwrap()
            .search_count(
                &mut tx,
                &Domain::from_json(&json!([["company_id", "in", companies]])).unwrap(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(names(&answer), ["Contacts"]);
        assert_eq!(
            answer["lines"][0]["cells"][0]["value"],
            json!(count.to_string())
        );
    });
}

#[test]
fn line_ids_orders_drill_downs_and_saves_are_checked() {
    run(async {
        let f = fixture().await;
        let on_hand = format!("root~reporting.report~{}|line:ON_HAND~~", f.stock);
        let bin = |i: usize| format!("{on_hand}|group:bin_id~reporting_fixture.bin~{}", f.bins[i]);
        let options = november(f.stock);

        for (id, segment) in [
            (format!("{on_hand}|group:kind~~in"), 3),
            (
                format!("{on_hand}|group:bin_id~res.partner~{}", f.bins[0]),
                3,
            ),
            (format!("{}|group:name~~P1|group:name~~P1", bin(0)), 5),
            (
                format!("root~reporting.report~{}|line:ON_HAND~~", f.secret),
                1,
            ),
            (format!("{on_hand}|group:bin_id~reporting_fixture.bin~x"), 3),
        ] {
            refused(
                expand(&f.orm, f.clerk, &options, &id, 0).await,
                ErrorKind::Validation,
                &format!("segment {segment} "),
            );
        }

        // a descending order pages the groups by their sum; a formula column is not sortable
        let ordered = with(
            options.clone(),
            "order",
            json!({"column": "qty", "direction": "desc"}),
        );
        let first = expand(&f.orm, f.clerk, &ordered, &on_hand, 0)
            .await
            .unwrap();
        assert_eq!(&names(&first)[..4], ["B12", "B11", "B10", "B02"]);
        let last = expand(&f.orm, f.clerk, &ordered, &on_hand, 10)
            .await
            .unwrap();
        assert_eq!(names(&last), ["B01", "B03"]);
        let by_formula = with(
            options.clone(),
            "order",
            json!({"column": "available", "direction": "desc"}),
        );
        refused(
            lines(&f.orm, f.clerk, &by_formula).await,
            ErrorKind::Validation,
            "column available is not sortable",
        );

        // drill-downs: the record behind a group line, the records behind a cell
        let mut tx = f.orm.begin(f.clerk).await.unwrap();
        let env = tx.env().clone();
        let open =
            runtime::report_drill(&env, &mut tx, &options, &bin(0), &json!({"item": "open"}))
                .await
                .unwrap();
        assert_eq!(open["res_model"], "reporting_fixture.bin");
        assert_eq!(open["res_id"], f.bins[0]);
        erp_addon_web::metadata_test_support::test_normalize_action_result(
            &env,
            &mut tx,
            Value::Json(Arc::new(open)),
        )
        .await
        .unwrap();
        let cell = runtime::report_drill(&env, &mut tx, &options, &bin(0), &json!({"cell": 0}))
            .await
            .unwrap();
        assert_eq!(
            (cell["res_model"].as_str(), cell["target"].as_str()),
            (Some("reporting_fixture.item"), Some("current"))
        );
        let listed = env
            .records("reporting_fixture.item")
            .unwrap()
            .search(&mut tx, &Domain::from_json(&cell["domain"]).unwrap())
            .await
            .unwrap()
            .read(&mut tx, &["qty"])
            .await
            .unwrap();
        let total: f64 = listed
            .iter()
            .map(|r| match r["qty"] {
                Value::Float(q) => q,
                ref v => panic!("{v:?}"),
            })
            .sum();
        assert_eq!(total, 3.0);
        erp_addon_web::metadata_test_support::test_normalize_action_result(
            &env,
            &mut tx,
            Value::Json(Arc::new(cell)),
        )
        .await
        .unwrap();
        let formula =
            runtime::report_drill(&env, &mut tx, &options, &bin(0), &json!({"cell": 2})).await;
        refused(formula, ErrorKind::Validation, "is a formula");
        refused(
            runtime::report_drill(&env, &mut tx, &options, &on_hand, &json!({"item": "open"}))
                .await,
            ErrorKind::Validation,
            "not a record to open",
        );
        drop(tx);

        // saves: the load-more bound and a formula cycle, each refused by name
        for limit in [5, 2001] {
            let mut tx = f.orm.begin(f.admin).await.unwrap();
            let report = tx
                .env()
                .records("reporting.report")
                .unwrap()
                .browse([f.stock])
                .unwrap();
            let error = report
                .write(
                    &mut tx,
                    RawVals::new().set_cell("load_more_limit", Cell::Int(limit)),
                )
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation);
            assert_eq!(
                error.message(),
                "Report 'Fixture Stock': Load More Limit must be from 10 to 2000 (setting reporting.max_load_more_limit)."
            );
        }
        let mut tx = f.orm.begin(f.admin).await.unwrap();
        let net = xmlid(&mut tx, "reporting_fixture", "stock_net_qty").await;
        let expression = tx
            .env()
            .records("reporting.report.expression")
            .unwrap()
            .browse([net])
            .unwrap();
        let error = expression
            .write(
                &mut tx,
                RawVals::new().set_cell("formula", Cell::Str("IN.qty - NET.qty".into())),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(
            error.message(),
            "Report 'Fixture Stock': the formulas of NET.qty form a cycle."
        );
    });
}

#[test]
fn every_read_runs_as_the_caller() {
    run(async {
        let f = fixture().await;
        let on_hand = format!("root~reporting.report~{}|line:ON_HAND~~", f.stock);
        let bin = format!("{on_hand}|group:bin_id~reporting_fixture.bin~{}", f.bins[0]);

        // the administrator sees the restricted item and the secret line
        let admin = lines(&f.orm, f.admin, &november(f.stock)).await.unwrap();
        assert_eq!(cells(line(&admin, &on_hand))[0], "187.00");
        assert_eq!(names(&admin), ["On Hand", "Moves", "Net", "Secret"]);
        assert_eq!(admin["hidden_lines"], json!([]));
        let details = expand(&f.orm, f.admin, &november(f.stock), &bin, 0)
            .await
            .unwrap();
        assert_eq!(names(&details), ["P1", "P2", "PX"]);

        // the clerk's access row hides it everywhere; the optional line is left out and named
        let clerk = lines(&f.orm, f.clerk, &november(f.stock)).await.unwrap();
        assert_eq!(cells(line(&clerk, &on_hand))[0], "87.00");
        assert_eq!(names(&clerk), ["On Hand", "Moves", "Net"]);
        assert_eq!(clerk["hidden_lines"], json!(["Secret"]));
        let details = expand(&f.orm, f.clerk, &november(f.stock), &bin, 0)
            .await
            .unwrap();
        assert_eq!(names(&details), ["P1", "P2"]);

        // a line the caller may not read is a refusal, not a number
        refused(
            lines(&f.orm, f.clerk, &november(f.secret)).await,
            ErrorKind::Access,
            "",
        );
        let shown = lines(&f.orm, f.admin, &november(f.secret)).await.unwrap();
        assert_eq!(shown["lines"][0]["cells"][0]["value"], "5.00");
    });
}

/// One part of an xlsx archive as text.
fn part(book: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(book)).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("the workbook has {name}"))
        .read_to_string(&mut text)
        .unwrap();
    text
}

/// The cells of one sheet of an xlsx: reference to its text (shared strings resolved) or number.
fn sheet_cells(book: &[u8], sheet: &str) -> BTreeMap<String, String> {
    let shared: Vec<String> = part(book, "xl/sharedStrings.xml")
        .split("<si>")
        .skip(1)
        .map(|si| {
            let from = si.find("<t").unwrap();
            let from = from + si[from..].find('>').unwrap() + 1;
            si[from..si.find("</t>").unwrap()].replace("&amp;", "&")
        })
        .collect();
    part(book, sheet)
        .split("<c r=\"")
        .skip(1)
        .filter_map(|cell| {
            let (reference, rest) = cell.split_once('"').unwrap();
            let (attrs, rest) = rest.split_once('>')?;
            if attrs.ends_with('/') {
                return None;
            }
            let value = rest.split_once("<v>")?.1.split_once("</v>")?.0;
            let value = if attrs.contains("t=\"s\"") {
                shared[value.parse::<usize>().unwrap()].clone()
            } else {
                value.to_owned()
            };
            Some((reference.to_owned(), value))
        })
        .collect()
}

/// The first column of a sheet, from `from_row`: the line names.
fn first_column(cells: &BTreeMap<String, String>, from_row: u32) -> Vec<String> {
    (from_row..=200)
        .map_while(|row| cells.get(&format!("A{row}")).cloned())
        .collect()
}

async fn prepared(
    f: &Fixture,
    user: UserId,
    report: i64,
    options: Json,
    format: &str,
) -> erp_sdk::Result<export::Prepared> {
    let mut tx = f.orm.begin(user).await.unwrap();
    let body = json!({"report": report, "options": options, "format": format});
    export::prepare(&mut tx, body.to_string().as_bytes()).await
}

fn text_of_pdf(bytes: &[u8]) -> (Vec<String>, Vec<Vec<f64>>) {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    let mut texts = Vec::new();
    let mut boxes = Vec::new();
    for page in pdf.get_pages().keys() {
        let text = pdf.extract_text(&[*page]).unwrap();
        texts.push(text.chars().filter(|c| !c.is_whitespace()).collect());
        let id = pdf.get_pages()[page];
        let media = pdf
            .get_dictionary(id)
            .unwrap()
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        boxes.push(media.iter().map(|n| n.as_float().unwrap().into()).collect());
    }
    (texts, boxes)
}

fn pdf_of(prepared: &export::Prepared) -> Vec<u8> {
    let document = report_frame::through_frame(&prepared.document().unwrap());
    let fonts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts");
    let engine = erp_report_typst::TypstEngine::new(&fonts, erp_report::Limits::default()).unwrap();
    use erp_report::LayoutEngine as _;
    engine.render(&document).unwrap().into_bytes()
}

/// numbers as numbers, the options sheet; the route is `human_reporting_export`)
#[test]
fn xlsx_holds_the_screens_lines_as_numbers_and_never_a_formula() {
    run(async {
        let f = fixture().await;
        // two items whose names read as formulas, in the first bin
        let item = |name: &str, qty: f64| {
            RawVals::new()
                .set_cell("name", Cell::Str(name.into()))
                .set_cell("bin_id", Cell::Id(f.bins[0]))
                .set_cell("kind", Cell::Str("in".into()))
                .set_cell("qty", Cell::Float(qty))
                .set_cell("reserved", Cell::Float(0.0))
                .set_cell("day", day(11, 12))
                .set_cell("restricted", Cell::Bool(false))
        };
        let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
        tx.env()
            .records("reporting_fixture.item")
            .unwrap()
            .create(&mut tx, vec![item("=1+1", 3.0), item("@SUM(A1)", 9.0)])
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let all = with(november(f.stock), "unfold_all", json!(true));
        let book = prepared(&f, f.admin, f.stock, all.clone(), "xlsx")
            .await
            .unwrap()
            .xlsx()
            .unwrap();
        let cells = sheet_cells(&book.bytes, "xl/worksheets/sheet1.xml");
        let names = first_column(&cells, 3);
        // two header rows, then the screen's lines with their totals below
        assert_eq!(cells["B1"], "2026-11-01 - 2026-11-30");
        assert_eq!(
            (
                cells["B2"].as_str(),
                cells["C2"].as_str(),
                cells["D2"].as_str()
            ),
            ("Quantity", "Reserved", "Available")
        );
        for name in [
            "On Hand", "B01", "P1", "=1+1", "@SUM(A1)", "Moves", "Incoming", "Net",
        ] {
            assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
        }
        // numbers are numeric cells: the item holds 9 and nothing reserved
        let row = names.iter().position(|n| n == "@SUM(A1)").unwrap() + 3;
        assert_eq!(cells[&format!("B{row}")], "9");
        assert_eq!(cells[&format!("C{row}")], "0");
        let sheet = part(&book.bytes, "xl/worksheets/sheet1.xml");
        assert!(!sheet.contains("<f>") && !sheet.contains("<f "), "{sheet}");
        assert!(part(&book.bytes, "xl/styles.xml").contains("quotePrefix=\"1\""));
        // the options used, on a second sheet
        let workbook = part(&book.bytes, "xl/workbook.xml");
        assert!(workbook.contains("name=\"Options\""), "{workbook}");
        let options = sheet_cells(&book.bytes, "xl/worksheets/sheet2.xml");
        assert!(options.values().any(|v| v == "unfold_all"), "{options:?}");
        assert!(options.values().any(|v| v == "date.preset"), "{options:?}");

        // folded: the same report without unfolding has no children
        let folded = prepared(&f, f.admin, f.stock, november(f.stock), "xlsx")
            .await
            .unwrap()
            .xlsx()
            .unwrap();
        let folded = first_column(&sheet_cells(&folded.bytes, "xl/worksheets/sheet1.xml"), 3);
        assert!(folded.iter().all(|n| n != "B01" && n != "P1"), "{folded:?}");

        // the search text keeps matches with their ancestors, and hide zero drops the zero lines
        let searched = with(all.clone(), "search", json!("B01"));
        let book = prepared(&f, f.admin, f.stock, searched, "xlsx")
            .await
            .unwrap()
            .xlsx()
            .unwrap();
        let names = first_column(&sheet_cells(&book.bytes, "xl/worksheets/sheet1.xml"), 3);
        assert!(
            names.iter().any(|n| n == "B01") && names.iter().any(|n| n == "P1"),
            "{names:?}"
        );
        assert!(
            names.iter().all(|n| n != "B02" && n != "Moves"),
            "{names:?}"
        );
        let december = json!({"report": f.stock, "unfold_all": true, "hide_zero": true,
            "date": {"preset": "custom", "from": "2026-12-01", "to": "2026-12-31"}});
        let book = prepared(&f, f.admin, f.stock, december, "xlsx")
            .await
            .unwrap()
            .xlsx()
            .unwrap();
        let names = first_column(&sheet_cells(&book.bytes, "xl/worksheets/sheet1.xml"), 3);
        assert!(names.iter().any(|n| n == "On Hand"), "{names:?}");
        assert!(
            names
                .iter()
                .all(|n| n != "Moves" && n != "Incoming" && n != "Net"),
            "{names:?}"
        );
    });
}

/// and the company letterhead)
#[test]
fn pdf_prints_the_letterhead_periods_and_lines_and_goes_landscape_from_six_columns() {
    run(async {
        let f = fixture().await;
        let mut tx = f.orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let name: String = tx
            .query("SELECT name FROM res_company WHERE id = $1", &[&company])
            .await
            .unwrap()[0]
            .get(0);
        drop(tx);
        let unfolded = with(november(f.stock), "unfold_all", json!(true));
        let prepared = prepared(&f, f.clerk, f.stock, unfolded, "pdf")
            .await
            .unwrap();
        assert!(!prepared.landscape());
        let (texts, boxes) = text_of_pdf(&pdf_of(&prepared));
        let page = texts.concat();
        let compact = |t: &str| -> String { t.chars().filter(|c| !c.is_whitespace()).collect() };
        for want in [
            &name,
            "Fixture Stock",
            "2026-11-01",
            "2026-11-30",
            "Quantity",
            "Available",
            "OnHand",
            "B01",
            "P1",
        ] {
            assert!(page.contains(&compact(want)), "{want} in {page}");
        }
        assert!(
            !page.contains("Secret"),
            "the clerk may not read it: {page}"
        );
        assert_eq!(
            boxes[0][2..],
            [595.0, 842.0],
            "five columns or fewer print portrait"
        );

        // a comparison of two earlier periods makes nine columns: landscape
        let wide = with(
            november(f.stock),
            "comparison",
            json!({"mode": "previous_period", "periods": 2}),
        );
        let wide = prepared_for(&f, wide).await;
        assert!(wide.landscape());
        let (_, boxes) = text_of_pdf(&pdf_of(&wide));
        assert_eq!(boxes[0][2..], [842.0, 595.0]);
        // the second group's period is in the header
        assert_eq!(wide.file_name(), "Fixture Stock.pdf");
    });
}

async fn prepared_for(f: &Fixture, options: Json) -> export::Prepared {
    prepared(f, f.clerk, f.stock, options, "pdf").await.unwrap()
}

#[test]
fn an_export_is_computed_as_the_caller() {
    run(async {
        let f = fixture().await;
        let error = prepared(&f, f.clerk, f.secret, november(f.secret), "xlsx")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access, "{}", error.message());
        // the clerk's access row hides the restricted item: the file holds what the clerk may read
        let all = with(november(f.stock), "unfold_all", json!(true));
        let book = prepared(&f, f.clerk, f.stock, all.clone(), "xlsx")
            .await
            .unwrap()
            .xlsx()
            .unwrap();
        let cells = sheet_cells(&book.bytes, "xl/worksheets/sheet1.xml");
        assert!(
            first_column(&cells, 3)
                .iter()
                .all(|n| n != "PX" && n != "Secret")
        );
        let admin = prepared(&f, f.admin, f.stock, all, "xlsx")
            .await
            .unwrap()
            .xlsx()
            .unwrap();
        assert!(
            first_column(&sheet_cells(&admin.bytes, "xl/worksheets/sheet1.xml"), 3)
                .iter()
                .any(|n| n == "PX")
        );
        // a request that posts rows is refused, never trusted
        let mut tx = f.orm.begin(f.admin).await.unwrap();
        let body =
            json!({"report": f.stock, "options": november(f.stock), "format": "pdf", "lines": []});
        let refused = export::prepare(&mut tx, body.to_string().as_bytes())
            .await
            .unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Validation);
    });
}
