// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `qweb-html` converter: the same validated [`DocTree`] the PDF engine lays out, written as one
//! self-contained HTML page with a fixed stylesheet. Text is escaped data; styles come only from the
//! validated [`Style`] values; the page loads nothing (its content security policy forbids it).
use crate::{Alignment, DocTree, Node, NodeId, PageCounter, ReportError, Style, error::invalid};
use std::fmt::Write;

/// The fixed preview stylesheet. Screen-only presentation of the paper document: a page-sized sheet,
/// page breaks as visible separators, table borders like the native renderer's default table stroke.
pub const HTML_STYLESHEET: &str = "\
body{margin:0;background:#e8e8e8}\
.erp-report-warnings{margin:12pt auto;max-width:800pt;padding:8pt 12pt;background:#fff4d6;border:1pt solid #c9a227;font:10pt sans-serif}\
.erp-report-warnings ul{margin:4pt 0 0;padding-left:16pt}\
.erp-document{margin:12pt auto;background:#fff;box-sizing:border-box;box-shadow:0 1pt 4pt rgba(0,0,0,.25);color:#000;font-size:10pt}\
.erp-document p{margin:0 0 6pt}\
.erp-document table{border-collapse:collapse}\
.erp-document td,.erp-document th{border:1pt solid #000;padding:5pt;text-align:start;font-weight:inherit;vertical-align:top}\
.erp-document hr{border:0;border-top:1pt solid #000}\
.erp-page-break{break-after:page;border-top:1pt dashed #999;margin:12pt 0}\
.erp-grid{display:grid}\
.erp-keep{break-inside:avoid}\
.erp-image{display:inline-block;box-sizing:border-box;border:1pt dashed #999;color:#666;font-size:8pt;overflow:hidden}\
@media print{body{background:#fff}.erp-report-warnings{display:none}.erp-document{box-shadow:none;margin:0}.erp-page-break{border:0;margin:0}}";

/// Content security policy of the preview page: inline styles only, no script, image, font or frame.
const POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'";

/// Render `document` as an HTML preview page. `warnings` are the evaluator's warning texts for this
/// document; each is shown above the page. Page counters stay empty: on screen there is no pagination,
/// as in the reference, whose pager exists only in the PDF.
///
/// Fails with `InvalidDocument` when a font family name holds characters outside letters, digits,
/// space, `-` and `_` (it would otherwise be written into CSS).
pub fn render_html(document: &DocTree, warnings: &[String]) -> Result<String, ReportError> {
    let options = document.options();
    let mut fonts = String::new();
    for (i, font) in options.fonts().iter().enumerate() {
        if !font
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        {
            return Err(invalid(
                "font family names in an HTML preview are letters, digits, space, '-' and '_'",
            ));
        }
        if i > 0 {
            fonts.push(',');
        }
        let _ = write!(fonts, "'{font}'");
    }
    let page = options.page();
    let mut out = String::with_capacity(4096);
    out.push_str("<!DOCTYPE html><html lang=\"");
    out.push_str(options.language());
    out.push_str(
        "\"><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"",
    );
    out.push_str(POLICY);
    out.push_str("\"><title>");
    escape(&mut out, options.title());
    out.push_str("</title><style>");
    out.push_str(HTML_STYLESHEET);
    out.push_str("</style></head><body>");
    if !warnings.is_empty() {
        out.push_str("<div class=\"erp-report-warnings\" role=\"status\">Warnings<ul>");
        for warning in warnings {
            out.push_str("<li>");
            escape(&mut out, warning);
            out.push_str("</li>");
        }
        out.push_str("</ul></div>");
    }
    let _ = write!(
        out,
        "<div class=\"erp-document\" style=\"width:{}pt;min-height:{}pt;padding:{}pt;font-family:{fonts}\">",
        page.width(),
        page.height(),
        page.margin()
    );
    let nodes = document.nodes();
    if let Some(header) = document.header() {
        out.push_str("<header>");
        node(&mut out, nodes, header);
        out.push_str("</header>");
    }
    out.push_str("<main>");
    node(&mut out, nodes, document.body());
    out.push_str("</main>");
    if let Some(footer) = document.footer() {
        out.push_str("<footer>");
        node(&mut out, nodes, footer);
        out.push_str("</footer>");
    }
    out.push_str("</div></body></html>");
    Ok(out)
}

fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

fn style(out: &mut String, style: &Style, block: bool) {
    let d = Style::default();
    let _ = write!(out, "font-size:{}pt", style.size());
    if style.bold() {
        out.push_str(";font-weight:bold");
    }
    if style.italic() {
        out.push_str(";font-style:italic");
    }
    if style.underline() {
        out.push_str(";text-decoration:underline");
    }
    if style.color() != d.color() {
        let [r, g, b] = style.color();
        let _ = write!(out, ";color:rgb({r},{g},{b})");
    }
    if block {
        if style.padding() != 0.0 {
            let _ = write!(out, ";padding:{}pt", style.padding());
        }
        match style.alignment() {
            Alignment::Start => {}
            Alignment::Center => out.push_str(";text-align:center"),
            Alignment::End => out.push_str(";text-align:end"),
        }
    }
}

fn size(out: &mut String, width: Option<f64>, height: Option<f64>) {
    if let Some(w) = width {
        let _ = write!(out, "width:{w}pt;");
    }
    if let Some(h) = height {
        let _ = write!(out, "height:{h}pt;");
    }
}

/// Table rows of `columns` slots. A `Cell` takes `colspan` slots and keeps its slots of the next
/// `rowspan - 1` rows, so the following cells flow into the free slots, as in the PDF table.
fn rows(out: &mut String, nodes: &[Node], cells: &[NodeId], columns: usize, tag: &str) {
    let mut covered = vec![0usize; columns.max(1)];
    let mut next = cells.iter().peekable();
    while next.peek().is_some() {
        out.push_str("<tr>");
        let mut col = 0;
        loop {
            while col < covered.len() && covered[col] > 0 {
                col += 1;
            }
            let Some(id) = (if col < covered.len() {
                next.next()
            } else {
                None
            }) else {
                break;
            };
            match nodes.get(id.index()) {
                Some(Node::Cell {
                    child,
                    colspan,
                    rowspan,
                }) => {
                    let _ = write!(out, "<{tag}");
                    if *colspan > 1 {
                        let _ = write!(out, " colspan=\"{colspan}\"");
                    }
                    if *rowspan > 1 {
                        let _ = write!(out, " rowspan=\"{rowspan}\"");
                    }
                    out.push('>');
                    node(out, nodes, child);
                    let _ = write!(out, "</{tag}>");
                    let end = (col + (*colspan).max(1)).min(covered.len());
                    covered[col..end].fill((*rowspan).max(1));
                    col = end;
                }
                _ => {
                    children(out, nodes, std::slice::from_ref(id), tag);
                    covered[col] = 1;
                    col += 1;
                }
            }
        }
        out.push_str("</tr>");
        covered.iter_mut().for_each(|c| *c = c.saturating_sub(1));
    }
}

fn children(out: &mut String, nodes: &[Node], ids: &[NodeId], tag: &str) {
    for id in ids {
        let _ = write!(out, "<{tag}>");
        node(out, nodes, id);
        let _ = write!(out, "</{tag}>");
    }
}

/// Recursion is bounded: a `DocTree` is admitted at a depth of at most 32 levels.
fn node(out: &mut String, nodes: &[Node], id: &NodeId) {
    // Every id of a finished tree points into its own arena (the builder refuses foreign or later ids).
    let Some(n) = nodes.get(id.index()) else {
        return;
    };
    match n {
        Node::Flow { children: c } => c.iter().for_each(|id| node(out, nodes, id)),
        Node::Text(text) => escape(out, text),
        Node::Block {
            children: c,
            style: s,
        } => {
            out.push_str(if s.keep_together() {
                "<div class=\"erp-keep\" style=\""
            } else {
                "<p style=\""
            });
            style(out, s, true);
            out.push_str("\">");
            c.iter().for_each(|id| node(out, nodes, id));
            out.push_str(if s.keep_together() { "</div>" } else { "</p>" });
        }
        Node::Inline {
            children: c,
            style: s,
        } => {
            out.push_str("<span style=\"");
            style(out, s, false);
            out.push_str("\">");
            c.iter().for_each(|id| node(out, nodes, id));
            out.push_str("</span>");
        }
        Node::Grid {
            columns,
            gutter,
            cells,
        } => {
            out.push_str("<div class=\"erp-grid\" style=\"grid-template-columns:");
            for (i, weight) in columns.iter().enumerate() {
                let _ = write!(out, "{}{weight}fr", if i > 0 { " " } else { "" });
            }
            let _ = write!(out, ";gap:{gutter}pt\">");
            children(out, nodes, cells, "div");
            out.push_str("</div>");
        }
        Node::Table {
            columns,
            header,
            cells,
        } => {
            out.push_str("<table>");
            if !header.is_empty() {
                out.push_str("<thead>");
                rows(out, nodes, header, *columns, "th");
                out.push_str("</thead>");
            }
            out.push_str("<tbody>");
            rows(out, nodes, cells, *columns, "td");
            out.push_str("</tbody></table>");
        }
        Node::List { ordered, items } => {
            let tag = if *ordered { "ol" } else { "ul" };
            let _ = write!(out, "<{tag}>");
            children(out, nodes, items, "li");
            let _ = write!(out, "</{tag}>");
        }
        // The preview loads nothing (POLICY), so a raster or user SVG image is a sized box with its
        // alt text, like the in-process engine's placeholder for user SVG.
        Node::Image {
            width, height, alt, ..
        } => {
            out.push_str("<span class=\"erp-image\" role=\"img\" aria-label=\"");
            escape(out, alt);
            out.push_str("\" style=\"");
            size(out, *width, *height);
            out.push_str("\">");
            escape(out, alt);
            out.push_str("</span>");
        }
        // Trusted: the SVG text is generated by `Vector` itself from numbers, never from user input.
        Node::Vector {
            vector,
            width,
            height,
            alt,
        } => {
            out.push_str("<span role=\"img\" aria-label=\"");
            escape(out, alt);
            out.push_str("\" style=\"display:inline-block;");
            size(
                out,
                Some(width.unwrap_or(vector.width())),
                Some(height.unwrap_or(vector.height())),
            );
            out.push_str("\">");
            out.push_str(&String::from_utf8_lossy(vector.svg()));
            out.push_str("</span>");
        }
        // A cell outside a table row cannot be built; its content is still written.
        Node::Cell { child, .. } => node(out, nodes, child),
        Node::Rule => out.push_str("<hr>"),
        Node::LineBreak => out.push_str("<br>"),
        Node::PageBreak => out.push_str("<div class=\"erp-page-break\"></div>"),
        Node::PageCounter(PageCounter::Current) => {
            out.push_str("<span class=\"erp-page-counter\" data-counter=\"current\"></span>");
        }
        Node::PageCounter(PageCounter::Total) => {
            out.push_str("<span class=\"erp-page-counter\" data-counter=\"total\"></span>");
        }
    }
}
