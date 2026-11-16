// SPDX-License-Identifier: MIT OR Apache-2.0
use erp_report::{
    Alignment, DocTree, ImageFormat, Limits, Node, NodeId, PageCounter, ReportError, Style,
    TableLook, probe_image,
};
use typst::{
    foundations::{Array, Bytes, Dict, Smart, Value},
    visualize::{ExchangeFormat, RasterImage},
};
/// Side of the placeholder box of an image without a requested size, in points.
const PLACEHOLDER_POINTS: f64 = 72.0;
fn points(value: Option<f64>) -> Value {
    value.map_or(Value::Auto, Value::Float)
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
fn styled(dict: &mut Dict, style: &Style) -> Result<(), ReportError> {
    dict.insert("size".into(), Value::Float(style.size()));
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
        Some(Node::Table { .. } | Node::Grid { .. }) => COLUMN_CHARS_MAX,
        _ => 0,
    }
}
/// Column shares from the longest text in each column, header included, as a browser's automatic table
/// layout gives them: a title column gets more room than a year column. A spanning cell counts toward the
/// column it starts in.
fn column_weights(doc: &DocTree, columns: usize, header: &[NodeId], cells: &[NodeId]) -> Vec<f64> {
    let mut longest = vec![0; columns];
    for (index, id) in header.iter().chain(cells).enumerate() {
        if let Some(column) = longest.get_mut(index % columns.max(1)) {
            *column = (*column).max(text_len(doc, id, 0));
        }
    }
    longest
        .into_iter()
        .map(|chars: usize| chars.clamp(COLUMN_CHARS_MIN, COLUMN_CHARS_MAX) as f64)
        .collect()
}
/// Typed renderer input and the warnings of nodes printed differently from what they ask for.
/// Raster images are decoded here, before layout, so a corrupt image is an `Image` error and not a
/// layout diagnostic; the decoder call is memoized, so layout reuses this decode.
pub(crate) fn encode(doc: &DocTree, limits: &Limits) -> Result<(Dict, Vec<String>), ReportError> {
    let mut nodes = Vec::with_capacity(doc.nodes().len());
    let mut warnings = Vec::new();
    for (index, node) in doc.nodes().iter().enumerate() {
        let mut d = Dict::new();
        let kind = match node {
            Node::Flow { children } => {
                d.insert("children".into(), indices(children));
                "flow"
            }
            Node::Text(text) => {
                d.insert("text".into(), string(text));
                "text"
            }
            Node::Block { children, style } | Node::Inline { children, style } => {
                d.insert("children".into(), indices(children));
                styled(&mut d, style)?;
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
            } => {
                d.insert(
                    "columns".into(),
                    array(columns.iter().copied().map(Value::Float)),
                );
                d.insert("gutter".into(), Value::Float(*gutter));
                d.insert("children".into(), indices(cells));
                "grid"
            }
            Node::Table {
                columns,
                header,
                cells,
            } => {
                d.insert("columns".into(), Value::Int(*columns as i64));
                d.insert(
                    "weights".into(),
                    array(
                        column_weights(doc, *columns, header, cells)
                            .into_iter()
                            .map(Value::Float),
                    ),
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
                alt,
            } => {
                let format = match probe_image(data, limits)?.format() {
                    ImageFormat::Png => ExchangeFormat::Png,
                    ImageFormat::Jpeg => ExchangeFormat::Jpg,
                    ImageFormat::Gif => ExchangeFormat::Gif,
                    ImageFormat::WebP => ExchangeFormat::Webp,
                    // Decision 7: user SVG is parsed only by an isolated worker; its bytes
                    // never reach this renderer.
                    ImageFormat::UntrustedSvg => {
                        warnings.push(format!(
                            "image node {index}: SVG from a user or upload is rendered only by the WorkerPool runner (a later release); printed as a placeholder"
                        ));
                        sized(
                            &mut d,
                            Some(width.unwrap_or(PLACEHOLDER_POINTS)),
                            Some(height.unwrap_or(PLACEHOLDER_POINTS)),
                            alt,
                        );
                        d.insert("kind".into(), string("placeholder"));
                        nodes.push(Value::Dict(d));
                        continue;
                    }
                    _ => {
                        return Err(ReportError::Unsupported {
                            feature: "image format",
                            task: "a later release",
                        });
                    }
                };
                let bytes = Bytes::new(data.clone());
                RasterImage::new(bytes.clone(), format, Smart::Auto).map_err(|e| {
                    ReportError::Image {
                        reason: format!("image node {index} cannot be decoded: {e}"),
                    }
                })?;
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
                sized(&mut d, *width, *height, alt);
                "image"
            }
            Node::Vector {
                vector,
                width,
                height,
                alt,
            } => {
                d.insert(
                    "data".into(),
                    Value::Bytes(Bytes::new(vector.svg().clone())),
                );
                sized(&mut d, *width, *height, alt);
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
    input.insert("margin".into(), Value::Float(o.page().margin()));
    Ok((input, warnings))
}
