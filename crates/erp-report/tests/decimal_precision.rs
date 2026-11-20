// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
use erp_report::{Language, PlanContext, ReportError, ReportLimits, ReportProgram, TemplateSource};

fn compile(options: &str) -> ReportProgram {
    let arch = format!("<span t-out='1.25' t-options=\"{options}\"/>");
    ReportProgram::compile(
        "precision.report",
        &[TemplateSource::new("precision.report", &arch)],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap()
}

#[test]
fn nonliteral_precision_is_rejected_by_the_plan() {
    let program = compile("{'widget': 'float', 'decimal_precision': precision_name}");
    let err = program
        .prefetch_plan(&PlanContext::new("x.doc"), &|_, _| None)
        .unwrap_err();
    assert!(matches!(err, ReportError::Compile { .. }), "{err}");
    assert!(err.to_string().contains("decimal_precision"), "{err}");
    assert!(err.to_string().contains("literal"), "{err}");
}

#[test]
fn null_precision_is_not_silently_ignored() {
    let program = compile("{'widget': 'float', 'decimal_precision': None}");
    let err = program
        .prefetch_plan(&PlanContext::new("x.doc"), &|_, _| None)
        .unwrap_err();
    assert!(matches!(err, ReportError::Compile { .. }), "{err}");
    assert!(err.to_string().contains("decimal_precision"), "{err}");
    assert!(err.to_string().contains("literal"), "{err}");
}

use erp_expr::{EvalContext, ExprValue};
use erp_report::{
    DataStage, DocumentOptions, EvalOptions, FieldInfo, FieldValue, LangFormat, Limits,
    LoadedPrecision, LoadedRecord, Node, PageSetup, RecordStore,
};
use std::sync::Arc;

fn make_plan(program: &ReportProgram) -> erp_report::PrefetchPlan {
    program
        .prefetch_plan(&PlanContext::new("x.doc"), &|_, field| {
            (field == "weight").then(|| FieldInfo::scalar(false))
        })
        .unwrap()
}
fn make_stage(plan: &erp_report::PrefetchPlan) -> DataStage<'_> {
    DataStage::new(
        plan,
        &[("docs", &[])],
        ReportLimits::default(),
        Limits::default(),
    )
    .unwrap()
}
fn precision(name: &str, digits: i64) -> LoadedPrecision {
    LoadedPrecision::new(name, digits).unwrap()
}
fn rendered(
    program: &ReportProgram,
    store: RecordStore,
    options: &EvalOptions,
    vars: &[(&str, ExprValue)],
) -> Result<String, ReportError> {
    let store = Arc::new(store);
    let mut context = EvalContext::standard().with_var("docs", store.recordset("x.doc", &[1]));
    for (key, value) in vars {
        context = context.with_var(*key, value.clone());
    }
    let document = program.evaluate(
        &context,
        &store,
        options,
        DocumentOptions::new(
            "precision",
            "precision",
            PageSetup::new(595.0, 842.0, 0.0).unwrap(),
        ),
    )?;
    Ok(document
        .nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect())
}
fn data_error(err: ReportError, reason: &str) {
    assert!(matches!(err, ReportError::Data { .. }), "{err}");
    assert_eq!(err.to_string(), format!("report data: {reason}"));
}

#[test]
fn named_precision_formats_fields_and_values_with_precedence() {
    let arch = "<p><t t-foreach='docs' t-as='doc'><span t-field='doc.weight' t-options=\"{'decimal_precision': 'Product Unit', 'precision': 1}\"/> <span t-out='4500.1267' t-options=\"{'widget': 'float', 'decimal_precision': 'Product Unit', 'precision': 0}\"/></t></p>";
    let program = ReportProgram::compile(
        "precision.report",
        &[TemplateSource::new("precision.report", arch)],
        &Language::new("de_DE").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    let field_plan = make_plan(&program);
    let mut field_stage = DataStage::new(
        &field_plan,
        &[("docs", &[1])],
        ReportLimits::default(),
        Limits::default(),
    )
    .unwrap();
    field_stage
        .supply_precisions(vec![precision("Product Unit", 3)])
        .unwrap();
    field_stage
        .supply(vec![
            LoadedRecord::new(1).with_field("weight", FieldValue::float(4500.1267, Some(2))),
        ])
        .unwrap();
    let options = EvalOptions::default()
        .with_formats(LangFormat::new(",", ".", &[3, 0], "%d.%m.%Y", "%H:%M:%S").unwrap());
    assert_eq!(
        rendered(&program, field_stage.finish().unwrap(), &options, &[]).unwrap(),
        "4.500,127 4.500,127"
    );
    for (digits, extra, expected) in [
        (0, "", "1"),
        (3, "", "1.250"),
        (3, ", 'min_precision': 1", "1.25"),
        (28, "", "1.2500000000000"),
    ] {
        let program = compile(&format!(
            "{{'widget': 'float', 'decimal_precision': 'Product Unit'{extra}}}"
        ));
        let plan = make_plan(&program);
        let mut stage = make_stage(&plan);
        stage
            .supply_precisions(vec![precision("Product Unit", digits)])
            .unwrap();
        assert_eq!(
            rendered(
                &program,
                stage.finish().unwrap(),
                &EvalOptions::default(),
                &[]
            )
            .unwrap(),
            expected
        );
    }
}

#[test]
fn invalid_and_ambiguous_precision_declarations_fail_by_expression() {
    for options in [
        "{'decimal_precision': False}",
        "{'decimal_precision': 3}",
        "{'decimal_precision': ''}",
        "{'decimal_precision': 'Unit', 'decimal_precision': 'Unit'}",
        "{'decimal_precision': 'Unit', key: 1}",
    ] {
        let program = compile(options);
        let err = program
            .prefetch_plan(&PlanContext::new("x.doc"), &|_, _| None)
            .unwrap_err();
        assert!(matches!(err, ReportError::Compile { .. }), "{err}");
        assert!(err.to_string().starts_with("precision.report:1: "), "{err}");
        assert!(err.to_string().contains(options), "{err}");
    }
    let name = "é".repeat(65);
    let options = format!("{{'decimal_precision': '{name}'}}");
    let err = compile(&options)
        .prefetch_plan(&PlanContext::new("x.doc"), &|_, _| None)
        .unwrap_err();
    assert!(matches!(err, ReportError::Compile { .. }), "{err}");
    assert!(err.to_string().contains("1 to 128 bytes"), "{err}");
}

#[test]
fn dynamic_precision_cannot_reuse_another_literal_occurrence() {
    let arch = "<p><span t-out='1.25' t-options=\"{'widget': 'float', 'decimal_precision': 'Unit'}\"/><t t-set='opts' t-value=\"{'widget': 'float', 'decimal_precision': 'Unit'}\"/><span t-out='1.25' t-options='opts'/></p>";
    let program = ReportProgram::compile(
        "precision.report",
        &[TemplateSource::new("precision.report", arch)],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    let plan = make_plan(&program);
    let mut stage = make_stage(&plan);
    stage.supply_precisions(vec![precision("Unit", 3)]).unwrap();
    let err = rendered(
        &program,
        stage.finish().unwrap(),
        &EvalOptions::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(err, ReportError::Evaluate { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "precision.report:1: `opts`: widget option `decimal_precision` requires a literal string in a static options dictionary"
    );
    let arch = "<p><t t-set='opts' t-value=\"{'widget': 'float', 'precision': 2}\"/><span t-out='1.25' t-options='opts'/></p>";
    let program = ReportProgram::compile(
        "precision.report",
        &[TemplateSource::new("precision.report", arch)],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    assert_eq!(
        rendered(&program, RecordStore::new(), &EvalOptions::default(), &[]).unwrap(),
        "1.25"
    );
}

#[test]
fn precision_answers_are_complete_atomic_and_single_use() {
    let program = compile("{'widget': 'float', 'decimal_precision': 'Unit'}");
    let plan = make_plan(&program);
    assert_eq!(plan.precision_names().collect::<Vec<_>>(), ["Unit"]);
    let mut stage = make_stage(&plan);
    assert_eq!(
        stage.precision_request().unwrap().collect::<Vec<_>>(),
        ["Unit"]
    );
    data_error(
        stage.supply_precisions(vec![]).unwrap_err(),
        "decimal precision `Unit` was not loaded for this report",
    );
    data_error(
        stage
            .supply_precisions(vec![precision("Unit", 2), precision("Unit", 3)])
            .unwrap_err(),
        "decimal precision `Unit` was supplied twice",
    );
    data_error(
        stage
            .supply_precisions(vec![precision("Other", 2)])
            .unwrap_err(),
        "decimal precision `Other` was not requested",
    );
    assert_eq!(stage.reads(), 0);
    stage.supply_precisions(vec![precision("Unit", 3)]).unwrap();
    assert!(stage.precision_request().is_none());
    data_error(
        stage
            .supply_precisions(vec![precision("Unit", 3)])
            .unwrap_err(),
        "no decimal precision read is pending",
    );
    assert_eq!(stage.reads(), 1);
    assert_eq!(stage.finish().unwrap().len(), 0);
    data_error(
        make_stage(&plan).finish().unwrap_err(),
        "decimal precision `Unit` was not loaded for this report",
    );
    for digits in [-1, 29, i64::MAX] {
        data_error(
            LoadedPrecision::new("Unit", digits).unwrap_err(),
            &format!("decimal precision `Unit` digits must be in 0..=28; got {digits}"),
        );
    }
    data_error(
        LoadedPrecision::new("", 2).unwrap_err(),
        "decimal precision name must be 1 to 128 bytes long",
    );
}

#[test]
fn precision_metadata_is_bounded_even_without_record_reads() {
    let arch = "<p><span t-out='1' t-options=\"{'widget':'float','decimal_precision':'A'}\"/><span t-out='2' t-options=\"{'widget':'float','decimal_precision':'B'}\"/></p>";
    let compile = |limits| {
        ReportProgram::compile(
            "precision.report",
            &[TemplateSource::new("precision.report", arch)],
            &Language::new("en_US").unwrap(),
            limits,
        )
        .unwrap()
    };
    let program = compile(ReportLimits::default().with_plan_paths(1).unwrap());
    let err = program
        .prefetch_plan(&PlanContext::new("x.doc"), &|_, _| None)
        .unwrap_err();
    assert!(
        matches!(
            err,
            ReportError::Limit {
                name: "plan_paths",
                maximum: 1
            }
        ),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "report limit plan_paths exceeded or invalid (maximum 1)"
    );
    let program = compile(ReportLimits::default());
    let plan = make_plan(&program);
    let err = DataStage::new(
        &plan,
        &[("docs", &[])],
        ReportLimits::default().with_data_records(1).unwrap(),
        Limits::default(),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            ReportError::Limit {
                name: "data_records",
                maximum: 1
            }
        ),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "report limit data_records exceeded or invalid (maximum 1)"
    );
}

#[test]
fn literal_names_are_deduplicated_across_the_closed_program() {
    let sources = [
        TemplateSource::new(
            "precision.report",
            "<p><t t-foreach='[1, 2]' t-as='n'><span t-out='n' t-options=\"{'widget':'float','decimal_precision':'B'}\"/><t t-call='precision.child'/></t><span t-if='False' t-out='0' t-options=\"{'widget':'float','decimal_precision':'A'}\"/></p>",
        ),
        TemplateSource::new(
            "precision.child",
            "<span t-out='1' t-options=\"{'widget':'float','decimal_precision':'A'}\"/>",
        ),
    ];
    let program = ReportProgram::compile(
        "precision.report",
        &sources,
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    let plan = make_plan(&program);
    assert_eq!(plan.precision_names().collect::<Vec<_>>(), ["A", "B"]);
    let mut stage = make_stage(&plan);
    stage
        .supply_precisions(vec![precision("B", 0), precision("A", 1)])
        .unwrap();
    assert_eq!(stage.reads(), 1);
    assert_eq!(
        rendered(
            &program,
            stage.finish().unwrap(),
            &EvalOptions::default(),
            &[]
        )
        .unwrap(),
        "11.021.0"
    );
}

#[test]
fn evaluation_without_loaded_precision_and_nonfloat_use_fail_loudly() {
    let program = compile("{'widget': 'float', 'decimal_precision': 'Unit'}");
    data_error(
        rendered(&program, RecordStore::new(), &EvalOptions::default(), &[]).unwrap_err(),
        "decimal precision `Unit` was not loaded for this report",
    );
    for options in [
        "{'widget': 'integer', 'decimal_precision': 'Unit'}",
        "{'decimal_precision': 'Unit'}",
    ] {
        let program = compile(options);
        let err = rendered(&program, RecordStore::new(), &EvalOptions::default(), &[]).unwrap_err();
        assert!(matches!(err, ReportError::Evaluate { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "precision.report:1: widget option `decimal_precision` requires the float widget"
        );
    }
}

#[test]
fn nonfloat_field_shortcuts_cannot_ignore_precision() {
    let arch = "<p><t t-foreach='docs' t-as='doc'><span t-field='doc.value' t-options=\"{'decimal_precision': 'Unit'}\"/></t></p>";
    let program = ReportProgram::compile(
        "precision.report",
        &[TemplateSource::new("precision.report", arch)],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    for field in [
        FieldValue::selection("a", "A"),
        FieldValue::many2one("x.doc", Some(2)),
        FieldValue::many("x.doc", &[2]),
        FieldValue::monetary(12.0, "currency_id"),
    ] {
        let mut store = RecordStore::new();
        store
            .insert(
                "x.doc",
                1,
                "Doc",
                vec![
                    ("value", field),
                    ("currency_id", FieldValue::many2one("res.currency", Some(3))),
                ],
            )
            .unwrap();
        store.insert("x.doc", 2, "Related", vec![]).unwrap();
        store
            .insert(
                "res.currency",
                3,
                "Currency",
                vec![
                    ("symbol", FieldValue::char("$")),
                    ("position", FieldValue::char("before")),
                    ("decimal_places", FieldValue::integer(2)),
                ],
            )
            .unwrap();
        let err = rendered(&program, store, &EvalOptions::default(), &[]).unwrap_err();
        assert!(matches!(err, ReportError::Evaluate { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "precision.report:1: widget option `decimal_precision` requires the float widget"
        );
    }
}

#[test]
fn precision_and_html_images_share_the_record_budget() {
    let program = ReportProgram::compile(
        "precision.images",
        &[TemplateSource::new(
            "precision.images",
            "<div><span t-foreach='docs' t-as='o' t-field='o.note'/><span t-out='1.25' t-options=\"{'widget': 'float', 'decimal_precision': 'Unit'}\"/></div>",
        )],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    let plan = program
        .prefetch_plan(&PlanContext::new("x.doc"), &|_, field| {
            (field == "note").then(|| FieldInfo::scalar(false).with_html())
        })
        .unwrap();
    for maximum in [2, 3] {
        let mut stage = DataStage::new(
            &plan,
            &[("docs", &[1])],
            ReportLimits::default().with_data_records(maximum).unwrap(),
            Limits::default(),
        )
        .unwrap();
        stage.supply_precisions(vec![precision("Unit", 2)]).unwrap();
        stage
            .supply(vec![
                LoadedRecord::new(1)
                    .with_display_name("Document")
                    .with_field(
                        "note",
                        FieldValue::html("<img src='/web/image/x.picture/2/data'>"),
                    ),
            ])
            .unwrap();
        if maximum == 2 {
            let error = stage.image_request().unwrap_err();
            assert!(matches!(
                error,
                ReportError::Limit {
                    name: "data_records",
                    maximum: 2
                }
            ));
            assert_eq!(
                error.to_string(),
                "report limit data_records exceeded or invalid (maximum 2)"
            );
        } else {
            assert!(stage.image_request().unwrap().is_some());
            stage.supply_images(vec![]).unwrap();
            assert!(stage.image_request().unwrap().is_none());
            assert_eq!(stage.finish().unwrap().len(), 1);
        }
    }
}
