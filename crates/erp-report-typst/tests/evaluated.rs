// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! Templates through the whole library: compile, evaluate, lay out, read the PDF back with an
//! independent parser. Every checked string comes out of the rendered bytes.
use erp_expr::EvalContext;
use erp_report::{
    DocumentOptions, EvalOptions, FieldValue, Language, LayoutEngine, Limits, PageSetup,
    RecordStore, ReportLimits, ReportProgram, TemplateSource,
};
use erp_report_typst::TypstEngine;
use std::{path::Path, sync::Arc};

fn engine() -> TypstEngine {
    let fonts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts");
    TypstEngine::new(fonts, Limits::default()).unwrap()
}
fn options() -> DocumentOptions {
    DocumentOptions::new(
        "orders",
        "Orders",
        PageSetup::new(400.0, 500.0, 35.0).unwrap(),
    )
}
/// Text of every page without white space, extracted after reopening the bytes.
fn page_texts(bytes: &[u8]) -> Vec<String> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    pdf.get_pages()
        .keys()
        .map(|page| {
            let text = pdf.extract_text(&[*page]).unwrap();
            text.chars().filter(|c| !c.is_whitespace()).collect()
        })
        .collect()
}
/// Three orders, named with letters only so that every digit on a page comes from the counters.
fn orders() -> Arc<RecordStore> {
    let mut store = RecordStore::new();
    for (id, name) in [(11, "Desk"), (12, "Chair")] {
        store
            .insert(
                "sale.order.line",
                id,
                name,
                vec![("name", FieldValue::char(name))],
            )
            .unwrap();
    }
    for (id, name) in [(1, "ORDER-A"), (2, "ORDER-B"), (3, "ORDER-C")] {
        store
            .insert(
                "sale.order",
                id,
                name,
                vec![
                    ("name", FieldValue::char(name)),
                    ("line_ids", FieldValue::many("sale.order.line", &[11, 12])),
                ],
            )
            .unwrap();
    }
    Arc::new(store)
}
fn render(arch: &str) -> (usize, Vec<String>) {
    let sources = [TemplateSource::new("sale.report_orders", arch)];
    let program = ReportProgram::compile(
        "sale.report_orders",
        &sources,
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    let store = orders();
    let context =
        EvalContext::standard().with_var("docs", store.recordset("sale.order", &[1, 2, 3]));
    let document = program
        .evaluate(&context, &store, &EvalOptions::default(), options())
        .unwrap();
    let output = engine().render(&document).unwrap();
    (output.page_count(), page_texts(output.bytes()))
}
const BODY: &str = "<h2 t-field='doc.name'/>\
    <table class='table table-sm'><thead><tr><th>Item</th></tr></thead><tbody>\
    <tr t-foreach='doc.line_ids' t-as='line'><td t-field='line.name'/></tr></tbody></table>\
    <div class='row'><div class='col-6'>Left</div><div class='col-6 text-end'>Right</div></div>\
    <ul><li>Note</li></ul>";

#[test]
fn an_evaluated_report_paginates_with_its_regions_counters_and_breaks() {
    for (label, arch) in [
        (
            "break before, at the page flow",
            format!(
                "<div><div class='header'>ORDER-HEADER</div>\
                 <div class='footer text-center'><span class='page'/> / <span class='topage'/></div>\
                 <div class='article'><t t-foreach='docs' t-as='doc'>\
                 <div t-att-class=\"None if doc_first else 'page-break-before'\">{BODY}</div>\
                 </t></div></div>"
            ),
        ),
        (
            "break after, inside a block that only changes the text style",
            format!(
                "<div><div class='header'>ORDER-HEADER</div>\
                 <div class='footer text-center'><span class='page'/> / <span class='topage'/></div>\
                 <div class='article small'><t t-foreach='docs' t-as='doc'>\
                 <div t-att-style=\"None if doc_last else 'page-break-after: always'\">{BODY}</div>\
                 </t></div></div>"
            ),
        ),
    ] {
        let (count, pages) = render(&arch);
        assert_eq!(count, 3, "{label}");
        assert_eq!(pages.len(), 3, "{label}");
        for (index, text) in pages.iter().enumerate() {
            let page = index + 1;
            let order = ["ORDER-A", "ORDER-B", "ORDER-C"][index];
            for expected in [
                "ORDER-HEADER",
                order,
                "Item",
                "Desk",
                "Chair",
                "Left",
                "Right",
                "Note",
            ] {
                assert!(
                    text.contains(expected),
                    "{label}, page {page}: no {expected} in {text:?}"
                );
            }
            for other in ["ORDER-A", "ORDER-B", "ORDER-C"] {
                assert_eq!(
                    text.contains(other),
                    other == order,
                    "{label}, page {page}: {text:?}"
                );
            }
            let counters: String = text
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '/')
                .collect();
            assert_eq!(
                counters,
                format!("{page}/3"),
                "{label}, page {page}: {text:?}"
            );
        }
    }
}
