// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
//! The prefetch plan and the data stage: a compiled program in, the reads of a batch out, and the
//! store those reads fill. The "database" is a function of model and id, so every expected value
//! is computed from the same function the reads were answered with.
use erp_expr::EvalContext;
use erp_report::{
    DataStage, DocTree, DocumentOptions, EvalOptions, FieldInfo, FieldValue, Language, LimitKind,
    Limits, LoadedRecord, Node, PageSetup, PlanContext, PrefetchPlan, ReportError, ReportHelper,
    ReportLimits, ReportProgram, TemplateSource, probe_image,
};
use std::sync::Arc;

const NBSP: &str = "\u{a0}";

fn compile_with(sources: &[(&str, &str)], limits: ReportLimits) -> ReportProgram {
    let sources: Vec<_> = sources
        .iter()
        .map(|(key, arch)| TemplateSource::new(key, arch))
        .collect();
    ReportProgram::compile(
        "test.report",
        &sources,
        &Language::new("en_US").unwrap(),
        limits,
    )
    .unwrap()
}
fn compile(arch: &str) -> ReportProgram {
    compile_with(&[("test.report", arch)], ReportLimits::default())
}
/// The model registry of the tests.
fn schema(model: &str, field: &str) -> Option<FieldInfo> {
    Some(match (model, field) {
        ("sale.order", "name" | "logo") => FieldInfo::scalar(false),
        ("sale.order", "partner_id") => FieldInfo::relation("res.partner", false),
        ("sale.order", "company_id") => FieldInfo::relation("res.company", false),
        ("sale.order", "currency_id") => FieldInfo::relation("res.currency", false),
        ("sale.order", "line_ids") => FieldInfo::relation("sale.order.line", false),
        ("sale.order", "amount_total") => {
            FieldInfo::scalar(false).with_currency_field("currency_id")
        }
        ("sale.order.line", "qty") => FieldInfo::scalar(false),
        ("sale.order.line", "product_id") => FieldInfo::relation("product.product", false),
        ("product.product", "default_code") => FieldInfo::scalar(false),
        ("res.partner" | "res.company" | "res.country", "name") => FieldInfo::scalar(false),
        ("res.partner", "country_id") => FieldInfo::relation("res.country", false),
        ("res.currency", "symbol" | "position" | "decimal_places") => FieldInfo::scalar(false),
        _ => return None,
    })
}
const PARTNERS: i64 = 50;
const PRODUCTS: i64 = 20;
const COUNTRIES: i64 = 3;
fn partner_of(order: i64) -> i64 {
    order % PARTNERS + 1
}
fn lines_of(order: i64) -> [i64; 2] {
    [order * 10 + 1, order * 10 + 2]
}
fn product_of(line: i64) -> i64 {
    line % PRODUCTS + 1
}
fn country_of(partner: i64) -> i64 {
    partner % COUNTRIES + 1
}
fn display_name(model: &str, id: i64) -> String {
    format!("{model} #{id}")
}
/// A header that states a PNG of this size; the pixels are not there.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes
}
/// The "database": every field of one record. Order 40404 does not exist.
fn row(model: &str, id: i64) -> Option<Vec<(&'static str, FieldValue)>> {
    Some(match model {
        "sale.order" if id == 40_404 => return None,
        "sale.order" => vec![
            ("name", FieldValue::char(&format!("S{id:05}"))),
            (
                "partner_id",
                FieldValue::many2one("res.partner", Some(partner_of(id))),
            ),
            ("company_id", FieldValue::many2one("res.company", Some(1))),
            ("currency_id", FieldValue::many2one("res.currency", Some(1))),
            (
                "line_ids",
                FieldValue::many("sale.order.line", &lines_of(id)),
            ),
            ("amount_total", FieldValue::monetary(100.5, "currency_id")),
            ("logo", FieldValue::binary(&png(20, 10))),
        ],
        "sale.order.line" => vec![
            ("qty", FieldValue::integer(id % 7)),
            (
                "product_id",
                FieldValue::many2one("product.product", Some(product_of(id))),
            ),
        ],
        "product.product" => vec![("default_code", FieldValue::char(&format!("P{id}")))],
        "res.partner" => vec![
            ("name", FieldValue::char(&format!("Partner {id}"))),
            (
                "country_id",
                FieldValue::many2one("res.country", Some(country_of(id))),
            ),
        ],
        "res.company" => vec![("name", FieldValue::char("Company"))],
        "res.country" => vec![("name", FieldValue::char(&format!("Country {id}")))],
        "res.currency" => vec![
            ("symbol", FieldValue::char("$")),
            ("position", FieldValue::selection("before", "Before")),
            ("decimal_places", FieldValue::integer(2)),
        ],
        other => panic!("the tests have no model {other}"),
    })
}
/// One read as the stage asked for it: model, number of ids, fields, display name.
type Read = (String, usize, Vec<String>, bool);
/// Answer every request from [`row`], and say what was asked.
fn run(stage: &mut DataStage<'_>) -> Result<Vec<Read>, ReportError> {
    let mut reads = Vec::new();
    while let Some(request) = stage.request() {
        let model = request.model().to_owned();
        let fields: Vec<String> = request.fields().iter().map(|f| (*f).to_owned()).collect();
        let named = request.display_name();
        let ids = request.ids().to_vec();
        let records = ids
            .iter()
            .filter_map(|id| {
                let all = row(&model, *id)?;
                let mut record = LoadedRecord::new(*id);
                if named {
                    record = record.with_display_name(display_name(&model, *id));
                }
                for field in &fields {
                    let (_, value) = all
                        .iter()
                        .find(|(name, _)| name == field)
                        .expect("the schema and the rows have the same fields");
                    record = record.with_field(field.as_str(), value.clone());
                }
                Some(record)
            })
            .collect();
        reads.push((model, ids.len(), fields, named));
        stage.supply(records)?;
    }
    Ok(reads)
}
/// The ids of `docs`, as the stage takes them.
fn docs(ids: &[i64]) -> [(&'static str, &[i64]); 1] {
    [("docs", ids)]
}
fn plan_of(program: &ReportProgram) -> PrefetchPlan {
    program
        .prefetch_plan(&PlanContext::new("sale.order"), &schema)
        .unwrap()
}
fn levels(plan: &PrefetchPlan) -> Vec<(usize, String, Vec<String>, bool)> {
    plan.levels()
        .iter()
        .map(|level| {
            (
                level.depth(),
                level.model().to_owned(),
                level.fields().to_vec(),
                level.display_name(),
            )
        })
        .collect()
}
fn level(
    depth: usize,
    model: &str,
    fields: &[&str],
    named: bool,
) -> (usize, String, Vec<String>, bool) {
    (
        depth,
        model.to_owned(),
        fields.iter().map(|f| (*f).to_owned()).collect(),
        named,
    )
}
fn plan_error(arch: &str, context: &PlanContext<'_>) -> ReportError {
    compile(arch).prefetch_plan(context, &schema).unwrap_err()
}
/// The text nodes of a document, in the order they were built.
fn texts(doc: &DocTree) -> Vec<String> {
    doc.nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Text(text) => Some(text.to_string()),
            _ => None,
        })
        .collect()
}
fn print(
    program: &ReportProgram,
    context: &EvalContext,
    store: &erp_report::RecordStore,
) -> Result<Vec<String>, ReportError> {
    let options = DocumentOptions::new("doc-1", "Test", PageSetup::new(595.0, 842.0, 40.0)?);
    let doc = program.evaluate(context, store, &EvalOptions::default(), options)?;
    Ok(texts(&doc))
}

const ORDER: &str = "<div><t t-foreach='docs' t-as='o'><p t-field='o.name'/><span \
                     t-out='o.partner_id.name'/><span t-out='o.partner_id.country_id.name'/><t \
                     t-foreach='o.line_ids' t-as='line'><span \
                     t-out='line.product_id.default_code'/><span t-out='line.qty'/></t></t></div>";

#[test]
fn the_plan_has_one_level_per_model_and_relation_depth() {
    let plan = plan_of(&compile(ORDER));
    assert_eq!(
        levels(&plan),
        [
            level(0, "sale.order", &["line_ids", "name", "partner_id"], false),
            level(1, "res.partner", &["country_id", "name"], false),
            level(1, "sale.order.line", &["product_id", "qty"], false),
            level(2, "product.product", &["default_code"], false),
            level(2, "res.country", &["name"], false),
        ]
    );
    assert_eq!(plan.roots().collect::<Vec<_>>(), [("docs", "sale.order")]);
    assert_eq!(plan.helpers().count(), 0);
}

/// against an in-memory source; the same bound in SQL statements is the ORM adapter's test)
#[test]
fn a_batch_of_a_thousand_records_is_read_once_per_plan_level() {
    let program = compile(ORDER);
    let plan = plan_of(&program);
    let mut counts = Vec::new();
    for size in [10_i64, 1_000] {
        let ids: Vec<i64> = (1..=size).collect();
        let mut stage = DataStage::new(
            &plan,
            &docs(&ids),
            ReportLimits::default(),
            Limits::default(),
        )
        .unwrap();
        let reads = run(&mut stage).unwrap();
        assert_eq!(stage.reads(), reads.len());
        assert!(
            reads.len() <= plan.levels().len(),
            "{} reads for {} levels",
            reads.len(),
            plan.levels().len()
        );
        // Every read is of the fields its level names, for records that were not read before.
        for (model, _, fields, named) in &reads {
            let of_model = plan.levels().into_iter().find(|l| l.model() == model);
            let of_model = of_model.expect("a read is of a model of the plan");
            assert_eq!(fields, of_model.fields());
            assert_eq!(*named, of_model.display_name());
        }
        let orders = reads.iter().find(|(model, ..)| model == "sale.order");
        assert_eq!(
            orders.map(|(_, asked, ..)| *asked),
            usize::try_from(size).ok()
        );
        let store = Arc::new(stage.finish().unwrap());
        let partners: std::collections::BTreeSet<i64> =
            ids.iter().map(|id| partner_of(*id)).collect();
        let lines: Vec<i64> = ids.iter().flat_map(|id| lines_of(*id)).collect();
        let products: std::collections::BTreeSet<i64> =
            lines.iter().map(|id| product_of(*id)).collect();
        let countries: std::collections::BTreeSet<i64> =
            partners.iter().map(|id| country_of(*id)).collect();
        assert_eq!(
            store.len(),
            ids.len() + lines.len() + partners.len() + products.len() + countries.len()
        );
        // The store is what evaluation reads: one order prints from it.
        let last = size;
        let context =
            EvalContext::standard().with_var("docs", store.recordset("sale.order", &[last]));
        let printed = print(&program, &context, &store).unwrap();
        let [first_line, second_line] = lines_of(last);
        // The paragraph is one text; the spans after it are inline and run together.
        let inline = format!(
            "Partner {}Country {}P{}{}P{}{}",
            partner_of(last),
            country_of(partner_of(last)),
            product_of(first_line),
            first_line % 7,
            product_of(second_line),
            second_line % 7,
        );
        assert_eq!(printed, [format!("S{last:05}"), inline]);
        counts.push(reads.len());
    }
    assert_eq!(counts[0], counts[1], "the reads depend on the batch size");
}

#[test]
fn a_record_that_no_longer_exists_is_left_out_and_not_followed() {
    let plan = plan_of(&compile(ORDER));
    let mut stage = DataStage::new(
        &plan,
        &docs(&[40_404, 1, 1]),
        ReportLimits::default(),
        Limits::default(),
    )
    .unwrap();
    let reads = run(&mut stage).unwrap();
    // The repeated id is asked once; the missing order has no partner or lines to read.
    assert_eq!(reads[0].1, 2);
    let partners = reads.iter().find(|(model, ..)| model == "res.partner");
    assert_eq!(partners.map(|(_, asked, ..)| *asked), Some(1));
    let store = Arc::new(stage.finish().unwrap());
    let program = compile("<div><p t-out='x.name'/></div>");
    let context = EvalContext::standard().with_var("x", store.recordset("sale.order", &[40_404]));
    let err = print(&program, &context, &store).unwrap_err();
    assert!(matches!(err, ReportError::Expression { .. }), "{err:?}");
    assert!(
        err.to_string()
            .contains("record sale.order(40404) was not loaded for this report"),
        "{err}"
    );
}

/// by the expression context)
#[test]
fn a_method_that_is_not_a_registered_helper_is_a_compile_error_naming_it() {
    let arch = "<div><t t-foreach='docs' t-as='o'><p t-out='o._get_name(1)'/></t></div>";
    let err = plan_error(arch, &PlanContext::new("sale.order"));
    assert!(matches!(err, ReportError::Compile { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "test.report:1: `o._get_name(1)`: `sale.order._get_name` is not a registered report helper"
    );
    // A helper of another model does not allow it.
    let other = [ReportHelper::new("res.partner", "_get_name", &[])];
    let err = plan_error(arch, &PlanContext::new("sale.order").with_helpers(&other));
    assert!(matches!(err, ReportError::Compile { .. }), "{err:?}");
    // ORM methods are no exception.
    let err = plan_error(
        "<div><p t-out=\"docs.mapped('name')\"/></div>",
        &PlanContext::new("sale.order"),
    );
    assert_eq!(
        err.to_string(),
        "test.report:1: `docs.mapped('name')`: `sale.order.mapped` is not a registered report helper"
    );
    // A method of a plain value is not a helper call.
    let plan = plan_of(&compile(
        "<div><t t-foreach='docs' t-as='o'><p t-out='o.name.upper()'/></t></div>",
    ));
    assert_eq!(levels(&plan), [level(0, "sale.order", &["name"], false)]);
}

#[test]
fn a_registered_helper_adds_its_declared_fields_and_waits_for_its_task() {
    let arch = "<div><t t-foreach='docs' t-as='o'><p t-out='o._get_name()'/></t></div>";
    let helpers = [ReportHelper::new(
        "sale.order",
        "_get_name",
        &["name", "partner_id.country_id.name"],
    )];
    let context = PlanContext::new("sale.order").with_helpers(&helpers);
    let plan = compile(arch).prefetch_plan(&context, &schema).unwrap();
    assert_eq!(
        levels(&plan),
        [
            level(0, "sale.order", &["name", "partner_id"], false),
            level(1, "res.partner", &["country_id"], false),
            level(2, "res.country", &["name"], false),
        ]
    );
    assert_eq!(
        plan.helpers().collect::<Vec<_>>(),
        [("sale.order", "_get_name")]
    );
    let err = DataStage::new(
        &plan,
        &docs(&[1]),
        ReportLimits::default(),
        Limits::default(),
    )
    .unwrap_err();
    assert!(matches!(err, ReportError::Unsupported { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "report helper calls requires a later release"
    );
    // A declared dependency that is no field path of the model is refused with the helper.
    let wrong = [ReportHelper::new("sale.order", "_get_name", &["name.x"])];
    let err = plan_error(arch, &PlanContext::new("sale.order").with_helpers(&wrong));
    assert!(matches!(err, ReportError::Compile { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "test.report:1: report helper `sale.order._get_name` declares `name.x`, which is not a path of fields"
    );
}

#[test]
fn a_field_the_model_does_not_have_is_a_compile_error() {
    let err = plan_error(
        "<div>\n<t t-foreach='docs' t-as='o'><p t-out='o.partner_id.nope'/></t></div>",
        &PlanContext::new("sale.order"),
    );
    assert!(matches!(err, ReportError::Compile { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "test.report:2: `o.partner_id.nope`: model `res.partner` has no field `nope`"
    );
}

#[test]
fn printed_relations_read_display_names_and_amounts_read_their_currency() {
    let program = compile(
        "<div><t t-foreach='docs' t-as='o'><p t-field='o.partner_id'/><p \
         t-field='o.amount_total'/><p t-out='o.display_name'/></t></div>",
    );
    let plan = plan_of(&program);
    assert_eq!(
        levels(&plan),
        [
            level(
                0,
                "sale.order",
                &["amount_total", "currency_id", "partner_id"],
                true
            ),
            level(
                1,
                "res.currency",
                &["decimal_places", "position", "symbol"],
                false
            ),
            level(1, "res.partner", &[], true),
        ]
    );
    let mut stage = DataStage::new(
        &plan,
        &docs(&[7]),
        ReportLimits::default(),
        Limits::default(),
    )
    .unwrap();
    run(&mut stage).unwrap();
    let store = Arc::new(stage.finish().unwrap());
    let context = EvalContext::standard().with_var("docs", store.recordset("sale.order", &[7]));
    assert_eq!(
        print(&program, &context, &store).unwrap(),
        [
            display_name("res.partner", partner_of(7)),
            format!("${NBSP}100.50"),
            display_name("sale.order", 7),
        ]
    );
    // The currency a widget is given is read the same way.
    let plan = plan_of(&compile(
        "<div><t t-foreach='docs' t-as='o'><p t-out='3.5' t-options=\"{'widget': 'monetary', \
         'display_currency': o.currency_id}\"/></t></div>",
    ));
    assert_eq!(
        levels(&plan),
        [
            level(0, "sale.order", &["currency_id"], false),
            level(
                1,
                "res.currency",
                &["decimal_places", "position", "symbol"],
                false
            ),
        ]
    );
}

#[test]
fn a_display_name_the_plan_did_not_see_is_refused_at_evaluation() {
    let plan = plan_of(&compile(
        "<div><t t-foreach='docs' t-as='o'><p t-out='o.name'/></t></div>",
    ));
    let mut stage = DataStage::new(
        &plan,
        &docs(&[1]),
        ReportLimits::default(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        run(&mut stage).unwrap(),
        [("sale.order".to_owned(), 1, vec!["name".to_owned()], false)]
    );
    let store = Arc::new(stage.finish().unwrap());
    // `x` is not a name the plan knows, so nothing was read for it.
    let program = compile("<div><p t-out='x.display_name'/></div>");
    let context = EvalContext::standard().with_var("x", store.recordset("sale.order", &[1]));
    let err = print(&program, &context, &store).unwrap_err();
    assert!(matches!(err, ReportError::Expression { .. }), "{err:?}");
    assert!(
        err.to_string()
            .contains("display_name of sale.order(1) was not loaded for this report"),
        "{err}"
    );
}

#[test]
fn declared_names_are_typed_and_other_names_are_plain_values() {
    let program = compile(
        "<div><p t-out='company.name'/><p t-out='title.name'/><p t-out='unknown.thing'/></div>",
    );
    let bindings = [("company", Some("res.company")), ("title", None)];
    let context = PlanContext::new("sale.order").with_bindings(&bindings);
    let plan = program.prefetch_plan(&context, &schema).unwrap();
    assert_eq!(levels(&plan), [level(0, "res.company", &["name"], false)]);
    assert_eq!(
        plan.roots().collect::<Vec<_>>(),
        [("docs", "sale.order"), ("company", "res.company")]
    );
    let limits = ReportLimits::default();
    for (roots, message) in [
        (
            vec![("docs", &[1_i64][..])],
            "report data: the ids of `company` were not given",
        ),
        (
            vec![
                ("docs", &[1][..]),
                ("company", &[1][..]),
                ("user", &[2][..]),
            ],
            "report data: `user` is not a name of the report context that holds records",
        ),
        (
            vec![
                ("docs", &[1][..]),
                ("company", &[1][..]),
                ("docs", &[2][..]),
            ],
            "report data: the ids of `docs` were given twice",
        ),
    ] {
        let err = DataStage::new(&plan, &roots, limits, Limits::default()).unwrap_err();
        assert!(matches!(err, ReportError::Data { .. }), "{err:?}");
        assert_eq!(err.to_string(), message);
    }
    // Nothing is read of `docs`: the only read is the company.
    let roots = [("docs", &[1_i64, 2][..]), ("company", &[1][..])];
    let mut stage = DataStage::new(&plan, &roots, limits, Limits::default()).unwrap();
    assert_eq!(
        run(&mut stage).unwrap(),
        [("res.company".to_owned(), 1, vec!["name".to_owned()], false)]
    );
    // A context name declared twice, or as `docs`, is refused.
    let twice = [
        ("company", Some("res.company")),
        ("docs", Some("res.company")),
    ];
    let err = program
        .prefetch_plan(
            &PlanContext::new("sale.order").with_bindings(&twice),
            &schema,
        )
        .unwrap_err();
    assert!(matches!(err, ReportError::Compile { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "test.report:1: context name `docs` is not a name, is declared twice or names no model"
    );
}

#[test]
fn a_called_template_is_typed_by_its_caller() {
    let program = compile_with(
        &[
            (
                "test.report",
                "<div><t t-foreach='docs' t-as='o'><t t-call='test.partner' \
                 partner='o.partner_id'><p t-out='o.name'/></t></t></div>",
            ),
            (
                "test.partner",
                "<div><p t-out='partner.name'/><p t-out='o.company_id.name'/><t t-out='0'/></div>",
            ),
        ],
        ReportLimits::default(),
    );
    assert_eq!(
        levels(&plan_of(&program)),
        [
            level(
                0,
                "sale.order",
                &["company_id", "name", "partner_id"],
                false
            ),
            level(1, "res.company", &["name"], false),
            level(1, "res.partner", &["name"], false),
        ]
    );
}

#[test]
fn a_value_of_either_model_reads_the_field_of_both() {
    let plan = plan_of(&compile(
        "<div><t t-foreach='docs' t-as='o'><t t-set='who' t-value='o.partner_id or \
         o.company_id'/><p t-out='who.name'/><p t-out='[l.qty for l in o.line_ids]'/><p \
         t-out='o.line_ids[0].product_id.default_code'/></t></div>",
    ));
    assert_eq!(
        levels(&plan),
        [
            level(
                0,
                "sale.order",
                &["company_id", "line_ids", "partner_id"],
                false
            ),
            level(1, "res.company", &["name"], false),
            level(1, "res.partner", &["name"], false),
            level(1, "sale.order.line", &["product_id", "qty"], false),
            level(2, "product.product", &["default_code"], false),
        ]
    );
}

#[test]
fn plan_paths_and_loaded_records_are_bounded() {
    for value in [0, 4_097] {
        let err = ReportLimits::default().with_plan_paths(value).unwrap_err();
        assert!(matches!(err, ReportError::Limit { .. }), "{err:?}");
        assert_eq!(
            err.to_string(),
            "report limit plan_paths exceeded or invalid (maximum 4096)"
        );
    }
    for value in [0, 200_001] {
        let err = ReportLimits::default()
            .with_data_records(value)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "report limit data_records exceeded or invalid (maximum 200000)"
        );
    }
    // `docs` and its partner are two paths.
    let limits = ReportLimits::default().with_plan_paths(2).unwrap();
    let program = compile_with(&[("test.report", ORDER)], limits);
    let err = program
        .prefetch_plan(&PlanContext::new("sale.order"), &schema)
        .unwrap_err();
    assert!(matches!(err, ReportError::Limit { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "report limit plan_paths exceeded or invalid (maximum 2)"
    );
    // Ten orders alone are over a limit of nine records, before anything is read.
    let plan = plan_of(&compile(ORDER));
    let ids: Vec<i64> = (1..=10).collect();
    let limits = ReportLimits::default().with_data_records(9).unwrap();
    let err = DataStage::new(&plan, &docs(&ids), limits, Limits::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "report limit data_records exceeded or invalid (maximum 9)"
    );
    // Three orders fit; their six lines do not, and the stage stops before asking for them.
    let limits = ReportLimits::default().with_data_records(5).unwrap();
    let mut stage = DataStage::new(&plan, &docs(&ids[..3]), limits, Limits::default()).unwrap();
    let err = run(&mut stage).unwrap_err();
    assert!(matches!(err, ReportError::Limit { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "report limit data_records exceeded or invalid (maximum 5)"
    );
}

/// error naming model, record and field; the image node is tested in `evaluate.rs`)
#[test]
fn a_template_image_over_a_limit_is_an_image_error_naming_the_field() {
    let program = compile("<div><t t-foreach='docs' t-as='o'><p t-if='o.logo'>logo</p></t></div>");
    let plan = plan_of(&program);
    let logo = png(20, 10);
    for document in [
        Limits::default()
            .lowered(LimitKind::ImageBytes, logo.len() - 1)
            .unwrap(),
        Limits::default()
            .lowered(LimitKind::ImagePixels, 199)
            .unwrap(),
    ] {
        let message = format!(
            "report image: sale.order(3).logo: {}",
            probe_image(&logo, &document).unwrap_err()
        );
        let mut stage =
            DataStage::new(&plan, &docs(&[3]), ReportLimits::default(), document).unwrap();
        let err = run(&mut stage).unwrap_err();
        assert!(matches!(err, ReportError::Image { .. }), "{err:?}");
        assert_eq!(err.to_string(), message);
    }
    // At the limits the image is kept and the template sees a value.
    let document = Limits::default()
        .lowered(LimitKind::ImageBytes, logo.len())
        .unwrap()
        .lowered(LimitKind::ImagePixels, 200)
        .unwrap();
    let mut stage = DataStage::new(&plan, &docs(&[3]), ReportLimits::default(), document).unwrap();
    run(&mut stage).unwrap();
    let store = Arc::new(stage.finish().unwrap());
    let context = EvalContext::standard().with_var("docs", store.recordset("sale.order", &[3]));
    assert_eq!(print(&program, &context, &store).unwrap(), ["logo"]);
}

#[test]
fn an_answer_that_does_not_match_its_request_is_refused() {
    let plan = plan_of(&compile(
        "<div><t t-foreach='docs' t-as='o'><p t-field='o.partner_id'/></t></div>",
    ));
    let start = || {
        DataStage::new(
            &plan,
            &docs(&[1]),
            ReportLimits::default(),
            Limits::default(),
        )
        .unwrap()
    };
    let partner = || FieldValue::many2one("res.partner", Some(2));
    for (records, message) in [
        (
            vec![LoadedRecord::new(9).with_field("partner_id", partner())],
            "report data: sale.order(9) was read but not asked for",
        ),
        (
            vec![LoadedRecord::new(1)],
            "report data: sale.order(1).partner_id is missing from its read",
        ),
        (
            vec![
                LoadedRecord::new(1)
                    .with_field("partner_id", partner())
                    .with_field("name", FieldValue::char("S1")),
            ],
            "report data: sale.order(1).name was read but is not in the plan",
        ),
        (
            vec![
                LoadedRecord::new(1).with_field("partner_id", partner()),
                LoadedRecord::new(1).with_field("partner_id", partner()),
            ],
            "report data: sale.order(1) was read twice",
        ),
        (
            vec![LoadedRecord::new(1).with_field("partner_id", FieldValue::char("x"))],
            "report data: sale.order.partner_id is a relation in the plan and was not read as one",
        ),
        (
            vec![
                LoadedRecord::new(1)
                    .with_field("partner_id", FieldValue::many2one("res.users", Some(2))),
            ],
            "report data: sale.order.partner_id was read as a relation to res.users; the plan says res.partner",
        ),
    ] {
        let err = start().supply(records).unwrap_err();
        assert!(matches!(err, ReportError::Data { .. }), "{err:?}");
        assert_eq!(err.to_string(), message);
    }
    // The partner's display name was asked for.
    let mut stage = start();
    stage
        .supply(vec![
            LoadedRecord::new(1).with_field("partner_id", partner()),
        ])
        .unwrap();
    let request = stage.request().expect("the partner is read next");
    assert_eq!(
        (request.model(), request.ids(), request.display_name()),
        ("res.partner", &[2_i64][..], true)
    );
    assert!(request.fields().is_empty());
    let err = stage.supply(vec![LoadedRecord::new(2)]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "report data: the display name of res.partner(2) is missing from its read"
    );
    // A stage with a pending read has no store, and one without takes no answer.
    let err = start().finish().unwrap_err();
    assert_eq!(
        err.to_string(),
        "report data: the read of sale.order is still pending"
    );
    let mut stage = start();
    stage
        .supply(vec![LoadedRecord::new(1).with_field(
            "partner_id",
            FieldValue::many2one("res.partner", None),
        )])
        .unwrap();
    assert!(stage.request().is_none());
    let err = stage.supply(Vec::new()).unwrap_err();
    assert_eq!(err.to_string(), "report data: no read is pending");
}
