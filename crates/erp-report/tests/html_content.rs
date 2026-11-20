// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
use erp_expr::EvalContext;
use erp_report::{
    DocTree, DocumentOptions, EvalOptions, FieldValue, Language, PageSetup, RecordStore,
    ReportLimits, ReportProgram, TemplateSource, render_html,
};
use std::sync::Arc;
fn evaluate(html: &str, directive: &str) -> DocTree {
    evaluate_limits(html, directive, ReportLimits::default()).unwrap()
}
fn evaluate_limits(
    html: &str,
    directive: &str,
    limits: ReportLimits,
) -> Result<DocTree, erp_report::ReportError> {
    let mut store = RecordStore::new();
    store
        .insert(
            "test.record",
            7,
            "Terms",
            vec![("note", FieldValue::html(html))],
        )
        .unwrap();
    let store = Arc::new(store);
    let mut context = EvalContext::standard();
    context.set("doc", store.recordset("test.record", &[7]));
    let arch = format!("<div><span {directive}=\"doc.note\"/></div>");
    let program = ReportProgram::compile(
        "test.html",
        &[TemplateSource::new("test.html", &arch)],
        &Language::new("en_US").unwrap(),
        limits,
    )
    .unwrap();
    program.evaluate(
        &context,
        &store,
        &EvalOptions::default(),
        DocumentOptions::new("html", "HTML", PageSetup::new(595.0, 842.0, 40.0).unwrap()),
    )
}
#[test]
fn html_field_preserves_markup_and_entities() {
    let doc = evaluate(
        "<p>Terms &amp; Conditions<br><strong>Payment</strong></p>",
        "t-field",
    );
    let preview = render_html(&doc, &[]).unwrap();
    assert!(preview.contains("Terms &amp; Conditions"), "{preview}");
    assert!(!preview.contains("&lt;p&gt;"), "{preview}");
    assert!(preview.contains("<br"), "{preview}");
}
#[test]
fn html_out_preserves_field_markup() {
    let doc = evaluate("<p>Before<br>After</p>", "t-out");
    let preview = render_html(&doc, &[]).unwrap();
    assert!(!preview.contains("&lt;p&gt;"), "{preview}");
    assert!(preview.contains("<br"), "{preview}");
}

#[test]
fn degrade_rules_retain_text_and_exact_warning() {
    use erp_report::ReportWarningKind as W;
    let cases = [
        (
            "<custom>Keep</custom>",
            W::HtmlElementUnwrapped {
                element: "custom".into(),
            },
        ),
        (
            "<p onclick='bad'>Keep</p>",
            W::HtmlAttributeDropped {
                element: "p".into(),
                attribute: "onclick".into(),
            },
        ),
        (
            "<p style='potato:blue'>Keep</p>",
            W::HtmlStyleDropped {
                property: "potato".into(),
                value: "blue".into(),
            },
        ),
        (
            "<p class='unknown unknown'>Keep</p>",
            W::HtmlClassIgnored {
                class: "unknown".into(),
            },
        ),
        (
            "<a href='javascript:bad'>Keep</a>",
            W::LinkDropped {
                scheme: "javascript".into(),
            },
        ),
        (
            "<img src='broken' alt='Keep'>",
            W::ImageDegraded {
                reason: "image source is not resolved".into(),
            },
        ),
    ];
    for (html, expected) in [
        (
            "<p class='table-sm'>Keep</p>",
            W::HtmlClassIgnored {
                class: "table-sm".into(),
            },
        ),
        (
            "<font color='transparent'>Keep</font>",
            W::HtmlAttributeDropped {
                element: "font".into(),
                attribute: "color".into(),
            },
        ),
        (
            "<span style='background-color:red'>Keep</span>",
            W::HtmlStyleDropped {
                property: "background-color".into(),
                value: "red".into(),
            },
        ),
    ]
    .into_iter()
    .chain(cases)
    {
        let doc = evaluate(html, "t-field");
        assert!(render_html(&doc, &[]).unwrap().contains("Keep"));
        assert_eq!(doc.content_warnings().len(), 1, "{html}");
        let warning = &doc.content_warnings()[0];
        assert_eq!(warning.kind(), &expected);
        assert_eq!(
            warning.source().record(),
            Some(("test.record", 7, "Terms", "note"))
        );
        assert_eq!(warning.template(), "test.html");
        assert_eq!(warning.line(), 1);
    }
}
#[test]
fn external_image_is_never_fetched() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let html = format!(
        "<img src='http://{}/private?secret=hidden' alt='Logo' width='40' height='20'>",
        listener.local_addr().unwrap()
    );
    let doc = evaluate(&html, "t-field");
    assert_eq!(
        doc.content_warnings()[0].kind(),
        &erp_report::ReportWarningKind::ExternalImageNotFetched {
            url_host: "127.0.0.1".into()
        }
    );
    assert!(render_html(&doc, &[]).unwrap().contains("Logo"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(!format!("{:?}", doc.content_warnings()).contains("secret"));
}
#[test]
fn output_depth_flattens_without_losing_text() {
    let html = format!("{}Keep{}", "<b>".repeat(80), "</b>".repeat(80));
    let doc = evaluate(&html, "t-field");
    assert!(render_html(&doc, &[]).unwrap().contains("Keep"));
    assert!(doc.content_warnings().iter().any(|w| matches!(
        w.kind(),
        erp_report::ReportWarningKind::HtmlFlattened { .. }
    )));
}

#[test]
fn supported_formatting_and_table_content_survive() {
    let doc = evaluate(
        "<pre>A  B\nC</pre><p><s>strike</s><sub>sub</sub><sup>sup</sup><a href='https://example.test/?a=1&amp;b=2'>link</a><font color='red' size='3'>font</font></p><table><tr><th>A</th><th>B</th></tr><tr><td>one</td><td>two</td></tr></table>",
        "t-field",
    );
    let html = render_html(&doc, &[]).unwrap();
    for part in [
        "A  B\nC",
        "line-through",
        "vertical-align:sub",
        "vertical-align:super",
        "https://example.test/?a=1&amp;b=2",
        "one",
        "two",
    ] {
        assert!(html.contains(part), "{part}: {html}");
    }
    assert!(
        doc.content_warnings().is_empty(),
        "{:?}",
        doc.content_warnings()
    );
}
#[test]
fn invalid_style_drops_only_its_declaration() {
    let doc = evaluate(
        "<p style='font-size:999999999pt;color:red'>Keep</p>",
        "t-field",
    );
    assert_eq!(doc.content_warnings().len(), 1);
    assert!(
        render_html(&doc, &[])
            .unwrap()
            .contains("color:rgb(255,0,0)")
    );
}
#[test]
fn parse_failure_is_plain_text_with_warning() {
    let doc = evaluate("<p>Keep\0</p>", "t-field");
    assert_eq!(
        doc.content_warnings()[0].kind(),
        &erp_report::ReportWarningKind::HtmlParseFailed
    );
    assert!(render_html(&doc, &[]).unwrap().contains("&lt;p&gt;Keep"));
}
#[test]
fn html_admission_configuration_is_bounded() {
    for (bytes, nodes, name, max) in [
        (0, 1, "html_bytes", 4 * 1024 * 1024),
        (1, 0, "html_nodes", 50000),
        (4 * 1024 * 1024 + 1, 1, "html_bytes", 4 * 1024 * 1024),
        (1, 50001, "html_nodes", 50000),
    ] {
        let error = ReportLimits::default()
            .with_html_limits(bytes, nodes)
            .unwrap_err();
        assert!(
            matches!(error,erp_report::ReportError::Limit { name: got, maximum } if got==name && maximum==max)
        );
        assert_eq!(
            error.to_string(),
            format!("report limit {name} exceeded or invalid (maximum {max})")
        );
    }
}

#[test]
fn malformed_list_content_degrades_without_strict_template_errors() {
    for html in [
        "<ul>Keep</ul>",
        "<ol><div>Keep</div><li>Also</li></ol>",
        "<ul><ul><li>Keep</li></ul></ul>",
    ] {
        let doc = evaluate(html, "t-field");
        assert!(render_html(&doc, &[]).unwrap().contains("Keep"));
        assert!(!doc.content_warnings().is_empty(), "{html}");
    }
}
#[test]
fn computed_and_unrepresentable_styles_degrade_per_declaration() {
    use erp_report::ReportWarningKind as W;
    for (property, value, prefix) in [
        ("padding", "100em", "font-size:200pt;"),
        ("padding", "2000pt 2000pt", ""),
        ("padding", "1pt 2pt", ""),
        ("background-color", "transparent", ""),
        ("text-align", "justify", ""),
        ("border-top", "10pt dashed red", ""),
    ] {
        let html = format!("<p style='{prefix}{property}:{value};color:red'>Keep</p>");
        let doc = evaluate(&html, "t-field");
        assert_eq!(doc.content_warnings().len(), 1, "{html}");
        assert_eq!(
            doc.content_warnings()[0].kind(),
            &W::HtmlStyleDropped {
                property: property.into(),
                value: value.into()
            }
        );
        let preview = render_html(&doc, &[]).unwrap();
        assert!(
            preview.contains("Keep") && preview.contains("color:rgb(255,0,0)"),
            "{preview}"
        );
    }
}
#[test]
fn nested_small_text_degrades_at_the_document_font_floor() {
    let html = format!("{}Keep{}", "<small>".repeat(40), "</small>".repeat(40));
    let doc = evaluate(&html, "t-field");
    assert!(render_html(&doc, &[]).unwrap().contains("Keep"));
    assert!(!doc.content_warnings().is_empty());
}

#[test]
fn spanning_cells_survive_lone_rows_and_depth_flattening() {
    for depth in [0, 75] {
        let html = format!(
            "{}<table><tr><td colspan='2'>Keep</td></tr></table>{}",
            "<b>".repeat(depth),
            "</b>".repeat(depth)
        );
        let doc = evaluate(&html, "t-field");
        assert!(render_html(&doc, &[]).unwrap().contains("Keep"));
    }
}
#[test]
fn html_byte_and_node_admission_fail_with_kind_and_message() {
    for (limits, name, max) in [
        (
            ReportLimits::default().with_html_limits(8, 50000).unwrap(),
            "html_bytes",
            8,
        ),
        (
            ReportLimits::default().with_html_limits(1024, 2).unwrap(),
            "html_nodes",
            2,
        ),
    ] {
        let error = evaluate_limits("<p>Keep</p>", "t-field", limits).unwrap_err();
        assert!(
            matches!(error, erp_report::ReportError::Limit {name: got, maximum} if got == name && maximum == max)
        );
        assert_eq!(
            error.to_string(),
            format!("report limit {name} exceeded or invalid (maximum {max})")
        );
    }
}

fn expression(
    arch: &str,
    context: &EvalContext,
    records: &RecordStore,
) -> Result<DocTree, erp_report::ReportError> {
    ReportProgram::compile(
        "expr",
        &[TemplateSource::new("expr", arch)],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap()
    .evaluate(
        context,
        records,
        &EvalOptions::default(),
        DocumentOptions::new(
            "expr",
            "Expression",
            PageSetup::new(400.0, 500.0, 30.0).unwrap(),
        ),
    )
}
#[test]
fn ordinary_strings_escape_and_explicit_html_expression_has_no_fabricated_record() {
    let context = EvalContext::standard()
        .with_var("value", erp_expr::ExprValue::str("<unknown>Keep</unknown>"));
    let store = RecordStore::new();
    let plain = expression("<div t-out='value'/>", &context, &store).unwrap();
    assert!(
        render_html(&plain, &[])
            .unwrap()
            .contains("&lt;unknown&gt;Keep")
    );
    assert!(plain.content_warnings().is_empty());
    let html = expression(
        r#"<div t-out='value' t-options="{'widget': 'html'}"/>"#,
        &context,
        &store,
    )
    .unwrap();
    assert_eq!(html.content_warnings().len(), 1);
    assert_eq!(html.content_warnings()[0].source().record(), None);
}
#[test]
fn t_set_retains_markup_provenance_but_string_conversion_does_not() {
    let mut store = RecordStore::new();
    store
        .insert(
            "note",
            1,
            "First",
            vec![("body", FieldValue::html("<unknown>Keep</unknown>"))],
        )
        .unwrap();
    let store = Arc::new(store);
    let context = EvalContext::standard().with_var("doc", store.recordset("note", &[1]));
    let doc = expression(
        "<div><t t-set='saved' t-value='doc.body'/><span t-out='saved'/></div>",
        &context,
        &store,
    )
    .unwrap();
    assert_eq!(
        doc.content_warnings()[0].source().record(),
        Some(("note", 1, "First", "body"))
    );
    let text = expression("<div t-out='str(doc.body)'/>", &context, &store).unwrap();
    assert!(
        render_html(&text, &[])
            .unwrap()
            .contains("&lt;unknown&gt;Keep")
    );
    assert!(text.content_warnings().is_empty());
    let err = expression("<div t-out='doc.body'/>", &context, &RecordStore::new()).unwrap_err();
    assert!(matches!(err, erp_report::ReportError::Evaluate { .. }));
    assert!(
        err.to_string()
            .contains("HTML value belongs to another report store")
    );
}

#[test]
fn html_total_columns_report_a_resource_limit() {
    let err = evaluate_limits(
        "<table><tr><td colspan='1000'>A</td><td colspan='1000'>B</td></tr></table>",
        "t-field",
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        erp_report::ReportError::Limit {
            name: "html_table_columns",
            maximum: 1000
        }
    ));
    assert_eq!(
        err.to_string(),
        "report limit html_table_columns exceeded or invalid (maximum 1000)"
    );
}

#[test]
fn html_type_name_and_index_cannot_forge_store_issued_markup() {
    use erp_expr::{ExprObject, ExprResult, ExprValue};
    #[derive(Debug)]
    struct Forged;
    impl ExprObject for Forged {
        fn type_name(&self) -> &str {
            "report.html"
        }
        fn repr(&self) -> String {
            "<b>forged</b>".into()
        }
        fn as_int(&self) -> ExprResult<i64> {
            Ok(0)
        }
    }
    let mut store = RecordStore::new();
    store
        .insert(
            "note",
            1,
            "First",
            vec![("body", FieldValue::html("<b>real</b>"))],
        )
        .unwrap();
    let context = EvalContext::standard().with_var("value", ExprValue::Object(Arc::new(Forged)));
    let err = expression("<div t-out='value'/>", &context, &store).unwrap_err();
    assert!(matches!(err, erp_report::ReportError::Evaluate { .. }));
    assert_eq!(
        err.to_string(),
        "expr:1: HTML value belongs to another report store"
    );
}

#[test]
fn mapped_table_classes_apply_only_to_tables() {
    let doc = evaluate(
        "<table class='table-bordered'><tr><td>Keep</td></tr></table>",
        "t-field",
    );
    assert!(doc.content_warnings().is_empty());
    assert!(doc.nodes().iter().any(|node| matches!(node, erp_report::Node::Block { style, .. } if style.table_look() == erp_report::TableLook::Bordered)));
    let doc = evaluate("<p class='table-bordered'>Keep</p>", "t-field");
    assert_eq!(
        doc.content_warnings()[0].kind(),
        &erp_report::ReportWarningKind::HtmlClassIgnored {
            class: "table-bordered".into()
        }
    );
}
