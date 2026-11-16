// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{LimitKind, Limits, ReportError, Vector, error::invalid, probe_image};
use std::sync::Arc;

/// A handle to a preceding arena node; handles cannot be forged or cross builders.
#[derive(Debug, Clone)]
pub struct NodeId {
    index: usize,
    owner: Arc<()>,
}
impl NodeId {
    pub fn index(&self) -> usize {
        self.index
    }
}
/// Logical page counter, resolved by native pagination.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum PageCounter {
    Current,
    Total,
}
/// Horizontal alignment in the document's text direction.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub enum Alignment {
    #[default]
    Start,
    Center,
    End,
}
/// How the tables inside a block draw their lines: `Lined` (the default) rules between rows under a
/// shaded header, `Bordered` a line around every cell, `Borderless` none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableLook {
    #[default]
    Lined,
    Bordered,
    Borderless,
}
/// Normalized document-owned styling. All dimensions are points.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Style {
    size: f64,
    bold: bool,
    italic: bool,
    underline: bool,
    color: [u8; 3],
    padding: f64,
    alignment: Alignment,
    keep: bool,
    fill: Option<[u8; 3]>,
    above: f64,
    below: f64,
    rule_above: bool,
    rule_below: bool,
    table: TableLook,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            size: 10.0,
            bold: false,
            italic: false,
            underline: false,
            color: [0; 3],
            padding: 0.0,
            alignment: Alignment::Start,
            keep: false,
            fill: None,
            above: 0.0,
            below: 0.0,
            rule_above: false,
            rule_below: false,
            table: TableLook::Lined,
        }
    }
}
impl Style {
    /// Space before and after a block, in points.
    pub fn with_spacing(mut self, above: f64, below: f64) -> Result<Self, ReportError> {
        if ![above, below]
            .iter()
            .all(|v| v.is_finite() && (0.0..=1440.0).contains(v))
        {
            return Err(invalid(
                "block spacing must be finite and in [0, 1440] points",
            ));
        }
        self.above = above;
        self.below = below;
        Ok(self)
    }
    /// Background colour of a block.
    pub fn with_fill(mut self, value: Option<[u8; 3]>) -> Self {
        self.fill = value;
        self
    }
    /// A thin line on the top or bottom edge of a block.
    pub fn with_rules(mut self, above: bool, below: bool) -> Self {
        self.rule_above = above;
        self.rule_below = below;
        self
    }
    /// How the tables inside this block draw their lines.
    pub fn with_table_look(mut self, value: TableLook) -> Self {
        self.table = value;
        self
    }
    /// Background colour, if any.
    pub fn fill(&self) -> Option<[u8; 3]> {
        self.fill
    }
    /// Space before and after the block, in points.
    pub fn spacing(&self) -> (f64, f64) {
        (self.above, self.below)
    }
    /// Whether a thin line is drawn on the top and on the bottom edge.
    pub fn rules(&self) -> (bool, bool) {
        (self.rule_above, self.rule_below)
    }
    /// How the tables inside this block draw their lines.
    pub fn table_look(&self) -> TableLook {
        self.table
    }
    /// Whether the block draws or spaces anything beyond its text: then it cannot be a bare paragraph.
    /// Whether the block draws or spaces anything beyond text: spacing, a fill, an edge rule or a table
    /// look other than the default. A decorated block is never folded into the surrounding text.
    pub fn decorated(&self) -> bool {
        decorates(
            self.above,
            self.below,
            self.fill,
            (self.rule_above, self.rule_below),
            self.table,
        )
    }
    pub fn with_size(mut self, points: f64) -> Result<Self, ReportError> {
        if !points.is_finite() || points <= 0.0 || points > 1440.0 {
            return Err(invalid("font size must be finite and in (0, 1440] points"));
        }
        self.size = points;
        Ok(self)
    }
    pub fn with_padding(mut self, points: f64) -> Result<Self, ReportError> {
        if !points.is_finite() || !(0.0..=1440.0).contains(&points) {
            return Err(invalid("padding must be finite and in [0, 1440] points"));
        }
        self.padding = points;
        Ok(self)
    }
    pub fn with_bold(mut self, value: bool) -> Self {
        self.bold = value;
        self
    }
    pub fn with_italic(mut self, value: bool) -> Self {
        self.italic = value;
        self
    }
    pub fn with_underline(mut self, value: bool) -> Self {
        self.underline = value;
        self
    }
    pub fn with_color(mut self, value: [u8; 3]) -> Self {
        self.color = value;
        self
    }
    pub fn with_alignment(mut self, value: Alignment) -> Self {
        self.alignment = value;
        self
    }
    pub fn with_keep_together(mut self, value: bool) -> Self {
        self.keep = value;
        self
    }
    pub fn size(&self) -> f64 {
        self.size
    }
    pub fn bold(&self) -> bool {
        self.bold
    }
    pub fn italic(&self) -> bool {
        self.italic
    }
    pub fn underline(&self) -> bool {
        self.underline
    }
    pub fn color(&self) -> [u8; 3] {
        self.color
    }
    pub fn padding(&self) -> f64 {
        self.padding
    }
    pub fn alignment(&self) -> Alignment {
        self.alignment
    }
    pub fn keep_together(&self) -> bool {
        self.keep
    }
}
/// Unvalidated layout nodes, admitted through [`DocumentBuilder::add`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Node {
    /// Document-level sequence, permitting page breaks between children.
    Flow {
        children: Vec<NodeId>,
    },
    Text(String),
    Block {
        children: Vec<NodeId>,
        style: Style,
    },
    Inline {
        children: Vec<NodeId>,
        style: Style,
    },
    Grid {
        columns: Vec<f64>,
        gutter: f64,
        cells: Vec<NodeId>,
    },
    Table {
        columns: usize,
        header: Vec<NodeId>,
        cells: Vec<NodeId>,
    },
    List {
        ordered: bool,
        items: Vec<NodeId>,
    },
    /// Raster bytes (PNG, JPEG, GIF, WebP) or user SVG, which the in-process engine prints as a
    /// placeholder box with `alt` and a warning. Sizes are points; `None` keeps the natural size.
    Image {
        data: Arc<[u8]>,
        width: Option<f64>,
        height: Option<f64>,
        alt: String,
    },
    /// Trusted vector graphic generated by ERPForAll (barcodes, QR codes).
    Vector {
        vector: Vector,
        width: Option<f64>,
        height: Option<f64>,
        alt: String,
    },
    /// A table cell spanning `colspan` columns and `rowspan` rows; only a direct child of a `Table`.
    Cell {
        child: NodeId,
        colspan: usize,
        rowspan: usize,
    },
    Rule,
    LineBreak,
    PageBreak,
    PageCounter(PageCounter),
}
impl Node {
    fn children(&self) -> impl Iterator<Item = &NodeId> {
        let (first, second): (&[NodeId], &[NodeId]) = match self {
            Self::Flow { children }
            | Self::Block { children, .. }
            | Self::Inline { children, .. } => (children, &[]),
            Self::Grid { cells, .. } => (cells, &[]),
            Self::Table { header, cells, .. } => (header, cells),
            Self::List { items, .. } => (items, &[]),
            Self::Cell { child, .. } => (std::slice::from_ref(child), &[]),
            Self::Text(_)
            | Self::Image { .. }
            | Self::Vector { .. }
            | Self::Rule
            | Self::LineBreak
            | Self::PageBreak
            | Self::PageCounter(_) => (&[], &[]),
        };
        first.iter().chain(second)
    }
}
fn size(points: Option<f64>) -> Result<(), ReportError> {
    match points {
        Some(v) if !v.is_finite() || v <= 0.0 || v > 14400.0 => Err(invalid(
            "image and vector sizes must be finite and in (0, 14400] points",
        )),
        _ => Ok(()),
    }
}
/// Validated page dimensions and margins, all in points.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PageSetup {
    width: f64,
    height: f64,
    /// top, right, bottom, left
    margins: [f64; 4],
}
impl PageSetup {
    /// A page with the same margin on every side.
    pub fn new(width: f64, height: f64, margin: f64) -> Result<Self, ReportError> {
        Self::with_margins(width, height, [margin; 4])
    }
    /// A page with its own margin per side: `[top, right, bottom, left]`, as a paper format gives them.
    pub fn with_margins(width: f64, height: f64, margins: [f64; 4]) -> Result<Self, ReportError> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(invalid("page dimensions must be finite and positive"));
        }
        if width > 14400.0 || height > 14400.0 {
            return Err(invalid("page dimensions exceed 14400 points"));
        }
        let [top, right, bottom, left] = margins;
        // A body thinner than a point would make a page of every line: unbounded work from valid input.
        if margins.iter().any(|m| !m.is_finite() || *m < 0.0)
            || width - left - right < 1.0
            || height - top - bottom < 1.0
        {
            return Err(invalid(
                "page margins must be finite, nonnegative and leave a body of at least 1 point each way",
            ));
        }
        Ok(Self {
            width,
            height,
            margins,
        })
    }
    pub fn width(&self) -> f64 {
        self.width
    }
    pub fn height(&self) -> f64 {
        self.height
    }
    /// Margins in points: `[top, right, bottom, left]`.
    pub fn margins(&self) -> [f64; 4] {
        self.margins
    }
}
/// Export profile; e-invoice metadata and attachments belong to the post-processing slice.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub enum PdfStandard {
    #[default]
    Pdf17,
    PdfA3b,
}
/// Explicit UTC date, shared by World and PDF metadata.
#[derive(Debug, Clone, Copy)]
pub struct RenderDate {
    year: i32,
    month: u8,
    day: u8,
}
impl RenderDate {
    pub fn new(year: i32, month: u8, day: u8) -> Result<Self, ReportError> {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if leap {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        };
        if !(1..=9999).contains(&year) || day == 0 || day > days {
            return Err(invalid("render date is not a Gregorian date"));
        }
        Ok(Self { year, month, day })
    }
    pub fn parts(self) -> (i32, u8, u8) {
        (self.year, self.month, self.day)
    }
}
/// Document-owned metadata and options; default date is a fixed epoch, never the clock.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DocumentOptions {
    id: String,
    title: String,
    page: PageSetup,
    language: String,
    fonts: Vec<String>,
    date: RenderDate,
    standard: PdfStandard,
}
impl DocumentOptions {
    pub fn new(id: impl Into<String>, title: impl Into<String>, page: PageSetup) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            page,
            language: "en".into(),
            fonts: vec!["Noto Sans".into()],
            date: RenderDate {
                year: 1970,
                month: 1,
                day: 1,
            },
            standard: PdfStandard::Pdf17,
        }
    }
    pub fn with_language(mut self, value: &str) -> Result<Self, ReportError> {
        // The renderer takes an ISO 639 code; a region is not part of this contract yet.
        if !(2..=3).contains(&value.len()) || !value.bytes().all(|c| c.is_ascii_lowercase()) {
            return Err(invalid(
                "language must be two or three lowercase ASCII letters",
            ));
        }
        self.language = value.into();
        Ok(self)
    }
    pub fn with_fonts(mut self, value: Vec<String>) -> Result<Self, ReportError> {
        if value.is_empty()
            || value.len() > 16
            || value.iter().any(|v| v.is_empty() || v.len() > 128)
        {
            return Err(invalid(
                "font families require 1..=16 names of 1..=128 bytes",
            ));
        }
        self.fonts = value;
        Ok(self)
    }
    pub fn with_date(mut self, date: RenderDate) -> Self {
        self.date = date;
        self
    }
    pub fn with_standard(mut self, standard: PdfStandard) -> Self {
        self.standard = standard;
        self
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn page(&self) -> &PageSetup {
        &self.page
    }
    pub fn language(&self) -> &str {
        &self.language
    }
    pub fn fonts(&self) -> &[String] {
        &self.fonts
    }
    pub fn date(&self) -> RenderDate {
        self.date
    }
    pub fn standard(&self) -> PdfStandard {
        self.standard
    }
}
#[derive(Debug, Clone, Copy)]
struct Cost {
    nodes: usize,
    bytes: usize,
    depth: usize,
    /// Expanded page break occurrences: each one starts a page whatever the content is.
    breaks: usize,
}
/// Flat arena: construction, rejection and destruction do not recurse through user trees.
#[derive(Debug)]
#[non_exhaustive]
pub struct DocumentBuilder {
    owner: Arc<()>,
    nodes: Vec<Node>,
    costs: Vec<Cost>,
    limits: Limits,
    stored_bytes: usize,
    header: Option<NodeId>,
    footer: Option<NodeId>,
}
impl DocumentBuilder {
    pub fn new(limits: Limits) -> Self {
        Self {
            owner: Arc::new(()),
            nodes: Vec::new(),
            costs: Vec::new(),
            limits,
            stored_bytes: 0,
            header: None,
            footer: None,
        }
    }
    fn node(&self, id: &NodeId) -> Result<&Node, ReportError> {
        self.cost(id)?;
        self.nodes
            .get(id.index)
            .ok_or_else(|| invalid("node must precede its parent"))
    }
    /// Places one table section row by row, as the renderer's automatic placement does, and refuses
    /// what it would place differently: a span past the last column, a cell that would overlap a row
    /// span or skip a free slot, an incomplete last row and a row span past the section's last row.
    /// Memory is one counter per column, whatever the row count.
    fn place(&self, columns: usize, cells: &[NodeId]) -> Result<(), ReportError> {
        // Rows still covered by a row span from above, per column, counting the current row.
        let mut busy = vec![0usize; columns];
        let mut col = 0;
        let next_row = |busy: &mut [usize]| {
            busy.iter_mut().for_each(|b| *b = b.saturating_sub(1));
        };
        for id in cells {
            let (colspan, rowspan) = match self.node(id)? {
                Node::Cell {
                    colspan, rowspan, ..
                } => (*colspan, *rowspan),
                _ => (1, 1),
            };
            loop {
                if col == columns {
                    next_row(&mut busy);
                    col = 0;
                    // A row fully covered by spans from above holds no new cell: skip such rows at once.
                    let covered = busy.iter().copied().min().unwrap_or(0);
                    if covered > 0 {
                        busy.iter_mut().for_each(|b| *b -= covered);
                    }
                }
                if busy[col] == 0 {
                    break;
                }
                col += 1;
            }
            let end = col
                .checked_add(colspan)
                .filter(|end| *end <= columns)
                .ok_or_else(|| invalid("table cell spans past the last column"))?;
            if busy[col..end].iter().any(|b| *b > 0) {
                return Err(invalid(
                    "table cell overlaps a row span or leaves a gap in its row",
                ));
            }
            busy[col..end].iter_mut().for_each(|b| *b = rowspan);
            col = end;
        }
        if !cells.is_empty() && busy[col..].contains(&0) {
            return Err(invalid(
                "table rows must fill a positive column count no larger than its cells",
            ));
        }
        next_row(&mut busy);
        if busy.iter().any(|b| *b > 0) {
            return Err(invalid("table row span reaches past the last row"));
        }
        Ok(())
    }
    fn cost(&self, id: &NodeId) -> Result<Cost, ReportError> {
        if !Arc::ptr_eq(&self.owner, &id.owner) {
            return Err(invalid("node belongs to another document builder"));
        }
        self.costs
            .get(id.index)
            .copied()
            .ok_or_else(|| invalid("node must precede its parent"))
    }
    pub fn add(&mut self, node: Node) -> Result<NodeId, ReportError> {
        self.limits.check(LimitKind::Nodes, self.nodes.len() + 1)?;
        let child_count = node.children().count();
        self.limits.check(LimitKind::Nodes, child_count)?;
        if let Node::Inline { style, .. } = &node
            && (style.padding != 0.0 || style.keep || !matches!(style.alignment, Alignment::Start))
        {
            return Err(ReportError::Unsupported {
                feature: "inline container styling",
                task: "a later release",
            });
        }
        match &node {
            Node::Grid {
                columns,
                gutter,
                cells,
            } if columns.is_empty()
                || columns.len() > cells.len().max(1)
                || columns
                    .iter()
                    .any(|v| !v.is_finite() || *v <= 0.0 || *v > 1_000_000.0)
                || !gutter.is_finite()
                || !(0.0..=1440.0).contains(gutter) =>
            {
                return Err(invalid(
                    "grid requires finite column weights in (0, 1000000] and a gutter in [0, 1440] points",
                ));
            }
            Node::Table {
                columns,
                header,
                cells,
            } => {
                if *columns == 0 || *columns > header.len().max(cells.len()).max(1) {
                    return Err(invalid(
                        "table rows must fill a positive column count no larger than its cells",
                    ));
                }
                self.place(*columns, header)?;
                self.place(*columns, cells)?;
            }
            Node::Cell {
                colspan, rowspan, ..
            } if *colspan == 0 || *rowspan == 0 => {
                return Err(invalid("table cell spans must be at least 1"));
            }
            Node::Image {
                data,
                width,
                height,
                ..
            } => {
                size(*width)?;
                size(*height)?;
                probe_image(data, &self.limits)?;
            }
            Node::Vector { width, height, .. } => {
                size(*width)?;
                size(*height)?;
            }
            _ => {} // every other node kind, and a grid or cell of a valid shape
        }
        if !matches!(node, Node::Table { .. }) {
            for child in node.children() {
                if matches!(self.node(child)?, Node::Cell { .. }) {
                    return Err(invalid(
                        "a Cell node is only allowed directly inside a Table",
                    ));
                }
            }
        }
        // Payload includes retained capacities; allocator bookkeeping/arena spare capacity is not RSS admission.
        let (payload, references) = match &node {
            Node::Text(s) => (s.capacity(), 0),
            Node::Image { data, alt, .. } => (
                data.len()
                    .checked_add(alt.capacity())
                    .ok_or_else(|| invalid("document size arithmetic overflow"))?,
                0,
            ),
            Node::Vector { vector, alt, .. } => (
                vector
                    .svg()
                    .len()
                    .checked_add(alt.capacity())
                    .ok_or_else(|| invalid("document size arithmetic overflow"))?,
                0,
            ),
            Node::Cell { .. } => (0, 1),
            Node::Grid { columns, cells, .. } => (
                columns
                    .capacity()
                    .checked_mul(std::mem::size_of::<f64>())
                    .ok_or_else(|| invalid("document size arithmetic overflow"))?,
                cells.capacity(),
            ),
            Node::Table { header, cells, .. } => (
                0,
                header
                    .capacity()
                    .checked_add(cells.capacity())
                    .ok_or_else(|| invalid("document size arithmetic overflow"))?,
            ),
            Node::Flow { children }
            | Node::Block { children, .. }
            | Node::Inline { children, .. } => (0, children.capacity()),
            Node::List { items, .. } => (0, items.capacity()),
            Node::Rule | Node::LineBreak | Node::PageBreak | Node::PageCounter(_) => (0, 0),
        };
        let own = std::mem::size_of::<Node>()
            .checked_add(payload)
            .and_then(|n| {
                references
                    .checked_mul(std::mem::size_of::<NodeId>())
                    .and_then(|r| n.checked_add(r))
            })
            .ok_or_else(|| invalid("document size arithmetic overflow"))?;
        let mut cost = Cost {
            nodes: 1,
            bytes: own,
            depth: 1,
            breaks: usize::from(matches!(node, Node::PageBreak)),
        };
        for child in node.children() {
            let c = self.cost(child)?;
            cost.nodes = self.limits.add(LimitKind::Nodes, cost.nodes, c.nodes)?;
            cost.bytes = self.limits.add(LimitKind::Bytes, cost.bytes, c.bytes)?;
            // A table spends twice the renderer's nesting budget of any other container.
            let step = if matches!(node, Node::Table { .. }) {
                2
            } else {
                1
            };
            cost.depth = cost.depth.max(c.depth + step);
            cost.breaks = cost
                .breaks
                .checked_add(c.breaks)
                .ok_or_else(|| invalid("document size arithmetic overflow"))?;
        }
        self.limits.check(LimitKind::Depth, cost.depth)?;
        self.limits
            .check(LimitKind::Pages, cost.breaks.saturating_add(1))?;
        self.limits.check(LimitKind::Bytes, cost.bytes)?;
        let stored = self.limits.add(LimitKind::Bytes, self.stored_bytes, own)?;
        let id = NodeId {
            index: self.nodes.len(),
            owner: self.owner.clone(),
        };
        self.nodes.push(node);
        self.costs.push(cost);
        self.stored_bytes = stored;
        Ok(id)
    }
    fn root(&self, id: &NodeId) -> Result<(), ReportError> {
        if matches!(self.node(id)?, Node::Cell { .. }) {
            return Err(invalid(
                "a Cell node is only allowed directly inside a Table",
            ));
        }
        Ok(())
    }
    pub fn set_header(&mut self, id: NodeId) -> Result<(), ReportError> {
        self.root(&id)?;
        self.header = Some(id);
        Ok(())
    }
    pub fn set_footer(&mut self, id: NodeId) -> Result<(), ReportError> {
        self.root(&id)?;
        self.footer = Some(id);
        Ok(())
    }
    pub fn finish(self, body: NodeId, options: DocumentOptions) -> Result<DocTree, ReportError> {
        if options.id.is_empty()
            || options.id.len() > 256
            || !options
                .id
                .bytes()
                .all(|c| c == b' ' || c.is_ascii_graphic())
        {
            return Err(invalid(
                "document id must be 1..=256 printable ASCII characters",
            ));
        }
        self.root(&body)?;
        let mut cost = self.cost(&body)?;
        for part in [&self.header, &self.footer].into_iter().flatten() {
            let c = self.cost(part)?;
            cost.nodes = self.limits.add(LimitKind::Nodes, cost.nodes, c.nodes)?;
            cost.bytes = self.limits.add(LimitKind::Bytes, cost.bytes, c.bytes)?;
            // A running region is laid out one level deeper than the body.
            self.limits.check(LimitKind::Depth, c.depth + 1)?;
            cost.depth = cost.depth.max(c.depth + 1);
            cost.breaks = cost
                .breaks
                .checked_add(c.breaks)
                .ok_or_else(|| invalid("document size arithmetic overflow"))?;
        }
        self.limits
            .check(LimitKind::Pages, cost.breaks.saturating_add(1))?;
        let mut metadata = self.limits.add(
            LimitKind::Bytes,
            options.id.capacity(),
            options.title.capacity(),
        )?;
        metadata = self
            .limits
            .add(LimitKind::Bytes, metadata, options.language.capacity())?;
        for font in &options.fonts {
            metadata = self
                .limits
                .add(LimitKind::Bytes, metadata, font.capacity())?;
        }
        metadata = self.limits.add(
            LimitKind::Bytes,
            metadata,
            options
                .fonts
                .capacity()
                .checked_mul(std::mem::size_of::<String>())
                .ok_or_else(|| invalid("document size arithmetic overflow"))?,
        )?;
        // The encoder hands every arena node to the renderer, reachable or not: admission takes the
        // largest expanded cost any retained node carries.
        for retained in &self.costs {
            cost.depth = cost.depth.max(retained.depth);
            cost.nodes = cost.nodes.max(retained.nodes);
            cost.bytes = cost.bytes.max(retained.bytes);
        }
        cost.bytes = self.limits.add(LimitKind::Bytes, cost.bytes, metadata)?;
        cost.bytes = cost.bytes.max(self.limits.add(
            LimitKind::Bytes,
            self.stored_bytes,
            metadata,
        )?);
        cost.nodes = cost.nodes.max(self.nodes.len());
        Ok(DocTree {
            nodes: self.nodes,
            body,
            header: self.header,
            footer: self.footer,
            options,
            cost,
        })
    }
}
/// Validated immutable layout input. No record handles, executable markup or file paths.
#[derive(Debug)]
#[non_exhaustive]
pub struct DocTree {
    nodes: Vec<Node>,
    body: NodeId,
    header: Option<NodeId>,
    footer: Option<NodeId>,
    options: DocumentOptions,
    cost: Cost,
}
impl DocTree {
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
    pub fn body(&self) -> &NodeId {
        &self.body
    }
    pub fn header(&self) -> Option<&NodeId> {
        self.header.as_ref()
    }
    pub fn footer(&self) -> Option<&NodeId> {
        self.footer.as_ref()
    }
    pub fn options(&self) -> &DocumentOptions {
        &self.options
    }
    pub fn check_limits(&self, limits: Limits) -> Result<(), ReportError> {
        limits.check(LimitKind::Nodes, self.cost.nodes)?;
        limits.check(LimitKind::Bytes, self.cost.bytes)?;
        limits.check(LimitKind::Depth, self.cost.depth)?;
        limits.check(LimitKind::Pages, self.cost.breaks.saturating_add(1))
    }
}

/// The one decoration rule shared by the evaluator and [`Style::decorated`].
pub(crate) fn decorates(
    above: f64,
    below: f64,
    fill: Option<[u8; 3]>,
    rules: (bool, bool),
    table: TableLook,
) -> bool {
    above > 0.0
        || below > 0.0
        || fill.is_some()
        || rules != (false, false)
        || table != TableLook::Lined
}
