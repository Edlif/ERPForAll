// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
use erp_expr::EvalContext;
use erp_report::{
    DataStage, DocTree, DocumentOptions, EvalOptions, FieldInfo, FieldValue, Language, Limits,
    LoadedRecord, Node, PageSetup, PlanContext, ReportLimits, ReportProgram, TemplateSource,
};
use std::sync::Arc;

// Header-only PNG: admission accepts it; renderer corruption tests cover the missing body.
const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB";
fn evaluate(html: &str, limits: Limits) -> DocTree {
    let budget = ReportLimits::default();
    let program = ReportProgram::compile(
        "test.images",
        &[TemplateSource::new(
            "test.images",
            "<div><span t-foreach='docs' t-as='o' t-field='o.note'/></div>",
        )],
        &Language::new("en_US").unwrap(),
        budget,
    )
    .unwrap();
    let plan = program
        .prefetch_plan(&PlanContext::new("test.record"), &|_, f| {
            (f == "note").then(|| FieldInfo::scalar(false).with_html())
        })
        .unwrap();
    let mut stage = DataStage::new(&plan, &[("docs", &[7])], budget, limits).unwrap();
    stage
        .supply(vec![
            LoadedRecord::new(7)
                .with_display_name("Terms")
                .with_field("note", FieldValue::html(html)),
        ])
        .unwrap();
    let store = Arc::new(stage.finish().unwrap());
    let context = EvalContext::standard().with_var("docs", store.recordset("test.record", &[7]));
    program
        .evaluate(
            &context,
            &store,
            &EvalOptions::default(),
            DocumentOptions::new(
                "images",
                "Images",
                PageSetup::new(400.0, 500.0, 30.0).unwrap(),
            ),
        )
        .unwrap()
}

#[test]
fn embedded_html_image_is_admitted_as_picture() {
    let doc = evaluate(
        &format!("<p>Before<img src='{PNG}' alt='Seal' width='20' height='10'>After</p>"),
        Limits::default(),
    );
    assert!(
        doc.content_warnings().is_empty(),
        "{:?}",
        doc.content_warnings()
    );
    assert!(doc.nodes().iter().any(|n| matches!(n, Node::Image {alt, width:Some(w), height:Some(h), ..} if alt == "Seal" && *w == 15.0 && *h == 7.5)));
}

#[test]
fn image_byte_limit_degrades_instead_of_stopping_report() {
    let doc = evaluate(
        &format!("<img src='{PNG}' alt='Seal'>"),
        Limits::default()
            .lowered(erp_report::LimitKind::ImageBytes, 1)
            .unwrap(),
    );
    let warning = &doc.content_warnings()[0];
    assert_eq!(
        warning.source().record(),
        Some(("test.record", 7, "Terms", "note"))
    );
    assert_eq!(warning.template(), "test.images");
    assert!(
        matches!(warning.kind(), erp_report::ReportWarningKind::ImageDegraded {reason} if reason.contains("image_bytes")),
        "{:?}",
        warning.kind()
    );
    assert!(
        doc.nodes()
            .iter()
            .any(|n| matches!(n, Node::Placeholder {alt,..} if alt == "Seal"))
    );
}

#[test]
fn untrusted_sources_degrade_with_redacted_typed_warnings() {
    use erp_report::ReportWarningKind;
    for src in [
        "/web/image/x/0/data",
        "/web/image/x/-1/data",
        "/web/image/x/9223372036854775808/data",
        "/web/image/x/1/data?q=s",
        "/web/image/x/1/data#f",
        "/web/image/x/1/data/extra",
        "/web/image/%2e%2e/1/data",
        "/web/image/x/1/bad-field",
    ] {
        let doc = evaluate(
            &format!("<img src='{src}' alt='Missing' width='20'>"),
            Limits::default(),
        );
        assert!(
            matches!(doc.content_warnings()[0].kind(),ReportWarningKind::ImageDegraded{reason} if reason == "invalid local image reference")
        );
    }
    for src in [
        "https://user:secret@example.com/a?token=secret",
        "//example.com/a?token=secret",
    ] {
        let doc = evaluate(&format!("<img src='{src}'>"), Limits::default());
        assert!(
            matches!(doc.content_warnings()[0].kind(),ReportWarningKind::ExternalImageNotFetched{url_host} if url_host == "example.com")
        );
        assert!(!format!("{:?}", doc.content_warnings()).contains("secret"));
    }
    for src in [
        "data:image/png;base64,!!!!",
        "file:///etc/passwd",
        "data:image/svg+xml;base64,PHN2Zy8+",
    ] {
        let doc = evaluate(&format!("<img src='{src}'>"), Limits::default());
        assert!(
            matches!(doc.content_warnings()[0].kind(),ReportWarningKind::ImageDegraded{reason} if !reason.is_empty())
        );
    }
}

fn prepared_stage<'a>(
    plan: &'a erp_report::PrefetchPlan,
    html: &str,
    limits: ReportLimits,
) -> DataStage<'a> {
    let mut stage = DataStage::new(plan, &[("docs", &[7])], limits, Limits::default()).unwrap();
    stage
        .supply(vec![
            LoadedRecord::new(7)
                .with_display_name("Terms")
                .with_field("note", FieldValue::html(html)),
        ])
        .unwrap();
    stage
}
fn image_plan() -> erp_report::PrefetchPlan {
    let p = ReportProgram::compile(
        "test.images",
        &[TemplateSource::new(
            "test.images",
            "<p t-foreach='docs' t-as='o' t-field='o.note'/>",
        )],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    p.prefetch_plan(&PlanContext::new("test.record"), &|_, f| {
        (f == "note").then(|| FieldInfo::scalar(false).with_html())
    })
    .unwrap()
}
#[test]
fn image_requests_are_exact_deduplicated_and_required() {
    let plan = image_plan();
    let html = "<img src='/web/image/x.photo/2/data'><img src='/web/image/x.photo/2/data'><img src='/web/image/x.photo/3/data'><img src='/web/image/x.photo/2/other'>";
    let mut stage = prepared_stage(&plan, html, ReportLimits::default());
    let request = stage.image_request().unwrap().unwrap();
    assert_eq!(
        (request.model(), request.field(), request.ids()),
        ("x.photo", "data", &[2, 3][..])
    );
    let error = stage.supply_images(vec![(4, Arc::from([]))]).unwrap_err();
    assert!(matches!(
        &error,
        erp_report::ReportError::InvalidDocument { .. }
    ));
    assert!(
        error
            .to_string()
            .contains("unrequested or duplicate record")
    );
    stage.supply_images(vec![]).unwrap();
    let request = stage.image_request().unwrap().unwrap();
    assert_eq!((request.field(), request.ids()), ("other", &[2][..]));
    let error = stage.finish().unwrap_err();
    assert!(
        matches!(&error,erp_report::ReportError::Data{reason} if reason == "an HTML image read is still pending")
    );
}
#[test]
fn auxiliary_records_obey_the_shared_record_cap() {
    let plan = image_plan();
    let limits = ReportLimits::default().with_data_records(1).unwrap();
    let mut stage = prepared_stage(&plan, "<img src='/web/image/x.photo/2/data'>", limits);
    assert!(matches!(
        stage.image_request(),
        Err(erp_report::ReportError::Limit {
            name: "data_records",
            maximum: 1
        })
    ));
}
#[test]
fn padding_is_not_counted_as_decoded_image_bytes() {
    // The same admitted 24-byte PNG header plus one zero byte: 25 bytes, padded base64.
    let doc = evaluate(
        "<img src='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAA=='>",
        Limits::default()
            .lowered(erp_report::LimitKind::ImageBytes, 25)
            .unwrap(),
    );
    assert!(
        doc.content_warnings().is_empty(),
        "{:?}",
        doc.content_warnings()
    );
}
