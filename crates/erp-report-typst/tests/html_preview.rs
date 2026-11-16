// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
use erp_report::{
    Alignment, CrossAlignment, DocTree, DocumentBuilder, DocumentOptions, LayoutEngine, Limits,
    Node, PageCounter, PageSetup, ReportError, Style, Vector, render_html,
};
use erp_report_typst::TypstEngine;
use std::path::Path;

fn engine() -> TypstEngine {
    let fonts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts");
    TypstEngine::new(fonts, Limits::default()).unwrap()
}
fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}
const TERMS: &str = "Terms: <script>alert('x')</script> & \"quoted\" payment in 30 days.";

/// An order: running header, two-column address grid, line table with a header row, styled totals,
/// terms text that looks like markup, a rule, and a `Page current / total` footer.
fn order(options: DocumentOptions) -> DocTree {
    let mut b = DocumentBuilder::new(Limits::default());
    let header = b.add(Node::Text("ACME Trading Ltd".into())).unwrap();
    b.set_header(header).unwrap();
    let parts = [
        b.add(Node::Text("Page ".into())).unwrap(),
        b.add(Node::PageCounter(PageCounter::Current)).unwrap(),
        b.add(Node::Text(" / ".into())).unwrap(),
        b.add(Node::PageCounter(PageCounter::Total)).unwrap(),
    ];
    let footer = b
        .add(Node::Inline {
            children: parts.to_vec(),
            style: Style::default(),
        })
        .unwrap();
    b.set_footer(footer).unwrap();
    let title_text = b.add(Node::Text("Quotation S00042".into())).unwrap();
    let title = b
        .add(Node::Block {
            children: vec![title_text],
            style: Style::default().with_size(16.0).unwrap().with_bold(true),
        })
        .unwrap();
    let ship = b.add(Node::Text("Shipping: Jl. Merdeka 1".into())).unwrap();
    let bill = b
        .add(Node::Text("Invoicing: Jl. Sudirman 2".into()))
        .unwrap();
    let grid = b
        .add(Node::Grid {
            columns: vec![1.0, 1.0],
            gutter: 12.0,
            cells: vec![ship, bill],
            align: erp_report::CrossAlignment::Start,
        })
        .unwrap();
    let mut header_cells = Vec::new();
    for h in ["Description", "Quantity", "Amount"] {
        header_cells.push(b.add(Node::Text(h.into())).unwrap());
    }
    let mut cells = Vec::new();
    for (name, qty, amount) in [
        ("Office chair", "2.00", "300.00"),
        ("Desk", "1.00", "450.00"),
    ] {
        for v in [name, qty, amount] {
            cells.push(b.add(Node::Text(v.into())).unwrap());
        }
    }
    let table = b
        .add(Node::Table {
            columns: 3,
            header: header_cells,
            cells,
        })
        .unwrap();
    let total_text = b.add(Node::Text("Total 750.00".into())).unwrap();
    let total = b
        .add(Node::Block {
            children: vec![total_text],
            style: Style::default()
                .with_bold(true)
                .with_color([20, 60, 120])
                .with_alignment(Alignment::End),
        })
        .unwrap();
    let rule = b.add(Node::Rule).unwrap();
    let terms = b.add(Node::Text(TERMS.into())).unwrap();
    let body = b
        .add(Node::Flow {
            children: vec![title, grid, table, total, rule, terms],
        })
        .unwrap();
    b.finish(body, options).unwrap()
}
fn options() -> DocumentOptions {
    DocumentOptions::new(
        "order-S00042",
        "Quotation <S00042>",
        PageSetup::new(595.0, 842.0, 40.0).unwrap(),
    )
}
/// Visible text of the document part of the page: tags removed, entities decoded.
fn visible_text(html: &str) -> String {
    let start = html.find("<div class=\"erp-document\"").unwrap();
    let mut out = String::new();
    let mut in_tag = false;
    for c in html[start..].chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[test]
fn html_preview_text_equals_pdf_text() {
    let document = order(options());
    let pdf = engine().render(&document).unwrap();
    assert_eq!(pdf.page_count(), 1);
    let parsed = lopdf::Document::load_mem(pdf.bytes()).unwrap();
    let pages: Vec<u32> = parsed.get_pages().keys().copied().collect();
    let pdf_text = compact(&parsed.extract_text(&pages).unwrap());
    let warnings = vec!["HtmlElementUnwrapped: <video> in terms".to_string()];
    let html = render_html(&document, &warnings).unwrap();
    let html_text = compact(&visible_text(&html));
    // The counters are the one difference: the PDF resolves them, the preview has no pages.
    let page_count = pdf.page_count().to_string();
    let resolved = html_text.replace("Page/", &format!("Page1/{page_count}"));
    assert_eq!(resolved, pdf_text);
    assert!(html_text.contains("Page/"), "{html_text}");

    // Text is data: markup-looking terms are escaped, no element comes from document text.
    assert!(
        html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;quoted&quot;")
    );
    assert!(!html.contains("<script"));
    assert!(html.contains("<title>Quotation &lt;S00042&gt;</title>"));
    // The page loads nothing and carries the fixed stylesheet.
    assert!(html.contains("content=\"default-src 'none'; style-src 'unsafe-inline'\""));
    assert!(html.contains(erp_report::HTML_STYLESHEET));
    assert!(!html.contains("src="));
    // Warnings are shown, escaped, outside the document text.
    assert!(html.contains("<li>HtmlElementUnwrapped: &lt;video&gt; in terms</li>"));
    assert!(!html_text.contains("HtmlElementUnwrapped"));
    let without = render_html(&document, &[]).unwrap();
    assert!(!without.contains("erp-report-warnings\""));
    // Style and structure reach the page.
    assert!(html.contains("<th>Description</th>"));
    assert!(html.contains("font-weight:bold;color:rgb(20,60,120);text-align:end"));
    assert!(html.contains("grid-template-columns:1fr 1fr;gap:12pt"));
    assert!(html.contains("width:595pt;min-height:842pt;padding:40pt"));
    assert!(html.starts_with("<!DOCTYPE html><html lang=\"en\">"));
}

#[test]
fn html_preview_shows_running_regions_once_and_page_breaks() {
    let mut b = DocumentBuilder::new(Limits::default());
    let header = b.add(Node::Text("RUNNING-HEADER".into())).unwrap();
    b.set_header(header).unwrap();
    let mut children = Vec::new();
    for page in ["Section-A", "Section-B", "Section-C"] {
        if !children.is_empty() {
            children.push(b.add(Node::PageBreak).unwrap());
        }
        children.push(b.add(Node::Text(page.into())).unwrap());
    }
    let body = b.add(Node::Flow { children }).unwrap();
    let document = b.finish(body, options()).unwrap();
    assert_eq!(engine().render(&document).unwrap().page_count(), 3);
    let html = render_html(&document, &[]).unwrap();
    assert_eq!(html.matches("RUNNING-HEADER").count(), 1);
    assert_eq!(html.matches("class=\"erp-page-break\"").count(), 2);
}

#[test]
fn html_preview_refuses_a_font_name_outside_css_identifiers() {
    let bad = options()
        .with_fonts(vec!["Noto Sans'};body{display:none".into()])
        .unwrap();
    let error = render_html(&order(bad), &[]).unwrap_err();
    assert!(
        matches!(error, ReportError::InvalidDocument { reason } if reason.contains("font family")),
        "{error:?}"
    );
}

/// Spanning cells take their slots in the preview table as in the PDF; a trusted vector is inlined
/// and a raster-free placeholder never loads anything.
#[test]
fn spans_and_vectors_in_the_preview() {
    let mut b = DocumentBuilder::new(Limits::default());
    let text = |b: &mut DocumentBuilder, t: &str| b.add(Node::Text(t.into())).unwrap();
    let wide = text(&mut b, "wide");
    let wide = b
        .add(Node::Cell {
            child: wide,
            colspan: 2,
            rowspan: 1,
        })
        .unwrap();
    let tall = text(&mut b, "tall");
    let tall = b
        .add(Node::Cell {
            child: tall,
            colspan: 1,
            rowspan: 2,
        })
        .unwrap();
    let (x, y) = (text(&mut b, "x"), text(&mut b, "y"));
    let table = b
        .add(Node::Table {
            columns: 2,
            header: vec![],
            cells: vec![wide, tall, x, y],
        })
        .unwrap();
    let vector = Vector::rectangles(10.0, 5.0, &[[0.0, 0.0, 1.0, 5.0]], [0, 0, 0]).unwrap();
    let code = b
        .add(Node::Vector {
            vector,
            width: None,
            height: None,
            alt: "code <1>".into(),
        })
        .unwrap();
    let body = b
        .add(Node::Flow {
            children: vec![table, code],
        })
        .unwrap();
    let html = render_html(&b.finish(body, options()).unwrap(), &[]).unwrap();
    assert!(
        html.contains(
            "<tbody><tr><td colspan=\"2\">wide</td></tr><tr><td rowspan=\"2\">tall</td><td>x</td></tr><tr><td>y</td></tr></tbody>"
        ),
        "{html}"
    );
    assert!(html.contains("aria-label=\"code &lt;1&gt;\""), "{html}");
    assert!(
        html.contains("<svg xmlns=\"http://www.w3.org/2000/svg\""),
        "{html}"
    );
}

/// The preview places a grid's cells in their row as the PDF does: the middle and the bottom are
/// written, the top is the grid's default.
#[test]
fn the_preview_writes_the_row_alignment_of_a_grid() {
    let preview = |align: CrossAlignment| {
        let mut b = DocumentBuilder::new(Limits::default());
        let left = b.add(Node::Text("logo".into())).unwrap();
        let right = b.add(Node::Text("name".into())).unwrap();
        let grid = b
            .add(Node::Grid {
                columns: vec![4.0, 8.0],
                gutter: 15.0,
                cells: vec![left, right],
                align,
            })
            .unwrap();
        render_html(&b.finish(grid, options()).unwrap(), &[]).unwrap()
    };
    let grid_style = |html: &str| {
        let start = html.find("class=\"erp-grid\" style=\"").unwrap();
        let style = &html[start + "class=\"erp-grid\" style=\"".len()..];
        style[..style.find('"').unwrap()].to_owned()
    };
    assert_eq!(
        grid_style(&preview(CrossAlignment::Start)),
        "grid-template-columns:4fr 8fr;gap:15pt"
    );
    assert_eq!(
        grid_style(&preview(CrossAlignment::Center)),
        "grid-template-columns:4fr 8fr;gap:15pt;align-items:center"
    );
    assert_eq!(
        grid_style(&preview(CrossAlignment::End)),
        "grid-template-columns:4fr 8fr;gap:15pt;align-items:end"
    );
}
