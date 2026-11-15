// SPDX-License-Identifier: MIT OR Apache-2.0
//! Report actions and paper formats of addon `base` (
//! `paperformat-layouts.md`): the two models, their
//! defaults and rules, the paper format resolution, the Print menu binding, and the template
//! check that runs when a report action and its QWeb view are both in the database.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

mod support;

use std::sync::Arc;

use erp_addon_base::ir_actions::{check_loaded_reports, paperformat_of, report_from_name};
use erp_addon_base::report_paperformat::{CUSTOM_NEEDS_SIZE, FORMAT_OR_SIZE, PAPER_SIZES};
use erp_sdk::{Cell, Error, ErrorKind, RawVals, Records, Value};
use erp_testkit::Harness;
use support::rows;

const REPORT: &str = "ir.actions.report";
const PAPER: &str = "report.paperformat";

async fn harness() -> Harness {
    Harness::builder([&erp_addon_base::ADDON, &erp_data::model_data::ADDON])
        .with_decls([erp_data::ir_model_data_decl()])
        .build()
        .await
        .expect("harness")
}

fn s(text: &str) -> Cell {
    Cell::Str(Arc::from(text))
}

fn vals(pairs: &[(&'static str, Cell)]) -> RawVals {
    pairs
        .iter()
        .fold(RawVals::new(), |v, (k, c)| v.set_cell(*k, c.clone()))
}

async fn create(
    tx: &mut erp_sdk::Transaction,
    model: &str,
    v: RawVals,
) -> erp_sdk::Result<Records> {
    let records = tx.env().records(model)?;
    records.create(tx, vec![v]).await
}

async fn create_one(tx: &mut erp_sdk::Transaction, model: &str, v: RawVals) -> i64 {
    create(tx, model, v).await.unwrap().ids()[0]
}

/// A report action created while a data file loads (`install_mode`, the loader's context key).
async fn create_loading(tx: &mut erp_sdk::Transaction, v: RawVals) -> erp_sdk::Result<Records> {
    let records = tx
        .env()
        .with_context([("install_mode", Value::Bool(true))])
        .records(REPORT)?;
    records.create(tx, vec![v]).await
}

fn browse(tx: &erp_sdk::Transaction, model: &str, id: i64) -> Records {
    tx.env().records(model).unwrap().browse([id]).unwrap()
}

async fn cell(tx: &mut erp_sdk::Transaction, model: &str, id: i64, field: &str) -> Cell {
    browse(tx, model, id).cells(tx, field).await.unwrap()[0].clone()
}

fn assert_error(result: erp_sdk::Result<impl std::fmt::Debug>, kind: ErrorKind, message: &str) {
    let error: Error = result.expect_err(message);
    assert_eq!(error.kind(), kind, "{error}");
    assert_eq!(error.message(), message);
}

/// A QWeb view with the key `key` and the body `body` under a `t` root.
async fn template(
    tx: &mut erp_sdk::Transaction,
    key: &str,
    body: &str,
) -> erp_sdk::Result<Records> {
    create(
        tx,
        "ir.ui.view",
        vals(&[
            ("name", s(key)),
            ("type", s("qweb")),
            ("key", s(key)),
            ("arch_db", s(&format!("<t t-name=\"{key}\">{body}</t>"))),
        ]),
    )
    .await
}

fn report_vals(name: &str, model: &str, template: &str) -> RawVals {
    vals(&[
        ("name", s(name)),
        ("model", s(model)),
        ("report_name", s(template)),
    ])
}

/// The `ir_model` row of `model` (the reflection is not filled by the harness).
async fn ir_model(tx: &mut erp_sdk::Transaction, model: &str) -> i64 {
    let rows = tx
        .query(
            "INSERT INTO ir_model (name, model, state, transient) VALUES ($1, $1, 'base', false) \
             RETURNING id",
            &[&model],
        )
        .await
        .unwrap();
    rows[0].get(0)
}

/// A report created with a name, a model and a template: PDF, not reused, not `multi`, bound as
/// `report`, no paper format; it is an action of the shared id space with its key row.
#[tokio::test]
async fn report_defaults_and_key_row() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    template(&mut tx, "test.report_groups", "<p>Groups</p>")
        .await
        .unwrap();
    let id = create_one(
        &mut tx,
        REPORT,
        report_vals("Groups", "res.groups", "test.report_groups"),
    )
    .await;
    assert_eq!(
        cell(&mut tx, REPORT, id, "report_type").await,
        s("qweb-pdf")
    );
    // booleans without a default have no value in the row until written
    assert_eq!(
        cell(&mut tx, REPORT, id, "attachment_use").await,
        Cell::Null
    );
    assert_eq!(cell(&mut tx, REPORT, id, "multi").await, Cell::Null);
    assert_eq!(cell(&mut tx, REPORT, id, "binding_type").await, s("report"));
    assert_eq!(
        cell(&mut tx, REPORT, id, "paperformat_id").await,
        Cell::Null
    );
    assert_eq!(cell(&mut tx, REPORT, id, "type").await, s(REPORT));
    tx.flush().await.unwrap();
    assert_eq!(
        rows(
            &mut tx,
            &format!("SELECT type, binding_type, name FROM ir_actions WHERE id = {id}")
        )
        .await,
        vec![vec![
            Some(REPORT.to_owned()),
            Some("report".to_owned()),
            Some("Groups".to_owned())
        ]]
    );
    // deleting the report deletes its key row (the follow-delete trigger)
    browse(&tx, REPORT, id).unlink(&mut tx).await.unwrap();
    tx.flush().await.unwrap();
    assert_eq!(
        rows(
            &mut tx,
            &format!("SELECT count(*)::text FROM ir_actions WHERE id = {id}")
        )
        .await,
        vec![vec![Some("0".to_owned())]]
    );
    tx.rollback().await.unwrap();
}

/// The three report types are accepted; another value is refused by the selection.
#[tokio::test]
async fn report_type_is_one_of_three() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    template(&mut tx, "test.report_types", "<p>Types</p>")
        .await
        .unwrap();
    for kind in ["qweb-pdf", "qweb-html", "qweb-text"] {
        let id = create_one(
            &mut tx,
            REPORT,
            report_vals(kind, "res.groups", "test.report_types").set_cell("report_type", s(kind)),
        )
        .await;
        assert_eq!(cell(&mut tx, REPORT, id, "report_type").await, s(kind));
    }
    assert_error(
        create(
            &mut tx,
            REPORT,
            report_vals("Other", "res.groups", "test.report_types")
                .set_cell("report_type", s("qweb-xml")),
        )
        .await,
        ErrorKind::Validation,
        "Wrong value for ir.actions.report.report_type: 'qweb-xml'",
    );
    tx.rollback().await.unwrap();
}

/// The checks of the values that need no template: the model exists, the two expressions parse,
/// and a report saved as attachment has an attachment name. Each refusal names the report.
#[tokio::test]
async fn report_values_are_checked_by_name() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    template(&mut tx, "test.report_checked", "<p>Checked</p>")
        .await
        .unwrap();
    assert_error(
        create(
            &mut tx,
            REPORT,
            report_vals("No model", "no.such.model", "test.report_checked"),
        )
        .await,
        ErrorKind::Validation,
        "Report 'No model' prints model 'no.such.model', which is not a model of this database",
    );
    for field in ["print_report_name", "attachment"] {
        let error = create(
            &mut tx,
            REPORT,
            report_vals("Bad name", "res.groups", "test.report_checked")
                .set_cell(field, s("'Order ' + (object.name")),
        )
        .await
        .expect_err(field);
        assert_eq!(error.kind(), ErrorKind::Validation, "{error}");
        assert!(
            error.message().starts_with(&format!(
                "Report 'Bad name': {field} is not a valid expression: "
            )),
            "{}",
            error.message()
        );
    }
    assert_error(
        create(
            &mut tx,
            REPORT,
            report_vals("Kept", "res.groups", "test.report_checked")
                .set_cell("save_as_attachment", Cell::Bool(true)),
        )
        .await,
        ErrorKind::Validation,
        "Report 'Kept' is saved as attachment and needs an attachment name",
    );
    // with a name it is stored, and clearing the name afterwards is refused the same way
    let id = create_one(
        &mut tx,
        REPORT,
        report_vals("Kept", "res.groups", "test.report_checked")
            .set_cell("save_as_attachment", Cell::Bool(true))
            .set_cell("attachment", s("'Group ' + object.name + '.pdf'")),
    )
    .await;
    assert_error(
        browse(&tx, REPORT, id)
            .write(&mut tx, RawVals::new().set_cell("attachment", Cell::Null))
            .await,
        ErrorKind::Validation,
        "Report 'Kept' is saved as attachment and needs an attachment name",
    );
    tx.rollback().await.unwrap();
}

/// `report_action`: the client action with the report's type, name, template and output type,
/// `active_ids` in the context when there are records, and `data` as given.
#[tokio::test]
async fn report_action_dictionary() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    template(&mut tx, "test.report_action", "<p>Action</p>")
        .await
        .unwrap();
    let id = create_one(
        &mut tx,
        REPORT,
        report_vals("Action", "res.groups", "test.report_action"),
    )
    .await;
    let report = browse(&tx, REPORT, id);
    let action = report
        .call::<(Vec<i64>, Option<Value>), Value>(&mut tx, "report_action", (vec![10, 11], None))
        .await
        .unwrap();
    let Value::Map(action) = action else {
        panic!("report_action returns a map: {action:?}");
    };
    assert_eq!(action.get("type"), Some(&Value::str(REPORT)));
    assert_eq!(action.get("name"), Some(&Value::str("Action")));
    assert_eq!(
        action.get("report_name"),
        Some(&Value::str("test.report_action"))
    );
    assert_eq!(action.get("report_type"), Some(&Value::str("qweb-pdf")));
    assert_eq!(action.get("data"), Some(&Value::False));
    let Some(Value::Map(context)) = action.get("context") else {
        panic!("context is a map: {action:?}");
    };
    assert_eq!(
        context.get("active_ids"),
        Some(&Value::List(vec![Value::Int(10), Value::Int(11)].into()))
    );

    let data = Value::Map(Arc::new(
        [("filter".to_owned(), Value::str("unpaid"))]
            .into_iter()
            .collect(),
    ));
    let action = report
        .call::<(Vec<i64>, Option<Value>), Value>(
            &mut tx,
            "report_action",
            (Vec::new(), Some(data.clone())),
        )
        .await
        .unwrap();
    let Value::Map(action) = action else {
        panic!("report_action returns a map: {action:?}");
    };
    assert_eq!(action.get("data"), Some(&data));
    let Some(Value::Map(context)) = action.get("context") else {
        panic!("context is a map: {action:?}");
    };
    assert_eq!(context.get("active_ids"), None, "no records, no active_ids");
    tx.rollback().await.unwrap();
}

/// A report is found by its template name; an unknown name finds none.
#[tokio::test]
async fn report_is_found_by_template_name() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    template(&mut tx, "test.report_named", "<p>Named</p>")
        .await
        .unwrap();
    let first = create_one(
        &mut tx,
        REPORT,
        report_vals("Zeta", "res.groups", "test.report_named"),
    )
    .await;
    let _second = create_one(
        &mut tx,
        REPORT,
        report_vals("Alpha", "res.groups", "test.report_named"),
    )
    .await;
    assert_eq!(
        report_from_name(&mut tx, "test.report_named")
            .await
            .unwrap(),
        Some(first)
    );
    assert_eq!(
        report_from_name(&mut tx, "test.report_unknown")
            .await
            .unwrap(),
        None
    );
    tx.rollback().await.unwrap();
}

/// "Offer when printing" binds the report to its model: it is in the `report` bindings of the
/// model, and "Stop offering when printing" takes it out again.
#[tokio::test]
async fn report_binds_to_the_print_menu() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let model = ir_model(&mut tx, "res.groups").await;
    template(&mut tx, "test.report_bound", "<p>Bound</p>")
        .await
        .unwrap();
    let id = create_one(
        &mut tx,
        REPORT,
        report_vals("Bound", "res.groups", "test.report_bound"),
    )
    .await;
    let env = tx.env();
    let before = erp_addon_base::ir_actions::get_bindings(&mut tx, &env, "res.groups")
        .await
        .unwrap();
    assert!(before.report().is_empty());

    let report = browse(&tx, REPORT, id);
    report
        .call::<(), Value>(&mut tx, "create_action", ())
        .await
        .unwrap();
    assert_eq!(
        cell(&mut tx, REPORT, id, "binding_model_id").await,
        Cell::Id(model)
    );
    let bound = erp_addon_base::ir_actions::get_bindings(&mut tx, &env, "res.groups")
        .await
        .unwrap();
    let ids: Vec<i64> = bound.report().iter().map(|b| b.id().get()).collect();
    assert_eq!(ids, vec![id]);
    assert!(
        bound.action().is_empty(),
        "a report is not an action binding"
    );
    assert_eq!(
        bound.report()[0].values().get("name"),
        Some(&Value::str("Bound"))
    );

    report
        .call::<(), Value>(&mut tx, "unlink_action", ())
        .await
        .unwrap();
    let after = erp_addon_base::ir_actions::get_bindings(&mut tx, &env, "res.groups")
        .await
        .unwrap();
    assert!(after.report().is_empty());

    // a report on a model without an ir.model row cannot be bound, by name
    let other = create_one(
        &mut tx,
        REPORT,
        report_vals("Unbound", "res.partner", "test.report_bound"),
    )
    .await;
    assert_error(
        browse(&tx, REPORT, other)
            .call::<(), Value>(&mut tx, "create_action", ())
            .await,
        ErrorKind::Validation,
        "Report 'Unbound' prints model 'res.partner', which has no ir.model row",
    );
    tx.rollback().await.unwrap();
}

/// A paper format created with a name only has the specified defaults.
#[tokio::test]
async fn paperformat_defaults() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let id = create_one(&mut tx, PAPER, vals(&[("name", s("Custom Format"))])).await;
    assert_eq!(cell(&mut tx, PAPER, id, "format").await, s("A4"));
    assert_eq!(
        cell(&mut tx, PAPER, id, "margin_top").await,
        Cell::Float(40.0)
    );
    assert_eq!(
        cell(&mut tx, PAPER, id, "margin_bottom").await,
        Cell::Float(20.0)
    );
    assert_eq!(
        cell(&mut tx, PAPER, id, "margin_left").await,
        Cell::Float(7.0)
    );
    assert_eq!(
        cell(&mut tx, PAPER, id, "margin_right").await,
        Cell::Float(7.0)
    );
    assert_eq!(
        cell(&mut tx, PAPER, id, "orientation").await,
        s("Landscape")
    );
    assert_eq!(
        cell(&mut tx, PAPER, id, "header_spacing").await,
        Cell::Int(35)
    );
    assert_eq!(cell(&mut tx, PAPER, id, "dpi").await, Cell::Int(90));
    // the field has no default: an unset boolean has no value in the row
    assert_eq!(cell(&mut tx, PAPER, id, "default").await, Cell::Null);
    tx.rollback().await.unwrap();
}

/// The printed page size of a record: a standard size, a custom size, and the swap in landscape;
/// every key of the selection but `custom` has a size.
#[tokio::test]
async fn paperformat_print_page_size() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let size = async |tx: &mut erp_sdk::Transaction, id: i64| {
        (
            cell(tx, PAPER, id, "print_page_width").await,
            cell(tx, PAPER, id, "print_page_height").await,
        )
    };
    let a4 = create_one(
        &mut tx,
        PAPER,
        vals(&[("name", s("A4 portrait")), ("orientation", s("Portrait"))]),
    )
    .await;
    assert_eq!(
        size(&mut tx, a4).await,
        (Cell::Float(210.0), Cell::Float(297.0))
    );
    browse(&tx, PAPER, a4)
        .write(
            &mut tx,
            RawVals::new().set_cell("orientation", s("Landscape")),
        )
        .await
        .unwrap();
    assert_eq!(
        size(&mut tx, a4).await,
        (Cell::Float(297.0), Cell::Float(210.0))
    );
    let letter = create_one(
        &mut tx,
        PAPER,
        vals(&[
            ("name", s("Letter")),
            ("format", s("Letter")),
            ("orientation", s("Portrait")),
        ]),
    )
    .await;
    assert_eq!(
        size(&mut tx, letter).await,
        (Cell::Float(215.9), Cell::Float(279.4))
    );
    let custom = create_one(
        &mut tx,
        PAPER,
        vals(&[
            ("name", s("Label")),
            ("format", s("custom")),
            ("page_width", Cell::Int(100)),
            ("page_height", Cell::Int(150)),
            ("orientation", s("Portrait")),
        ]),
    )
    .await;
    assert_eq!(
        size(&mut tx, custom).await,
        (Cell::Float(100.0), Cell::Float(150.0))
    );

    // the selection and the size table name the same formats
    let spec = h.registry().model(PAPER).unwrap();
    let erp_fields::FieldType::Selection {
        values: Some(values),
        ..
    } = &spec.field("format").unwrap().ty
    else {
        panic!("format is a static selection");
    };
    let keys: Vec<&str> = values.iter().map(|(key, _)| key.as_str()).collect();
    let mut sized: Vec<&str> = PAPER_SIZES.iter().map(|size| size.key).collect();
    sized.push("custom");
    assert_eq!(keys, sized);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn paperformat_format_or_size() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    for (field, value) in [("page_width", 100), ("page_height", 200)] {
        assert_error(
            create(
                &mut tx,
                PAPER,
                vals(&[
                    ("name", s("Both")),
                    ("format", s("A4")),
                    (field, Cell::Int(value)),
                ]),
            )
            .await,
            ErrorKind::Validation,
            FORMAT_OR_SIZE,
        );
    }
    let id = create_one(&mut tx, PAPER, vals(&[("name", s("Plain A4"))])).await;
    assert_error(
        browse(&tx, PAPER, id)
            .write(
                &mut tx,
                RawVals::new().set_cell("page_width", Cell::Int(100)),
            )
            .await,
        ErrorKind::Validation,
        FORMAT_OR_SIZE,
    );
    assert_error(
        create(
            &mut tx,
            PAPER,
            vals(&[
                ("name", s("Half custom")),
                ("format", s("custom")),
                ("page_width", Cell::Int(100)),
            ]),
        )
        .await,
        ErrorKind::Validation,
        CUSTOM_NEEDS_SIZE,
    );
    tx.rollback().await.unwrap();
}

/// The paper format of a report: its own, else the company's, else the installed default
/// `base.paperformat_euro`; with none of the three the refusal names the report.
#[tokio::test]
async fn paperformat_resolution_tiers() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    template(&mut tx, "test.report_paper", "<p>Paper</p>")
        .await
        .unwrap();
    let report = create_one(
        &mut tx,
        REPORT,
        report_vals("Paper", "res.groups", "test.report_paper"),
    )
    .await;
    let currency = create_one(
        &mut tx,
        "res.currency",
        vals(&[("name", s("TST")), ("symbol", s("T"))]),
    )
    .await;
    let company = create_one(
        &mut tx,
        "res.company",
        vals(&[("name", s("Paper Co")), ("currency_id", Cell::Id(currency))]),
    )
    .await;

    // none of the three
    assert_error(
        paperformat_of(&mut tx, report, Some(company)).await,
        ErrorKind::Validation,
        "Report 'Paper' has no paper format: neither the report nor the company names one, and \
         the default format base.paperformat_euro does not exist",
    );

    // tier 4: the installed default
    let euro = create_one(
        &mut tx,
        PAPER,
        vals(&[("name", s("A4")), ("orientation", s("Portrait"))]),
    )
    .await;
    tx.execute(
        "INSERT INTO ir_model_data (module, name, model, res_id, noupdate) \
         VALUES ('base', 'paperformat_euro', 'report.paperformat', $1, true)",
        &[&euro],
    )
    .await
    .unwrap();
    assert_eq!(
        paperformat_of(&mut tx, report, Some(company))
            .await
            .unwrap(),
        euro
    );
    assert_eq!(paperformat_of(&mut tx, report, None).await.unwrap(), euro);

    // tier 3: the company's
    let us = create_one(
        &mut tx,
        PAPER,
        vals(&[
            ("name", s("US Letter")),
            ("format", s("Letter")),
            ("orientation", s("Portrait")),
        ]),
    )
    .await;
    browse(&tx, "res.company", company)
        .write(
            &mut tx,
            RawVals::new().set_cell("paperformat_id", Cell::Id(us)),
        )
        .await
        .unwrap();
    assert_eq!(
        paperformat_of(&mut tx, report, Some(company))
            .await
            .unwrap(),
        us
    );

    // tier 2: the report's own, whatever the company's
    let deposit = create_one(
        &mut tx,
        PAPER,
        vals(&[
            ("name", s("US Batch Deposit")),
            ("format", s("Letter")),
            ("margin_top", Cell::Float(15.0)),
        ]),
    )
    .await;
    browse(&tx, REPORT, report)
        .write(
            &mut tx,
            RawVals::new().set_cell("paperformat_id", Cell::Id(deposit)),
        )
        .await
        .unwrap();
    assert_eq!(
        paperformat_of(&mut tx, report, Some(company))
            .await
            .unwrap(),
        deposit
    );

    let error = paperformat_of(&mut tx, 2_000_000_000, None)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Missing);
    assert_eq!(
        error.message(),
        "ir.actions.report 2000000000 does not exist"
    );
    tx.rollback().await.unwrap();
}

/// refusal at print is)
///
/// The template of a PDF report is checked against the print profile when the report is created
/// and when the template is written, and the refusal names the report, the template, the line
/// and the token. A report whose template is not in the database is refused outside a load.
#[tokio::test]
async fn template_is_checked_when_report_or_template_is_written() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();

    // the template first, then the report
    template(&mut tx, "test.report_bad", "<p onclick=\"x\">Bad</p>")
        .await
        .unwrap();
    assert_error(
        create(
            &mut tx,
            REPORT,
            report_vals("Bad", "res.groups", "test.report_bad"),
        )
        .await,
        ErrorKind::Validation,
        "Report 'Bad' (test.report_bad): test.report_bad:1: attribute `onclick` is not in print \
         profile 1; see the addon guide page on reports",
    );
    // a text report has no print profile: the same template is accepted
    create_one(
        &mut tx,
        REPORT,
        report_vals("Bad as text", "res.groups", "test.report_bad")
            .set_cell("report_type", s("qweb-text")),
    )
    .await;

    // no template at all
    assert_error(
        create(
            &mut tx,
            REPORT,
            report_vals("Nowhere", "res.groups", "test.report_nowhere"),
        )
        .await,
        ErrorKind::Validation,
        "Report 'Nowhere' (test.report_nowhere): template 'test.report_nowhere' is not an active \
         QWeb view of this database",
    );

    // the report first (a valid template), then the template is changed into an invalid one
    let good = template(&mut tx, "test.report_good", "<p class=\"fw-bold\">Good</p>")
        .await
        .unwrap();
    let report = create_one(
        &mut tx,
        REPORT,
        report_vals("Good", "res.groups", "test.report_good"),
    )
    .await;
    assert_error(
        good.write(
            &mut tx,
            RawVals::new().set_cell(
                "arch_db",
                s("<t t-name=\"test.report_good\"><p invented=\"x\">Good</p></t>"),
            ),
        )
        .await,
        ErrorKind::Validation,
        "Report 'Good' (test.report_good): test.report_good:1: attribute `invented` is not in \
         print profile 1; see the addon guide page on reports",
    );
    // and pointing the report at the invalid template is refused the same way
    assert_error(
        browse(&tx, REPORT, report)
            .write(
                &mut tx,
                RawVals::new().set_cell("report_name", s("test.report_bad")),
            )
            .await,
        ErrorKind::Validation,
        "Report 'Good' (test.report_bad): test.report_bad:1: attribute `onclick` is not in print \
         profile 1; see the addon guide page on reports",
    );
    tx.rollback().await.unwrap();
}

/// The template a report calls by a literal `t-call` is checked with it: a called template
/// outside the profile is refused under the calling report's name, and a called template that
/// does not exist is refused by name.
#[tokio::test]
async fn called_templates_are_checked_with_the_report() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    template(&mut tx, "test.part_bad", "<p onclick=\"x\">Part</p>")
        .await
        .unwrap();
    template(
        &mut tx,
        "test.report_calls_bad",
        "<t t-call=\"test.part_bad\"/>",
    )
    .await
    .unwrap();
    assert_error(
        create(
            &mut tx,
            REPORT,
            report_vals("Caller", "res.groups", "test.report_calls_bad"),
        )
        .await,
        ErrorKind::Validation,
        "Report 'Caller' (test.report_calls_bad): test.part_bad:1: attribute `onclick` is not in \
         print profile 1; see the addon guide page on reports",
    );
    template(
        &mut tx,
        "test.report_calls_missing",
        "<t t-call=\"test.part_missing\"/>",
    )
    .await
    .unwrap();
    assert_error(
        create(
            &mut tx,
            REPORT,
            report_vals("Caller", "res.groups", "test.report_calls_missing"),
        )
        .await,
        ErrorKind::Validation,
        "Report 'Caller' (test.report_calls_missing): template 'test.part_missing' is not an \
         active QWeb view of this database",
    );
    tx.rollback().await.unwrap();
}

/// While a data file loads, a report may come before its template: nothing is refused until the
/// final check, which names what is still missing; a template that then loads is checked at once.
#[tokio::test]
async fn loading_defers_a_missing_template_to_the_final_check() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    // nothing was written while loading: the final check reads nothing and passes
    let before = tx.query_count();
    check_loaded_reports(&mut tx).await.unwrap();
    assert_eq!(tx.query_count() - before, 0, "no load, no statement");

    create_loading(
        &mut tx,
        report_vals("Later", "res.groups", "test.report_later"),
    )
    .await
    .unwrap();
    assert_error(
        check_loaded_reports(&mut tx).await,
        ErrorKind::Validation,
        "Report 'Later' (test.report_later): template 'test.report_later' is not an active QWeb \
         view of this database",
    );
    // the template arrives, invalid: refused when it is written, under the report's name
    assert_error(
        template(&mut tx, "test.report_later", "<p onclick=\"x\">Later</p>").await,
        ErrorKind::Validation,
        "Report 'Later' (test.report_later): test.report_later:1: attribute `onclick` is not in \
         print profile 1; see the addon guide page on reports",
    );
    tx.rollback().await.unwrap();

    let mut tx = h.begin().await.unwrap();
    create_loading(
        &mut tx,
        report_vals("Later", "res.groups", "test.report_later"),
    )
    .await
    .unwrap();
    template(&mut tx, "test.report_later", "<p>Later</p>")
        .await
        .unwrap();
    check_loaded_reports(&mut tx).await.unwrap();
    tx.rollback().await.unwrap();
}

/// A report with an attachment expression whose template prints a field limited to groups is
/// refused until the action acknowledges it; a field the registry does not know is refused
/// whatever the acknowledgement.
#[tokio::test]
async fn attachment_reports_check_group_restricted_fields() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    template(
        &mut tx,
        "test.report_keys",
        "<t t-foreach=\"docs\" t-as=\"doc\"><p t-field=\"doc.legacy_hash\"/></t>",
    )
    .await
    .unwrap();
    let keys = |name: &str| {
        report_vals(name, "res.users.apikeys", "test.report_keys")
            .set_cell("attachment", s("'Key ' + object.kind + '.pdf'"))
    };
    // without an attachment expression nothing is kept, so nothing is checked
    create_one(
        &mut tx,
        REPORT,
        report_vals("Keys printed", "res.users.apikeys", "test.report_keys"),
    )
    .await;
    assert_error(
        create(&mut tx, REPORT, keys("Keys kept")).await,
        ErrorKind::Validation,
        "report Keys kept: test.report_keys:1 prints restricted field \
         res.users.apikeys.legacy_hash; set allow_attachment_group_fields explicitly to \
         acknowledge the first printer's field view",
    );
    let acknowledged = create_one(
        &mut tx,
        REPORT,
        keys("Keys acknowledged").set_cell("allow_attachment_group_fields", Cell::Bool(true)),
    )
    .await;
    // withdrawing the acknowledgement is refused like the create without it
    assert_error(
        browse(&tx, REPORT, acknowledged)
            .write(
                &mut tx,
                RawVals::new().set_cell("allow_attachment_group_fields", Cell::Bool(false)),
            )
            .await,
        ErrorKind::Validation,
        "report Keys acknowledged: test.report_keys:1 prints restricted field \
         res.users.apikeys.legacy_hash; set allow_attachment_group_fields explicitly to \
         acknowledge the first printer's field view",
    );

    template(
        &mut tx,
        "test.report_unknown_field",
        "<t t-foreach=\"docs\" t-as=\"doc\"><p t-field=\"doc.no_such_field\"/></t>",
    )
    .await
    .unwrap();
    assert_error(
        create(
            &mut tx,
            REPORT,
            report_vals("Unknown", "res.users.apikeys", "test.report_unknown_field")
                .set_cell("attachment", s("'Key.pdf'"))
                .set_cell("allow_attachment_group_fields", Cell::Bool(true)),
        )
        .await,
        ErrorKind::Validation,
        "report Unknown: test.report_unknown_field:1 cannot resolve `doc.no_such_field` for \
         attachment field validation",
    );
    tx.rollback().await.unwrap();
}
