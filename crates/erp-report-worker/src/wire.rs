// SPDX-License-Identifier: MIT OR Apache-2.0
//! The messages of the frame protocol. A document crosses the pipe as plain values and is admitted
//! again by the worker through `DocumentBuilder`: the worker trusts no frame.
use erp_report::{
    Alignment, CrossAlignment, DocTree, DocumentBuilder, DocumentOptions, ImageFit, LayoutFailure,
    LimitKind, Limits, Node, NodeId, PageCounter, PageSetup, PdfStandard, PictureQuality,
    RenderDate, ReportError, RichText, Style, TableLook, Vector,
};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// The task that adds the frames and document nodes this version does not carry.
const LATER: &str = "a later release";
/// Longest text of one failure or warning sent back; the rest is cut and marked.
const TEXT_BYTES: usize = 64 * 1024;
/// Most warnings sent back with one document; a last entry counts the rest.
const WARNINGS: usize = 256;

/// What the pool sends.
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Request<'a> {
    /// First frame of a worker's life.
    Hello(Hello),
    /// Lay out one document; answered by `Rendered` and a PDF frame, or by `Failed`.
    Render(WireDocument<'a>),
    /// Report what the worker can see of its own isolation.
    Diag,
    /// Exit with status 0.
    Shutdown,
}

/// Start message: everything a worker knows besides its three environment variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Hello {
    pub protocol: u32,
    /// The shipped font store, an absolute path.
    pub font_directory: String,
    /// `(package name, absolute path)` of each optional font package installed on the node.
    pub optional_fonts: Vec<(String, String)>,
    /// `(limit name, value)` for every document limit, lowered from the defaults.
    pub limits: Vec<(String, u64)>,
    /// Bytes of address space allowed beyond the memory cap and the size at start.
    pub address_space_headroom: u64,
}
impl Hello {
    pub fn new(
        font_directory: String,
        optional_fonts: Vec<(String, String)>,
        limits: Limits,
        address_space_headroom: u64,
    ) -> Self {
        Self {
            protocol: crate::PROTOCOL_VERSION,
            font_directory,
            optional_fonts,
            limits: LimitKind::ALL
                .into_iter()
                .map(|kind| (kind.to_string(), limits.get(kind) as u64))
                .collect(),
            address_space_headroom,
        }
    }
    /// The limits named by the frame. An unknown name, a zero or a value above the default is refused.
    pub fn limits(&self) -> Result<Limits, ReportError> {
        let mut limits = Limits::default();
        for (name, value) in &self.limits {
            let kind = LimitKind::from_name(name).ok_or(ReportError::InvalidDocument {
                reason: "unknown report limit in the worker start message",
            })?;
            let value = usize::try_from(*value).map_err(|_| ReportError::InvalidLimit {
                kind,
                value: usize::MAX,
                max: limits.get(kind),
            })?;
            limits = limits.lowered(kind, value)?;
        }
        Ok(limits)
    }
}

/// What a worker answers.
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Response {
    /// The engine is built and the worker reads requests.
    Ready {
        protocol: u32,
        address_space: CapState,
    },
    /// The next frame holds the PDF bytes.
    Rendered {
        pages: u64,
        warnings: Vec<String>,
        image_failures: Vec<(u64, String)>,
        renderer: String,
    },
    /// The document (or, before `Ready`, the start message) was refused; the worker lives on after a
    /// refused document.
    Failed(WireFailure),
    Diag(DiagReport),
}

/// Admit untrusted response diagnostics through core constructors before they reach a result.
pub(crate) fn image_failures_from_wire(
    failures: Vec<(u64, String)>,
    limits: Limits,
) -> Result<Vec<erp_report::ImageFailure>, ReportError> {
    limits.check(LimitKind::Images, failures.len())?;
    failures
        .into_iter()
        .map(|(node, reason)| {
            let node = usize::try_from(node).map_err(|_| ReportError::InvalidDocument {
                reason: "image failure node index is not representable",
            })?;
            erp_report::ImageFailure::new(node, reason, limits)
        })
        .collect()
}

/// Whether a protection is in force, said by the side that applies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CapState {
    Applied { detail: String },
    Unavailable { reason: String },
}

/// What a worker sees of itself (design `reports-pdf.md` §6.1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DiagReport {
    pub protocol: u32,
    pub process_id: u32,
    /// Names of the worker's environment variables, sorted.
    pub environment: Vec<String>,
    /// Open descriptors, counted in `/proc/self/fd`; `None` where the system has no such directory.
    pub open_descriptors: Option<u64>,
    pub working_directory: String,
    /// Entries of the working directory; `None` when it cannot be listed.
    pub working_directory_entries: Option<u64>,
    pub resident_bytes: Option<u64>,
    pub memory_cap: u64,
    pub address_space: CapState,
    pub landlock: CapState,
    pub seccomp: CapState,
}

/// A refusal in a form that crosses the pipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WireFailure {
    DocumentLimit {
        kind: String,
        actual: u64,
        max: u64,
    },
    InvalidLimit {
        kind: String,
        value: u64,
        max: u64,
    },
    Font {
        reason: String,
    },
    Image {
        reason: String,
    },
    Layout {
        panic: bool,
        reason: String,
    },
    Pdf {
        reason: String,
    },
    /// Every other kind, as the worker's own error text.
    Other {
        message: String,
    },
}
impl WireFailure {
    pub(crate) fn from_error(error: &ReportError) -> Self {
        match error {
            ReportError::DocumentLimit { kind, actual, max } => Self::DocumentLimit {
                kind: kind.to_string(),
                actual: *actual as u64,
                max: *max as u64,
            },
            ReportError::InvalidLimit { kind, value, max } => Self::InvalidLimit {
                kind: kind.to_string(),
                value: *value as u64,
                max: *max as u64,
            },
            ReportError::Font { reason, source } => Self::Font {
                reason: clip(match source {
                    Some(source) => format!("{reason}: {source}"),
                    None => reason.clone(),
                }),
            },
            ReportError::Image { reason } => Self::Image {
                reason: clip(reason.clone()),
            },
            ReportError::Layout { kind, reason, .. } => Self::Layout {
                panic: matches!(kind, LayoutFailure::Panic),
                reason: clip(reason.clone()),
            },
            ReportError::Pdf { reason, .. } => Self::Pdf {
                reason: clip(reason.clone()),
            },
            other => Self::Other {
                message: clip(other.to_string()),
            },
        }
    }
    /// The error the caller of the pool gets: the same kind an in-process engine returns wherever
    /// the kind owns its data.
    pub(crate) fn into_error(self, document: &str) -> ReportError {
        let refused = |message: String| ReportError::WorkerRefused {
            document: document.into(),
            message,
        };
        match self {
            Self::DocumentLimit { kind, actual, max } => {
                match (
                    LimitKind::from_name(&kind),
                    usize::try_from(actual),
                    usize::try_from(max),
                ) {
                    (Some(kind), Ok(actual), Ok(max)) => {
                        ReportError::DocumentLimit { kind, actual, max }
                    }
                    _ => refused(format!("report limit {kind} exceeded: {actual} > {max}")),
                }
            }
            Self::InvalidLimit { kind, value, max } => {
                match (
                    LimitKind::from_name(&kind),
                    usize::try_from(value),
                    usize::try_from(max),
                ) {
                    (Some(kind), Ok(value), Ok(max)) => {
                        ReportError::InvalidLimit { kind, value, max }
                    }
                    _ => refused(format!(
                        "report limit {kind} must be in 1..={max}; got {value}"
                    )),
                }
            }
            Self::Font { reason } => ReportError::Font {
                reason,
                source: None,
            },
            Self::Image { reason } => ReportError::Image { reason },
            Self::Layout { panic, reason } => ReportError::Layout {
                document: document.into(),
                kind: if panic {
                    LayoutFailure::Panic
                } else {
                    LayoutFailure::Diagnostic
                },
                reason,
            },
            Self::Pdf { reason } => ReportError::Pdf {
                document: document.into(),
                reason,
            },
            Self::Other { message } => refused(message),
        }
    }
    /// One line for a start failure's cause.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::DocumentLimit { kind, actual, max } => {
                format!("report limit {kind} exceeded: {actual} > {max}")
            }
            Self::InvalidLimit { kind, value, max } => {
                format!("report limit {kind} must be in 1..={max}; got {value}")
            }
            Self::Font { reason } => format!("report font: {reason}"),
            Self::Image { reason } => format!("report image: {reason}"),
            Self::Layout { reason, .. } | Self::Pdf { reason } => reason.clone(),
            Self::Other { message } => message.clone(),
        }
    }
}

/// Cut a text at a character boundary and say so.
pub(crate) fn clip(mut text: String) -> String {
    if text.len() > TEXT_BYTES {
        let mut end = TEXT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(" [cut]");
    }
    text
}
/// The warnings that travel with a document: each cut to size, and a count in place of the tail.
pub(crate) fn clip_warnings(warnings: &[String]) -> Vec<String> {
    let mut sent: Vec<String> = warnings
        .iter()
        .take(WARNINGS)
        .map(|warning| clip(warning.clone()))
        .collect();
    if warnings.len() > WARNINGS {
        sent.push(format!(
            "{} more warnings were not sent by the render worker",
            warnings.len() - WARNINGS
        ));
    }
    sent
}

/// Raw bytes sent as one CBOR byte string (serde's default would send an array of integers).
#[derive(Debug)]
struct WireBytes<'a>(Cow<'a, [u8]>);

impl Serialize for WireBytes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for WireBytes<'_> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl serde::de::Visitor<'_> for Bytes {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a byte string")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                Ok(v.to_vec())
            }
            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(v)
            }
        }
        deserializer
            .deserialize_byte_buf(Bytes)
            .map(|v| WireBytes(Cow::Owned(v)))
    }
}

/// A document as plain values. Short variant names: a document can hold 500,000 nodes.
#[derive(Debug, Serialize, Deserialize)]
pub struct WireDocument<'a> {
    nodes: Vec<WireNode<'a>>,
    body: u32,
    header: Option<u32>,
    footer: Option<u32>,
    options: WireOptions<'a>,
}
#[derive(Debug, Serialize, Deserialize)]
enum WireNode<'a> {
    #[serde(rename = "f")]
    Flow(Vec<u32>),
    #[serde(rename = "t")]
    Text(Cow<'a, str>),
    #[serde(rename = "b")]
    Block(Vec<u32>, WireStyle),
    #[serde(rename = "i")]
    Inline(Vec<u32>, WireStyle),
    #[serde(rename = "g")]
    Grid(Vec<f64>, f64, Vec<u32>, u8),
    #[serde(rename = "a")]
    Table(u64, Vec<u32>, Vec<u32>),
    #[serde(rename = "l")]
    List(bool, Vec<u32>),
    #[serde(rename = "r")]
    Rule,
    #[serde(rename = "n")]
    LineBreak,
    #[serde(rename = "p")]
    PageBreak,
    #[serde(rename = "c")]
    PageCurrent,
    #[serde(rename = "o")]
    PageTotal,
    /// Picture bytes (a CBOR byte string), width and height in points, alternative text, fit
    /// (0 contain, 1 cover; protocol 8).
    #[serde(rename = "m")]
    Image(WireBytes<'a>, Option<f64>, Option<f64>, Cow<'a, str>, u8),
    /// The box of a picture that could not be read: width and height in points, alternative text
    /// and the reason returned as a warning (protocol 8).
    #[serde(rename = "x")]
    Placeholder(f64, f64, Cow<'a, str>, Cow<'a, str>),
    /// A table cell: its child, colspan and rowspan (protocol 6). The builder admits the spans again.
    #[serde(rename = "s")]
    Cell(u32, u64, u64),
    /// A generated graphic, a barcode or a QR code (protocol 9), as the numbers it is made from: box
    /// width and height, rectangles `[x, y, width, height]`, colour, printed width and height in
    /// points, alternative text, the text under it. Never markup: the worker writes its own from the
    /// numbers, under the same checks as the server.
    #[serde(rename = "v")]
    Vector(
        f64,
        f64,
        Cow<'a, [[f64; 4]]>,
        [u8; 3],
        Option<f64>,
        Option<f64>,
        Cow<'a, str>,
        Option<Cow<'a, str>>,
    ),
}
/// size, bold, italic, underline, colour, padding, alignment (0 start, 1 centre, 2 end), keep together,
/// space above and below, fill, rules above and below, table look (0 lined, 1 bordered, 2 borderless),
/// strike, baseline, preformatted, direction, tracking, font, safe link (protocol 10)
#[derive(Debug, Serialize, Deserialize)]
struct WireStyle(
    f64,
    bool,
    bool,
    bool,
    [u8; 3],
    f64,
    u8,
    bool,
    (f64, f64),
    Option<[u8; 3]>,
    (bool, bool),
    u8,
    bool,
    i8,
    bool,
    Option<bool>,
    f64,
    String,
    String,
);
#[derive(Debug, Serialize, Deserialize)]
struct WireOptions<'a> {
    id: Cow<'a, str>,
    title: Cow<'a, str>,
    /// width, height, margins (top, right, bottom, left)
    page: (f64, f64, [f64; 4]),
    language: Cow<'a, str>,
    fonts: Vec<Cow<'a, str>>,
    /// year, month, day
    date: (i32, u8, u8),
    /// 0 PDF 1.7, 1 PDF/A-3b
    standard: u8,
    /// Resolution pictures are embedded at (pixels per inch) and JPEG quality (protocol 8).
    pictures: (u16, u8),
}

fn later(feature: &'static str) -> ReportError {
    ReportError::Unsupported {
        feature,
        task: LATER,
    }
}
fn index(id: &NodeId) -> Result<u32, ReportError> {
    u32::try_from(id.index()).map_err(|_| ReportError::DocumentLimit {
        kind: LimitKind::Nodes,
        actual: id.index(),
        max: u32::MAX as usize,
    })
}
fn indices(ids: &[NodeId]) -> Result<Vec<u32>, ReportError> {
    ids.iter().map(index).collect()
}
fn wire_style(style: &Style) -> Result<WireStyle, ReportError> {
    Ok(WireStyle(
        style.size(),
        style.bold(),
        style.italic(),
        style.underline(),
        style.color(),
        style.padding(),
        match style.alignment() {
            Alignment::Start => 0,
            Alignment::Center => 1,
            Alignment::End => 2,
            _ => return Err(later("text alignment over the render worker protocol")),
        },
        style.keep_together(),
        style.spacing(),
        style.fill(),
        style.rules(),
        match style.table_look() {
            TableLook::Lined => 0,
            TableLook::Bordered => 1,
            TableLook::Borderless => 2,
            _ => return Err(later("table look over the render worker protocol")),
        },
        style.rich().strike(),
        style.rich().baseline(),
        style.rich().preformatted(),
        style.rich().rtl(),
        style.rich().tracking(),
        style.rich().font().to_owned(),
        style.rich().link().to_owned(),
    ))
}
fn style(wire: WireStyle) -> Result<Style, ReportError> {
    let WireStyle(
        size,
        bold,
        italic,
        underline,
        color,
        padding,
        alignment,
        keep,
        (above, below),
        fill,
        (rule_above, rule_below),
        table,
        strike,
        baseline,
        preformatted,
        rtl,
        tracking,
        font,
        link,
    ) = wire;
    let rich = RichText::default()
        .with_strike(strike)
        .with_baseline(baseline)?
        .with_preformatted(preformatted)
        .with_rtl(rtl)
        .with_tracking(tracking)?
        .with_font(&font)?
        .with_link(&link)?;
    Ok(Style::default()
        .with_rich(rich)
        .with_size(size)?
        .with_padding(padding)?
        .with_bold(bold)
        .with_italic(italic)
        .with_underline(underline)
        .with_color(color)
        .with_alignment(match alignment {
            0 => Alignment::Start,
            1 => Alignment::Center,
            2 => Alignment::End,
            _ => {
                return Err(ReportError::InvalidDocument {
                    reason: "unknown text alignment in a document frame",
                });
            }
        })
        .with_keep_together(keep)
        .with_spacing(above, below)?
        .with_fill(fill)
        .with_rules(rule_above, rule_below)
        .with_table_look(match table {
            0 => TableLook::Lined,
            1 => TableLook::Bordered,
            2 => TableLook::Borderless,
            _ => {
                return Err(ReportError::InvalidDocument {
                    reason: "unknown table look in a document frame",
                });
            }
        }))
}

impl<'a> WireDocument<'a> {
    /// The frame form of a document. Text is borrowed, not copied.
    pub fn from_tree(tree: &'a DocTree) -> Result<Self, ReportError> {
        let mut nodes = Vec::with_capacity(tree.nodes().len());
        for node in tree.nodes() {
            nodes.push(match node {
                Node::Flow { children } => WireNode::Flow(indices(children)?),
                Node::Text(text) => WireNode::Text(Cow::Borrowed(text)),
                Node::Block { children, style } => {
                    WireNode::Block(indices(children)?, wire_style(style)?)
                }
                Node::Inline { children, style } => {
                    WireNode::Inline(indices(children)?, wire_style(style)?)
                }
                Node::Grid {
                    columns,
                    gutter,
                    cells,
                    align,
                } => WireNode::Grid(
                    columns.clone(),
                    *gutter,
                    indices(cells)?,
                    match align {
                        CrossAlignment::Start => 0,
                        CrossAlignment::Center => 1,
                        CrossAlignment::End => 2,
                        _ => return Err(later("row alignment over the render worker protocol")),
                    },
                ),
                Node::Table {
                    columns,
                    header,
                    cells,
                } => WireNode::Table(*columns as u64, indices(header)?, indices(cells)?),
                Node::List { ordered, items } => WireNode::List(*ordered, indices(items)?),
                Node::Rule => WireNode::Rule,
                Node::LineBreak => WireNode::LineBreak,
                Node::PageBreak => WireNode::PageBreak,
                Node::PageCounter(PageCounter::Current) => WireNode::PageCurrent,
                Node::PageCounter(PageCounter::Total) => WireNode::PageTotal,
                Node::Image {
                    data,
                    width,
                    height,
                    fit,
                    alt,
                } => WireNode::Image(
                    WireBytes(Cow::Borrowed(data)),
                    *width,
                    *height,
                    Cow::Borrowed(alt),
                    match fit {
                        ImageFit::Contain => 0,
                        ImageFit::Cover => 1,
                        _ => return Err(later("picture fit over the render worker protocol")),
                    },
                ),
                Node::Placeholder {
                    width,
                    height,
                    alt,
                    reason,
                } => WireNode::Placeholder(
                    *width,
                    *height,
                    Cow::Borrowed(alt),
                    Cow::Borrowed(reason),
                ),
                Node::Cell {
                    child,
                    colspan,
                    rowspan,
                } => WireNode::Cell(index(child)?, *colspan as u64, *rowspan as u64),
                Node::Vector {
                    vector,
                    width,
                    height,
                    alt,
                    caption,
                } => WireNode::Vector(
                    vector.width(),
                    vector.height(),
                    Cow::Borrowed(vector.rectangle_list()),
                    vector.color(),
                    *width,
                    *height,
                    Cow::Borrowed(alt),
                    caption.as_deref().map(Cow::Borrowed),
                ),
                _ => {
                    return Err(later(
                        "document node of a newer kind over the render worker protocol",
                    ));
                }
            });
        }
        let options = tree.options();
        let (year, month, day) = options.date().parts();
        Ok(Self {
            nodes,
            body: index(tree.body())?,
            header: tree.header().map(index).transpose()?,
            footer: tree.footer().map(index).transpose()?,
            options: WireOptions {
                id: Cow::Borrowed(options.id()),
                title: Cow::Borrowed(options.title()),
                page: (
                    options.page().width(),
                    options.page().height(),
                    options.page().margins(),
                ),
                language: Cow::Borrowed(options.language()),
                fonts: options
                    .fonts()
                    .iter()
                    .map(|font| Cow::Borrowed(font.as_str()))
                    .collect(),
                date: (year, month, day),
                standard: match options.standard() {
                    PdfStandard::Pdf17 => 0,
                    PdfStandard::PdfA3b => 1,
                    _ => return Err(later("PDF standard over the render worker protocol")),
                },
                pictures: (options.pictures().dpi(), options.pictures().jpeg_quality()),
            },
        })
    }

    /// Admit the frame as a document under `limits`, node by node, exactly as a document built in
    /// this process is admitted.
    pub fn into_tree(self, limits: Limits) -> Result<DocTree, ReportError> {
        fn resolve(ids: &[NodeId], wire: &[u32]) -> Result<Vec<NodeId>, ReportError> {
            wire.iter().map(|index| one(ids, *index)).collect()
        }
        fn one(ids: &[NodeId], index: u32) -> Result<NodeId, ReportError> {
            ids.get(index as usize)
                .cloned()
                .ok_or(ReportError::InvalidDocument {
                    reason: "node must precede its parent",
                })
        }
        let mut builder = DocumentBuilder::new(limits);
        // Not sized from the frame: the builder's node limit bounds the growth.
        let mut ids: Vec<NodeId> = Vec::new();
        for node in self.nodes {
            let node = match node {
                WireNode::Flow(children) => Node::Flow {
                    children: resolve(&ids, &children)?,
                },
                WireNode::Text(text) => Node::Text(text.into_owned()),
                WireNode::Block(children, wire) => Node::Block {
                    children: resolve(&ids, &children)?,
                    style: style(wire)?,
                },
                WireNode::Inline(children, wire) => Node::Inline {
                    children: resolve(&ids, &children)?,
                    style: style(wire)?,
                },
                WireNode::Grid(columns, gutter, cells, align) => Node::Grid {
                    columns,
                    gutter,
                    cells: resolve(&ids, &cells)?,
                    align: match align {
                        0 => CrossAlignment::Start,
                        1 => CrossAlignment::Center,
                        2 => CrossAlignment::End,
                        _ => {
                            return Err(ReportError::InvalidDocument {
                                reason: "unknown row alignment in a document frame",
                            });
                        }
                    },
                },
                WireNode::Table(columns, header, cells) => Node::Table {
                    columns: usize::try_from(columns).map_err(|_| {
                        ReportError::InvalidDocument {
                            reason: "table column count out of range in a document frame",
                        }
                    })?,
                    header: resolve(&ids, &header)?,
                    cells: resolve(&ids, &cells)?,
                },
                WireNode::List(ordered, items) => Node::List {
                    ordered,
                    items: resolve(&ids, &items)?,
                },
                WireNode::Rule => Node::Rule,
                WireNode::LineBreak => Node::LineBreak,
                WireNode::PageBreak => Node::PageBreak,
                WireNode::PageCurrent => Node::PageCounter(PageCounter::Current),
                WireNode::PageTotal => Node::PageCounter(PageCounter::Total),
                // the builder probes the picture against the limits again: the worker trusts no frame
                WireNode::Image(data, width, height, alt, fit) => Node::Image {
                    data: data.0.into_owned().into(),
                    width,
                    height,
                    fit: match fit {
                        0 => ImageFit::Contain,
                        1 => ImageFit::Cover,
                        _ => {
                            return Err(ReportError::InvalidDocument {
                                reason: "unknown picture fit in a document frame",
                            });
                        }
                    },
                    alt: alt.into_owned(),
                },
                // sizes and the length of the reason are checked by the builder, as in process
                WireNode::Placeholder(width, height, alt, reason) => Node::Placeholder {
                    width,
                    height,
                    alt: alt.into_owned(),
                    reason: reason.into_owned(),
                },
                // The graphic is made again from its numbers: box, count, finiteness and "inside the
                // box" are checked by `Vector::rectangles`, the printed size and the lowered
                // rectangle limit by the builder.
                WireNode::Vector(wide, high, rectangles, color, width, height, alt, caption) => {
                    Node::Vector {
                        vector: Vector::rectangles(wide, high, &rectangles, color)?,
                        width,
                        height,
                        alt: alt.into_owned(),
                        caption: caption.map(Cow::into_owned),
                    }
                }
                // spans and the "only inside a Table" rule are checked by the builder, as in process
                WireNode::Cell(child, colspan, rowspan) => {
                    let span = |value: u64| {
                        usize::try_from(value).map_err(|_| ReportError::InvalidDocument {
                            reason: "table cell span out of range in a document frame",
                        })
                    };
                    Node::Cell {
                        child: one(&ids, child)?,
                        colspan: span(colspan)?,
                        rowspan: span(rowspan)?,
                    }
                }
            };
            ids.push(builder.add(node)?);
        }
        if let Some(header) = self.header {
            builder.set_header(one(&ids, header)?)?;
        }
        if let Some(footer) = self.footer {
            builder.set_footer(one(&ids, footer)?)?;
        }
        let wire = self.options;
        let (width, height, margins) = wire.page;
        let (year, month, day) = wire.date;
        // floor and bound of both values are checked again: the worker trusts no frame
        let (dpi, jpeg_quality) = wire.pictures;
        let options = DocumentOptions::new(
            wire.id.into_owned(),
            wire.title.into_owned(),
            PageSetup::with_margins(width, height, margins)?,
        )
        .with_language(&wire.language)?
        .with_fonts(wire.fonts.into_iter().map(Cow::into_owned).collect())?
        .with_date(RenderDate::new(year, month, day)?)
        .with_pictures(PictureQuality::new(dpi, jpeg_quality)?)
        .with_standard(match wire.standard {
            0 => PdfStandard::Pdf17,
            1 => PdfStandard::PdfA3b,
            _ => {
                return Err(ReportError::InvalidDocument {
                    reason: "unknown PDF standard in a document frame",
                });
            }
        });
        builder.finish(one(&ids, self.body)?, options)
    }
}

/// Cap of a request frame under `limits`: twice the document byte limit and one mebibyte. The frame
/// form of a node is smaller than the node, so every admitted document fits.
pub(crate) fn request_frame_bytes(limits: Limits) -> usize {
    limits
        .get(LimitKind::Bytes)
        .saturating_mul(2)
        .saturating_add(1024 * 1024)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod image_failure_tests {
    use super::*;

    #[test]
    fn worker_diagnostics_reject_bad_indices_counts_and_reasons() {
        let limits = Limits::default().lowered(LimitKind::Nodes, 2).unwrap();
        assert!(matches!(
            image_failures_from_wire(vec![(2, "bad".into())], limits),
            Err(ReportError::DocumentLimit {
                kind: LimitKind::Nodes,
                actual: 3,
                max: 2
            })
        ));
        let limits = Limits::default().lowered(LimitKind::Images, 1).unwrap();
        assert!(matches!(
            image_failures_from_wire(vec![(0, "a".into()), (1, "b".into())], limits),
            Err(ReportError::DocumentLimit {
                kind: LimitKind::Images,
                actual: 2,
                max: 1
            })
        ));
        let limits = Limits::default().lowered(LimitKind::Bytes, 2).unwrap();
        assert!(matches!(
            image_failures_from_wire(vec![(0, "bad".into())], limits),
            Err(ReportError::DocumentLimit {
                kind: LimitKind::Bytes,
                actual: 3,
                max: 2
            })
        ));
    }
}
