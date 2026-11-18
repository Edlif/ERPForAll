// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::picture::Pictures;
use erp_report::{
    Alignment, CrossAlignment, DocTree, ImageFit, ImageFormat, Limits, Node, NodeId, PageCounter,
    ReportError, Style, TableLook, probe_image,
};
use typst::{
    foundations::{Array, Bytes, Dict, Smart, Value},
    visualize::{ExchangeFormat, RasterImage},
};
/// Side of the placeholder box of an image without a requested size, in points.
const PLACEHOLDER_POINTS: f64 = 72.0;
/// Points of one pixel of a picture printed at its natural size (96 pixels per inch, as on screen).
const POINTS_PER_PIXEL: f64 = 0.75;
/// The box that stands for a picture that is not printed.
fn placeholder(d: &mut Dict, width: f64, height: f64, alt: &str) -> &'static str {
    sized(d, Some(width), Some(height), alt);
    "placeholder"
}
fn points(value: Option<f64>) -> Value {
    value.map_or(Value::Auto, Value::Float)
}
/// Height the text under a code takes with its gap, in points.
const CAPTION_ROOM: f64 = erp_report::CODE_CAPTION_POINTS * 1.5;
/// The printed size of a vector: its own proportions at the given width or height, fitted inside
/// both when both are given, its natural size (one unit a point) when neither is.
fn vector_size(vector: &erp_report::Vector, width: Option<f64>, height: Option<f64>) -> (f64, f64) {
    let (wide, high) = (vector.width(), vector.height());
    let scale = match (width, height) {
        (Some(w), Some(h)) => (w / wide).min(h / high),
        (Some(w), None) => w / wide,
        (None, Some(h)) => h / high,
        (None, None) => 1.0,
    };
    (wide * scale, high * scale)
}
/// The size of the text under a code `wide` points across: the house size, or smaller so the text
/// stays on one line (a figure is about 0.6 of its size wide).
fn caption_size(caption: &str, wide: f64) -> f64 {
    let fits = wide / (0.6 * caption.chars().count().max(1) as f64);
    erp_report::CODE_CAPTION_POINTS.min(fits).max(1.0)
}
fn sized(d: &mut Dict, width: Option<f64>, height: Option<f64>, alt: &str) {
    d.insert("width".into(), points(width));
    d.insert("height".into(), points(height));
    d.insert("alt".into(), string(alt));
}
fn string(value: &str) -> Value {
    Value::Str(value.into())
}
fn array(values: impl IntoIterator<Item = Value>) -> Value {
    Value::Array(values.into_iter().collect::<Array>())
}
fn indices(nodes: &[NodeId]) -> Value {
    array(nodes.iter().map(|n| Value::Int(n.index() as i64)))
}
fn styled(dict: &mut Dict, style: &Style, scale: f64) -> Result<(), ReportError> {
    dict.insert("size".into(), Value::Float(style.size() * scale));
    dict.insert("bold".into(), Value::Bool(style.bold()));
    dict.insert("italic".into(), Value::Bool(style.italic()));
    dict.insert("underline".into(), Value::Bool(style.underline()));
    dict.insert(
        "color".into(),
        array(style.color().into_iter().map(|v| Value::Int(i64::from(v)))),
    );
    dict.insert("padding".into(), Value::Float(style.padding()));
    dict.insert("keep".into(), Value::Bool(style.keep_together()));
    let (above, below) = style.spacing();
    dict.insert("above".into(), Value::Float(above));
    dict.insert("below".into(), Value::Float(below));
    dict.insert(
        "fill".into(),
        style.fill().map_or(Value::None, |rgb| {
            array(rgb.into_iter().map(|v| Value::Int(i64::from(v))))
        }),
    );
    let (rule_above, rule_below) = style.rules();
    dict.insert("rule_above".into(), Value::Bool(rule_above));
    dict.insert("rule_below".into(), Value::Bool(rule_below));
    dict.insert("decorated".into(), Value::Bool(style.decorated()));
    dict.insert(
        "table".into(),
        string(match style.table_look() {
            TableLook::Bordered => "bordered",
            TableLook::Borderless => "borderless",
            TableLook::Lined => "lined",
            _ => {
                return Err(ReportError::Unsupported {
                    feature: "table look",
                    task: "a later release",
                });
            }
        }),
    );
    dict.insert(
        "align".into(),
        string(match style.alignment() {
            Alignment::Center => "center",
            Alignment::End => "end",
            Alignment::Start => "start",
            _ => {
                return Err(ReportError::Unsupported {
                    feature: "text alignment",
                    task: "a later release",
                });
            }
        }),
    );
    Ok(())
}
/// Characters a column may claim at most when the columns share the width: one long text does not
/// squeeze the others to nothing.
const COLUMN_CHARS_MAX: usize = 40;
/// Characters a column claims at least: a short column keeps room for its header.
const COLUMN_CHARS_MIN: usize = 3;
/// A column whose longest text has at most this many characters is sized to its content (a year, a
/// format, a status), so a word never runs into the next column; longer columns share what is left.
const COLUMN_FIT_CHARS: usize = 16;
/// Points of cell padding a content-sized column adds to its text (the house table's inset, both sides).
const COLUMN_PADDING_POINTS: f64 = 12.0;
/// Share of the body width content-sized columns may take while a longer column needs the rest; above it
/// every column shares the width by its text, so many short columns never squeeze a long one to nothing.
const COLUMN_FIT_SHARE: f64 = 0.7;
/// Font scale of a table whose number columns and short columns do not fit the page at the base size:
/// the table steps down one size instead of letting an amount wrap or overlap its neighbour.
const COMPACT_SCALE: f64 = 0.9;
/// Points of width one character stands for when an image or graphic claims a column share.
const POINTS_PER_CHAR: f64 = 5.0;
/// Text length of a subtree, for sharing a table's width; an image or graphic counts by its width and a
/// nested table or grid as a full column. `depth` stops at the admitted document depth.
fn text_len(doc: &DocTree, id: &NodeId, depth: usize) -> usize {
    if depth > 64 {
        return 0;
    }
    let sum = |ids: &[NodeId]| ids.iter().map(|id| text_len(doc, id, depth + 1)).sum();
    match doc.nodes().get(id.index()) {
        Some(Node::Text(text)) => text.chars().count(),
        Some(
            Node::Block { children, .. } | Node::Inline { children, .. } | Node::Flow { children },
        ) => sum(children),
        Some(Node::Cell { child, .. }) => text_len(doc, child, depth + 1),
        Some(Node::List { items, .. }) => sum(items),
        Some(Node::Image { width, .. } | Node::Vector { width, .. }) => {
            width.map_or(COLUMN_CHARS_MAX, |w| (w / POINTS_PER_CHAR).ceil() as usize)
        }
        Some(Node::Placeholder { width, .. }) => (width / POINTS_PER_CHAR).ceil() as usize,
        Some(Node::Table { .. } | Node::Grid { .. }) => COLUMN_CHARS_MAX,
        _ => 0,
    }
}
/// Whether a cell's content is aligned to the end (`text-end`), as a number column is.
fn aligned_end(doc: &DocTree, id: &NodeId) -> bool {
    match doc.nodes().get(id.index()) {
        Some(Node::Cell { child, .. }) => aligned_end(doc, child),
        Some(Node::Block { style, .. }) => matches!(style.alignment(), Alignment::End),
        _ => false,
    }
}
/// Where each cell of one table section sits, as the renderer places them (spans move the cells after
/// them): `(row, column, colspan, rowspan)` per cell, in order.
fn placements(
    doc: &DocTree,
    columns: usize,
    section: &[NodeId],
) -> Vec<(usize, usize, usize, usize)> {
    let mut out = Vec::with_capacity(section.len());
    if columns == 0 {
        return out;
    }
    // Rows still covered by a row span from above, per column, counting the current row.
    let mut busy = vec![0usize; columns];
    let (mut row, mut col) = (0, 0);
    for id in section {
        let (colspan, rowspan) = match doc.nodes().get(id.index()) {
            Some(Node::Cell {
                colspan, rowspan, ..
            }) => (*colspan, *rowspan),
            _ => (1, 1),
        };
        // The builder admitted the placement, so a free slot exists within one more row.
        while col == columns || busy.get(col).is_some_and(|b| *b > 0) {
            if col == columns {
                busy.iter_mut().for_each(|b| *b = b.saturating_sub(1));
                col = 0;
                row += 1;
            } else {
                col += 1;
            }
        }
        let end = (col + colspan).min(columns);
        busy[col..end].iter_mut().for_each(|b| *b = rowspan);
        out.push((row, col, end - col, rowspan));
        col = end;
    }
    out
}
/// A table's columns as encoded: the shares, whether it prints one size smaller, the number of
/// header rows and the longest text per column.
struct Columns {
    weights: Vec<f64>,
    compact: bool,
    header_rows: usize,
    longest: Vec<usize>,
}
/// Column shares from the longest text in each column, header included, as a browser's automatic table
/// layout gives them: a title column gets more room than a year column. A cell spanning several columns,
/// such as a section title over a whole row, claims no column of its own. A column whose every cell is
/// aligned to the end holds numbers: it is sized to its content so an amount never wraps. `compact` says
/// the table prints one font size smaller ([`COMPACT_SCALE`]) because its content-sized columns do not
/// fit; the header rows are counted as the cells are placed (a spanning cell counts once).
fn column_weights(doc: &DocTree, columns: usize, header: &[NodeId], cells: &[NodeId]) -> Columns {
    let mut longest = vec![0; columns];
    let mut numbers = vec![true; columns];
    let mut header_rows = 0;
    for (index, section) in [header, cells].into_iter().enumerate() {
        let placed = placements(doc, columns, section);
        if index == 0 {
            header_rows = placed
                .iter()
                .map(|(row, .., rowspan)| row + rowspan)
                .max()
                .unwrap_or(0);
        }
        for (id, (_, col, colspan, _)) in section.iter().zip(placed) {
            if colspan == 1
                && let Some(column) = longest.get_mut(col)
            {
                let len = text_len(doc, id, 0);
                *column = (*column).max(len);
                if len > 0 && !aligned_end(doc, id) {
                    numbers[col] = false;
                }
            }
        }
    }
    let page = doc.options().page();
    let [_, right, _, left] = page.margins();
    let (weights, compact) = shares(&longest, &numbers, page.width() - left - right);
    Columns {
        weights,
        compact,
        header_rows,
        longest,
    }
}
/// Width of one character and height of one line, in ems, for estimating how tall content prints.
const CHAR_EM: f64 = 0.5;
const LINE_EM: f64 = 1.4;
/// Share of the page body above which a row or a kept block is estimated too tall to keep on one page:
/// it may break across pages instead of running past the bottom margin. The estimate is rough, so the
/// share leaves room for it to be off by half.
const TALL_SHARE: f64 = 0.5;
/// Line breaks a subtree forces (a block, a cell or a break starts a line) and the height of its
/// images, in points.
fn breaks(doc: &DocTree, id: &NodeId, depth: usize) -> (usize, f64) {
    if depth > 64 {
        return (0, 0.0);
    }
    let node = doc.nodes().get(id.index());
    let own = match node {
        Some(Node::Block { .. } | Node::Cell { .. } | Node::LineBreak | Node::PageBreak) => 1,
        _ => 0,
    };
    let image = match node {
        Some(Node::Image { height, .. }) => height.unwrap_or(PLACEHOLDER_POINTS),
        Some(Node::Vector {
            vector,
            width,
            height,
            caption,
            ..
        }) => {
            let room = caption.as_ref().map_or(0.0, |_| CAPTION_ROOM);
            vector_size(vector, *width, *height).1 + room
        }
        Some(Node::Placeholder { height, .. }) => *height,
        _ => 0.0,
    };
    node.into_iter()
        .flat_map(Node::children)
        .map(|child| breaks(doc, child, depth + 1))
        .fold((own, image), |(n, h), (m, g)| (n + m, h + g))
}
/// Estimated height in points of a subtree laid out `width` points wide with text of `size` points.
fn estimated_height(doc: &DocTree, id: &NodeId, width: f64, size: f64) -> f64 {
    let per_line = (width / (size * CHAR_EM)).max(1.0);
    let (forced, images) = breaks(doc, id, 0);
    let lines = (text_len(doc, id, 0) as f64 / per_line).ceil() + forced as f64;
    lines * size * LINE_EM + images
}
/// Column shares from the longest text per column; `0.0` asks the template for a column sized to its
/// content. Without a long column the longest one takes the rest of the width, so the table still spans
/// the page. When the content-sized columns would take more than their part of `body` points (estimated
/// from their text), every column shares the width by its text instead. A number column (`numbers`) is
/// sized to its content whatever its length; when the content-sized columns are too wide, the number
/// columns stay content-sized, the others share by text and the table steps down one font size (`true`),
/// as long as the number columns at that size leave the text columns the rest of [`COLUMN_FIT_SHARE`]
/// of the body; otherwise every column shares by its text (an amount may wrap, a description keeps
/// room to be read).
fn shares(longest: &[usize], numbers: &[bool], body: f64) -> (Vec<f64>, bool) {
    let shared = |chars: usize| chars.clamp(COLUMN_CHARS_MIN, COLUMN_CHARS_MAX) as f64;
    let fits =
        |i: usize| longest[i] <= COLUMN_FIT_CHARS || numbers.get(i).copied().unwrap_or(false);
    let fitted: f64 = (0..longest.len())
        .filter(|&i| fits(i))
        .map(|i| longest[i] as f64 * POINTS_PER_CHAR + COLUMN_PADDING_POINTS)
        .sum();
    let has_long = (0..longest.len()).any(|i| !fits(i));
    let number = |i: usize| numbers.get(i).copied().unwrap_or(false);
    if fitted > body * if has_long { COLUMN_FIT_SHARE } else { 1.0 } {
        let numbers_at_compact: f64 = (0..longest.len())
            .filter(|&i| number(i))
            .map(|i| longest[i] as f64 * POINTS_PER_CHAR * COMPACT_SCALE + COLUMN_PADDING_POINTS)
            .sum();
        let room = if (0..longest.len()).all(number) {
            f64::INFINITY
        } else {
            body * COLUMN_FIT_SHARE
        };
        let compact = (0..longest.len()).any(number) && numbers_at_compact <= room;
        let out = (0..longest.len())
            .map(|i| {
                if compact && number(i) {
                    0.0
                } else {
                    shared(longest[i])
                }
            })
            .collect();
        return (out, compact);
    }
    let mut out: Vec<f64> = (0..longest.len())
        .map(|i| {
            if fits(i) {
                0.0
            } else {
                longest[i].min(COLUMN_CHARS_MAX) as f64
            }
        })
        .collect();
    if out.iter().all(|&w| w == 0.0)
        && let Some(widest) = (0..longest.len()).max_by_key(|&i| longest[i])
    {
        out[widest] = longest[widest].max(COLUMN_CHARS_MIN) as f64;
    }
    (out, false)
}
/// Typed renderer input and the warnings of nodes printed differently from what they ask for.
/// Raster images are decoded here, before layout, so a corrupt image is an `Image` error and not a
/// layout diagnostic; the decoder call is memoized, so layout reuses this decode.
pub(crate) fn encode(doc: &DocTree, limits: &Limits) -> Result<(Dict, Vec<String>), ReportError> {
    let mut nodes = Vec::with_capacity(doc.nodes().len());
    let mut warnings = Vec::new();
    let mut pictures = Pictures::default();
    // Table column shares, and the font scale every node inherits from a compact table above it.
    // Parents come after their children in the arena, so a reverse walk reaches a table first.
    // `inherited` is the size a node's text gets from the nearest styled node above it, so a bare
    // text in a compact table prints at the table's size as the styled cells beside it do.
    // `width` is the width a node is laid out in (estimated for table columns), and `tall` marks the
    // cells of rows and the kept blocks estimated taller than [`TALL_SHARE`] of the page body.
    let count = doc.nodes().len();
    let mut tables = std::collections::HashMap::new();
    let mut scale = vec![1.0; count];
    let mut inherited = vec![Style::default().size(); count];
    let page = doc.options().page();
    let [top, right, bottom, left] = page.margins();
    let mut width = vec![page.width() - left - right; count];
    let limit = (page.height() - top - bottom) * TALL_SHARE;
    let mut tall = vec![false; count];
    for (index, node) in doc.nodes().iter().enumerate().rev() {
        let table = match node {
            Node::Table {
                columns,
                header,
                cells,
            } => Some(column_weights(doc, *columns, header, cells)),
            _ => None,
        };
        if table.as_ref().is_some_and(|table| table.compact) {
            scale[index] = COMPACT_SCALE;
        }
        let (own, outer) = (scale[index], width[index]);
        let size = match node {
            Node::Block { style, .. } | Node::Inline { style, .. } => style.size(),
            _ => inherited[index],
        };
        let inner = match node {
            Node::Block { style, .. } => (outer - 2.0 * style.padding()).max(0.0),
            _ => outer,
        };
        for child in node.children() {
            if let Some(s) = scale.get_mut(child.index()) {
                *s = f64::min(*s, own);
            }
            if let Some(s) = inherited.get_mut(child.index()) {
                *s = size;
            }
            if let Some(w) = width.get_mut(child.index()) {
                *w = inner;
            }
        }
        match (node, table) {
            (
                Node::Table {
                    columns,
                    header,
                    cells,
                },
                Some(table),
            ) => {
                let size = inherited[index] * own;
                // content-sized columns as wide as their longest text, the others share the rest
                let auto =
                    |i: usize| table.longest[i] as f64 * size * CHAR_EM + COLUMN_PADDING_POINTS;
                let fixed: f64 = (0..*columns)
                    .filter(|&i| table.weights[i] == 0.0)
                    .map(auto)
                    .sum();
                let shared: f64 = table.weights.iter().sum();
                let rest = (outer - fixed).max(0.0);
                let column = |i: usize| {
                    if table.weights[i] == 0.0 {
                        auto(i)
                    } else {
                        rest * table.weights[i] / shared
                    }
                };
                for (section, body) in [(header, false), (cells, true)] {
                    let placed = placements(doc, *columns, section);
                    let mut rows: Vec<f64> = Vec::new();
                    for (id, &(row, col, span, _)) in section.iter().zip(&placed) {
                        let cell = ((col..col + span).map(column).sum::<f64>()
                            - COLUMN_PADDING_POINTS)
                            .max(size);
                        width[id.index()] = cell;
                        if body {
                            if rows.len() <= row {
                                rows.resize(row + 1, 0.0);
                            }
                            rows[row] = rows[row].max(estimated_height(doc, id, cell, size));
                        }
                    }
                    if body {
                        for (id, &(row, ..)) in section.iter().zip(&placed) {
                            tall[id.index()] = rows[row] > limit;
                        }
                    }
                }
                tables.insert(index, (table.weights, table.header_rows));
            }
            (
                Node::Grid {
                    columns,
                    gutter,
                    cells,
                    ..
                },
                _,
            ) => {
                let total: f64 = columns.iter().sum();
                let free = (outer - gutter * columns.len().saturating_sub(1) as f64).max(0.0);
                for (i, id) in cells.iter().enumerate() {
                    let share = columns
                        .get(i % columns.len().max(1))
                        .copied()
                        .unwrap_or(0.0);
                    if total > 0.0 {
                        width[id.index()] = free * share / total;
                    }
                }
            }
            (Node::Block { style, children }, _) if style.keep_together() => {
                let height: f64 = children
                    .iter()
                    .map(|id| estimated_height(doc, id, inner.max(size), size * own))
                    .sum();
                tall[index] = height > limit;
            }
            _ => {}
        }
    }
    for (index, node) in doc.nodes().iter().enumerate() {
        let mut d = Dict::new();
        let kind = match node {
            Node::Flow { children } => {
                d.insert("children".into(), indices(children));
                "flow"
            }
            Node::Text(text) => {
                d.insert("text".into(), string(text));
                let size = if scale[index] < 1.0 {
                    Value::Float(inherited[index] * scale[index])
                } else {
                    Value::None
                };
                d.insert("size".into(), size);
                "text"
            }
            Node::Block { children, style } | Node::Inline { children, style } => {
                d.insert("children".into(), indices(children));
                styled(&mut d, style, scale[index])?;
                d.insert("tall".into(), Value::Bool(tall[index]));
                if matches!(node, Node::Block { .. }) {
                    "block"
                } else {
                    "inline"
                }
            }
            Node::Grid {
                columns,
                gutter,
                cells,
                align,
            } => {
                d.insert(
                    "columns".into(),
                    array(columns.iter().copied().map(Value::Float)),
                );
                d.insert("gutter".into(), Value::Float(*gutter));
                d.insert(
                    "align".into(),
                    string(match align {
                        CrossAlignment::Start => "start",
                        CrossAlignment::Center => "center",
                        CrossAlignment::End => "end",
                        _ => {
                            return Err(ReportError::Unsupported {
                                feature: "row alignment",
                                task: "a later release",
                            });
                        }
                    }),
                );
                d.insert("children".into(), indices(cells));
                "grid"
            }
            Node::Table {
                columns,
                header,
                cells,
            } => {
                let (weights, header_rows) = tables.remove(&index).unwrap_or_default();
                d.insert("columns".into(), Value::Int(*columns as i64));
                d.insert(
                    "weights".into(),
                    array(weights.into_iter().map(Value::Float)),
                );
                d.insert("header_rows".into(), Value::Int(header_rows as i64));
                d.insert(
                    "loose".into(),
                    array(cells.iter().map(|id| {
                        let cell = matches!(doc.nodes().get(id.index()), Some(Node::Cell { .. }));
                        Value::Bool(tall[id.index()] && !cell)
                    })),
                );
                d.insert("header".into(), indices(header));
                d.insert("children".into(), indices(cells));
                "table"
            }
            Node::List { ordered, items } => {
                d.insert("ordered".into(), Value::Bool(*ordered));
                d.insert("children".into(), indices(items));
                "list"
            }
            Node::Image {
                data,
                width,
                height,
                fit,
                alt,
            } => 'picture: {
                let info = probe_image(data, limits)?;
                // Decision 7: user SVG is parsed only by an isolated worker; its bytes
                // never reach this renderer.
                if info.format() == ImageFormat::UntrustedSvg {
                    warnings.push(format!(
                        "image node {index}: SVG from a user or upload is rendered only by the WorkerPool runner (a later release); printed as a placeholder"
                    ));
                    break 'picture placeholder(
                        &mut d,
                        width.unwrap_or(PLACEHOLDER_POINTS),
                        height.unwrap_or(PLACEHOLDER_POINTS),
                        alt,
                    );
                }
                // The box of the picture, in points: the sides given, a side that is not given
                // following the picture's proportions, and its natural size when none is.
                let natural = (
                    f64::from(info.width()) * POINTS_PER_PIXEL,
                    f64::from(info.height()) * POINTS_PER_PIXEL,
                );
                let (wide, high) = match (*width, *height) {
                    (Some(wide), Some(high)) => (wide, high),
                    (Some(wide), None) => (wide, wide * natural.1 / natural.0),
                    (None, Some(high)) => (high * natural.0 / natural.1, high),
                    (None, None) => natural,
                };
                let cover = match fit {
                    ImageFit::Contain => false,
                    ImageFit::Cover => true,
                    _ => {
                        return Err(ReportError::Unsupported {
                            feature: "picture fit",
                            task: "a later release",
                        });
                    }
                };
                // Scaled down to the pixels the box needs, converted when no reader shows its
                // coding, then decoded once here, before layout: a picture the decoder refuses
                // prints as an empty box with a warning, never as a failed document. The decoder
                // call is memoized, so layout reuses this decode.
                let options = doc.options().pictures();
                let ready = pictures
                    .embed(data, &info, (wide, high), *fit, options, limits)
                    .and_then(|(bytes, format)| {
                        let format = match format {
                            ImageFormat::Png => ExchangeFormat::Png,
                            ImageFormat::Jpeg => ExchangeFormat::Jpg,
                            ImageFormat::Gif => ExchangeFormat::Gif,
                            ImageFormat::WebP => ExchangeFormat::Webp,
                            _ => return Err("its format is not one the renderer prints".into()),
                        };
                        let bytes = Bytes::new(bytes);
                        let raster = RasterImage::new(bytes.clone(), format, Smart::Auto)
                            .map_err(|e| format!("the decoder cannot read it: {e}"))?;
                        Ok((bytes, format, raster.width() >= raster.height()))
                    });
                let (bytes, format, landscape) = match ready {
                    Ok(ready) => ready,
                    Err(reason) => {
                        warnings.push(format!(
                            "image node {index}: {reason}; an empty box is printed"
                        ));
                        break 'picture placeholder(&mut d, wide, high, alt);
                    }
                };
                // The size the picture is drawn at inside its box, from the proportions of its
                // header: scaling a picture down to whole pixels shifts the proportions of the
                // pixels a little, and the picture still fills the place it was given. A decoder
                // that turned the picture a quarter where the header did not say so (an
                // orientation in a PNG or WebP) gives the turned proportions.
                let (across, down) = if landscape == (info.width() >= info.height()) {
                    natural
                } else {
                    (natural.1, natural.0)
                };
                let scale = if cover {
                    f64::max(wide / across, high / down)
                } else {
                    f64::min(wide / across, high / down)
                };
                let fitted = (across * scale, down * scale);
                // within a hundredth of a point the picture is its box: no box is drawn around it
                let exact = (fitted.0 - wide).abs() < 0.01 && (fitted.1 - high).abs() < 0.01;
                d.insert("data".into(), Value::Bytes(bytes));
                d.insert(
                    "format".into(),
                    string(match format {
                        ExchangeFormat::Png => "png",
                        ExchangeFormat::Jpg => "jpg",
                        ExchangeFormat::Gif => "gif",
                        _ => "webp",
                    }),
                );
                d.insert(
                    "fitted".into(),
                    array([fitted.0, fitted.1].map(Value::Float)),
                );
                d.insert("exact".into(), Value::Bool(exact));
                d.insert("clip".into(), Value::Bool(cover));
                sized(&mut d, Some(wide), Some(high), alt);
                "image"
            }
            Node::Placeholder {
                width,
                height,
                alt,
                reason,
            } => {
                warnings.push(reason.clone());
                placeholder(&mut d, *width, *height, alt)
            }
            Node::Vector {
                vector,
                width,
                height,
                alt,
                caption,
            } => {
                d.insert(
                    "data".into(),
                    Value::Bytes(Bytes::new(vector.svg().clone())),
                );
                // One scale both ways: a box of another proportion is fitted into, never filled.
                let (wide, high) = vector_size(vector, *width, *height);
                sized(&mut d, Some(wide), Some(high), alt);
                match caption {
                    Some(caption) => {
                        d.insert("caption".into(), string(caption));
                        d.insert(
                            "caption_size".into(),
                            Value::Float(caption_size(caption, wide)),
                        );
                    }
                    None => {
                        d.insert("caption".into(), Value::None);
                    }
                }
                "vector"
            }
            Node::Cell {
                child,
                colspan,
                rowspan,
            } => {
                d.insert("children".into(), indices(std::slice::from_ref(child)));
                d.insert("colspan".into(), Value::Int(*colspan as i64));
                d.insert("rowspan".into(), Value::Int(*rowspan as i64));
                d.insert("breakable".into(), Value::Bool(tall[index]));
                "cell"
            }
            Node::Rule => "rule",
            Node::LineBreak => "linebreak",
            Node::PageBreak => "pagebreak",
            Node::PageCounter(PageCounter::Current) => "current",
            Node::PageCounter(PageCounter::Total) => "total",
            _ => {
                return Err(ReportError::Unsupported {
                    feature: "document node",
                    task: "a later release",
                });
            }
        };
        d.insert("kind".into(), string(kind));
        nodes.push(Value::Dict(d));
    }
    let mut input = Dict::new();
    input.insert("nodes".into(), array(nodes));
    input.insert("body".into(), Value::Int(doc.body().index() as i64));
    input.insert(
        "header".into(),
        doc.header()
            .map_or(Value::None, |n| Value::Int(n.index() as i64)),
    );
    input.insert(
        "footer".into(),
        doc.footer()
            .map_or(Value::None, |n| Value::Int(n.index() as i64)),
    );
    let o = doc.options();
    input.insert("title".into(), string(o.title()));
    input.insert("lang".into(), string(o.language()));
    input.insert("fonts".into(), array(o.fonts().iter().map(|s| string(s))));
    input.insert("width".into(), Value::Float(o.page().width()));
    input.insert("height".into(), Value::Float(o.page().height()));
    let [top, right, bottom, left] = o.page().margins();
    input.insert(
        "margins".into(),
        array([top, right, bottom, left].map(Value::Float)),
    );
    Ok((input, warnings))
}

#[cfg(test)]
mod tests {
    use super::shares;

    /// Short columns fit their content and long ones share the rest of the width.
    #[test]
    fn short_columns_fit_and_long_columns_share() {
        // A4 body with 12 mm sides
        let body = 527.0;
        // Title, Author, Format, Year, Status of the sample Book List
        assert_eq!(
            shares(&[25, 22, 9, 4, 9], &[false; 5], body),
            (vec![25.0, 22.0, 0.0, 0.0, 0.0], false)
        );
        assert_eq!(
            shares(&[90, 17], &[false; 2], body),
            (vec![40.0, 17.0], false)
        );
        // only short columns: the widest takes the rest of the page
        assert_eq!(
            shares(&[4, 9, 2], &[false; 3], body),
            (vec![0.0, 9.0, 0.0], false)
        );
        assert_eq!(shares(&[], &[], body), (Vec::<f64>::new(), false));
        // seven 12-character amounts beside a partner name would leave the name almost no room: shared
        assert_eq!(
            shares(&[30, 12, 12, 12, 12, 12, 12, 12], &[false; 8], body),
            (vec![30.0, 12.0, 12.0, 12.0, 12.0, 12.0, 12.0, 12.0], false)
        );
        // the same seven columns marked as numbers would leave the name less than its share of the
        // body even one size smaller: every column shares by its text
        assert_eq!(
            shares(
                &[30, 12, 12, 12, 12, 12, 12, 12],
                &[false, true, true, true, true, true, true, true],
                body
            ),
            (vec![30.0, 12.0, 12.0, 12.0, 12.0, 12.0, 12.0, 12.0], false)
        );
        // four amounts that fit the number share only one size smaller: they keep their width and
        // the table steps down a size
        assert_eq!(
            shares(
                &[40, 17, 17, 17, 17],
                &[false, true, true, true, true],
                body
            ),
            (vec![40.0, 0.0, 0.0, 0.0, 0.0], true)
        );
        // amounts alone have no text column to keep room for: they step down
        assert_eq!(shares(&[20; 6], &[true; 6], body), (vec![0.0; 6], true));
        // six 16-character columns alone overflow the body: shared, never wider than the page
        assert_eq!(shares(&[16; 6], &[false; 6], body), (vec![16.0; 6], false));
        // an invoice: description, quantity, unit price, discount, taxes, amount; the number
        // columns fit their longest amount, the text columns share the rest
        assert_eq!(
            shares(
                &[66, 14, 16, 7, 18, 20],
                &[false, true, true, true, false, true],
                body
            ),
            (vec![40.0, 0.0, 0.0, 0.0, 18.0, 0.0], false)
        );
        // the same amounts without the number mark: the long amount column shares and may wrap
        assert_eq!(
            shares(&[66, 14, 16, 7, 18, 20], &[false; 6], body),
            (vec![40.0, 0.0, 0.0, 0.0, 18.0, 20.0], false)
        );
    }
}
