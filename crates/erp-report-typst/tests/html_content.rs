// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
use erp_report::{
    DocumentBuilder, DocumentOptions, LayoutEngine, Limits, Node, PageSetup, RichText, Style,
};
use erp_report_typst::TypstEngine;
use std::path::Path;

fn engine() -> TypstEngine {
    TypstEngine::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts"),
        Limits::default(),
    )
    .unwrap()
}
fn options() -> DocumentOptions {
    DocumentOptions::new("html", "HTML", PageSetup::new(400.0, 500.0, 35.0).unwrap())
}

#[test]
fn rich_content_renders_as_data_and_keeps_safe_pdf_link() {
    let mut b = DocumentBuilder::new(Limits::default());
    let text = b
        .add(Node::Text("Sentinel #panic(\"not code\")".into()))
        .unwrap();
    let rich = RichText::default()
        .with_strike(true)
        .with_preformatted(true)
        .with_rtl(Some(false))
        .with_baseline(1)
        .unwrap()
        .with_tracking(0.2)
        .unwrap()
        .with_font("Noto Sans")
        .unwrap()
        .with_link("https://example.test/path?q=one&x=two")
        .unwrap();
    let body = b
        .add(Node::Inline {
            children: vec![text],
            style: Style::default().with_rich(rich),
        })
        .unwrap();
    let document = b.finish(body, options()).unwrap();
    let result = engine().render(&document).unwrap();
    let pdf = lopdf::Document::load_mem(result.bytes()).unwrap();
    let pages: Vec<_> = pdf.get_pages().keys().copied().collect();
    let text = pdf.extract_text(&pages).unwrap();
    assert!(text.contains("Sentinel"), "{text}");
    assert!(text.contains("not code"), "{text}");
    let mut pending: Vec<_> = pdf.objects.values().collect();
    let mut found = false;
    while let Some(object) = pending.pop() {
        match object {
            lopdf::Object::Dictionary(dict) => {
                if let Ok(uri) = dict.get(b"URI") {
                    found |= uri.as_str().unwrap() == b"https://example.test/path?q=one&x=two";
                }
                pending.extend(dict.iter().map(|(_, value)| value));
            }
            lopdf::Object::Array(values) => pending.extend(values),
            _ => {}
        }
    }
    assert!(found, "PDF must retain the typed safe-link annotation");
}

#[test]
fn evaluated_content_warnings_reach_render_result_once() {
    use erp_expr::EvalContext;
    use erp_report::{
        EvalOptions, FieldValue, Language, RecordStore, ReportLimits, ReportProgram, TemplateSource,
    };
    let mut records = RecordStore::new();
    records
        .insert(
            "note",
            1,
            "First",
            vec![("body", FieldValue::html("<unknown>retained</unknown><p>Before<br>After &amp; next</p><pre>A  B\nC</pre>"))],
        )
        .unwrap();
    let records = std::sync::Arc::new(records);
    let context = EvalContext::standard().with_var("doc", records.recordset("note", &[1]));
    let program = ReportProgram::compile(
        "html",
        &[TemplateSource::new("html", "<div t-field='doc.body'/>")],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    let doc = program
        .evaluate(&context, &records, &EvalOptions::default(), options())
        .unwrap();
    assert_eq!(doc.content_warnings().len(), 1);
    let result = engine().render(&doc).unwrap();
    assert_eq!(result.content_warnings(), doc.content_warnings());
    let pdf = lopdf::Document::load_mem(result.bytes()).unwrap();
    let text = pdf
        .extract_text(&pdf.get_pages().keys().copied().collect::<Vec<_>>())
        .unwrap();
    assert!(
        text.contains("retained") && text.contains("Before") && text.contains("After & next"),
        "{text}"
    );
    assert!(!text.contains("<p>") && !text.contains("&amp;"), "{text}");
}

#[test]
fn corrupt_html_image_keeps_typed_origin_after_rendering() {
    use erp_expr::EvalContext;
    use erp_report::{
        EvalOptions, FieldValue, Language, RecordStore, ReportLimits, ReportProgram,
        ReportWarningKind, TemplateSource,
    };
    let mut records = RecordStore::new();
    records.insert("note", 7, "Invoice", vec![("body", FieldValue::html(
        "<p>Before<img alt='Broken seal' src='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB'/>After</p>"
    ))]).unwrap();
    let records = std::sync::Arc::new(records);
    let context = EvalContext::standard().with_var("doc", records.recordset("note", &[7]));
    let program = ReportProgram::compile(
        "html",
        &[TemplateSource::new("html", "<div t-field='doc.body'/>")],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    let doc = program
        .evaluate(&context, &records, &EvalOptions::default(), options())
        .unwrap();
    assert!(doc.content_warnings().is_empty());
    let result = engine().render(&doc).unwrap();
    assert_eq!(result.image_failures().len(), 1);
    assert_eq!(result.content_warnings().len(), 1);
    let warning = &result.content_warnings()[0];
    assert!(
        matches!(warning.kind(), ReportWarningKind::ImageDegraded { reason } if !reason.is_empty())
    );
    assert_eq!(
        warning.source().record(),
        Some(("note", 7, "Invoice", "body"))
    );
    assert_eq!(warning.template(), "html");
    assert_eq!(warning.line(), 1);
    assert_eq!(result.warnings(), &[warning.to_string()]);
    let pdf = lopdf::Document::load_mem(result.bytes()).unwrap();
    let text = pdf
        .extract_text(&pdf.get_pages().keys().copied().collect::<Vec<_>>())
        .unwrap();
    assert!(text.contains("Before") && text.contains("After"), "{text}");
    assert!(
        text.split_whitespace()
            .collect::<String>()
            .contains("Brokenseal"),
        "{text}"
    );
}
