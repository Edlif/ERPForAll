// SPDX-License-Identifier: MIT OR Apache-2.0
//! Evaluation (design §4.2): a compiled program, a context and the loaded records become one
//! `DocTree`. Control flow runs here; expressions run in `erp-expr` under its limits; the result
//! holds no expression and no record handle.
//!
//! Layout-neutral wrappers are not copied into the document: a `<t>`, an inline element that
//! changes no text style and a block element that changes nothing and holds only blocks emit no
//! node, their content stands in their place (design §4.3). Content is never dropped by this.
use crate::{
    Alignment, Diagnostic, DocTree, DocumentBuilder, DocumentOptions, Limits, Node, NodeId,
    PageCounter, ReportError, ReportProgram, Style,
    asset::embedded_bytes,
    format::LangFormat,
    limits::millis,
    look::{self, Look, Role, Size, Tint},
    probe_image,
    program::{Argument, At, Attr, Code, Content, Element, Op, Part},
    records::{IMAGE, RecordStore},
    validate::{embedded_image, validate_attribute},
    widget::{self, Piece},
};
use erp_expr::{EvalContext, ExprError, ExprObject, ExprResult, ExprValue, Map, Seq, eval_ast};
use std::{rc::Rc, sync::Arc, time::Instant};

const FRAGMENT: &str = "report.fragment";
const VECTORS_TASK: &str = "a later release";
/// The helper a template gives an image field to for an `<img>` source, as in the reference.
const IMAGE_HELPER: &str = "image_data_uri";
/// Points of one CSS pixel: the natural size of an image is its pixel size at 96 per inch.
const POINTS_PER_PIXEL: f64 = 0.75;
/// The largest side of an image, in points (the document's own bound).
const IMAGE_POINTS_MAX: f64 = 14400.0;

/// The user's offset from UTC at a timestamp (seconds since the epoch).
#[derive(Clone)]
pub(crate) struct UtcOffsets(Arc<dyn Fn(i64) -> Option<i32> + Send + Sync>);
impl UtcOffsets {
    /// The offset at `unix`, within 18 hours either way.
    pub(crate) fn at(&self, unix: i64) -> Option<i32> {
        (self.0)(unix).filter(|seconds| (-64_800..=64_800).contains(seconds))
    }
}
impl std::fmt::Debug for UtcOffsets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UtcOffsets(..)")
    }
}

/// What an evaluation needs besides the program, the context and the records.
#[derive(Debug, Clone)]
pub struct EvalOptions {
    formats: LangFormat,
    utc_offset: UtcOffsets,
    primary: [u8; 3],
    secondary: [u8; 3],
    limits: Limits,
}
impl Default for EvalOptions {
    /// `en_US` formats, UTC, the default theme colours and the default document limits.
    fn default() -> Self {
        Self {
            formats: LangFormat::default(),
            utc_offset: UtcOffsets(Arc::new(|_: i64| Some(0))),
            primary: [13, 110, 253],
            secondary: [108, 117, 125],
            limits: Limits::default(),
        }
    }
}
impl EvalOptions {
    /// Number, date and time formats of the document's language.
    pub fn with_formats(mut self, formats: LangFormat) -> Self {
        self.formats = formats;
        self
    }
    /// The user's offset from UTC for the timestamps of this document, in seconds; at most 18
    /// hours either way. The caller resolves a timezone name to the offset.
    pub fn with_utc_offset_seconds(mut self, seconds: i32) -> Result<Self, ReportError> {
        if !(-64_800..=64_800).contains(&seconds) {
            return Err(ReportError::InvalidDocument {
                reason: "UTC offset must be within 18 hours",
            });
        }
        self.utc_offset = UtcOffsets(Arc::new(move |_: i64| Some(seconds)));
        Ok(self)
    }
    /// The user's timezone: `offset` gives its offset from UTC, in seconds, at a timestamp (in
    /// seconds since 1970-01-01 UTC), so a timestamp in summer and one in winter each print in
    /// their own local time. `None`, or an offset of more than 18 hours, stops the document.
    pub fn with_utc_offsets(
        mut self,
        offset: impl Fn(i64) -> Option<i32> + Send + Sync + 'static,
    ) -> Self {
        self.utc_offset = UtcOffsets(Arc::new(offset));
        self
    }
    /// The colours `text-primary` and `text-secondary`/`text-muted` print in.
    pub fn with_theme_colors(mut self, primary: [u8; 3], secondary: [u8; 3]) -> Self {
        self.primary = primary;
        self.secondary = secondary;
        self
    }
    /// Lowered document limits (nodes, bytes, depth, pages).
    pub fn with_document_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// The text style a node's content inherits.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Text {
    size: f64,
    bold: bool,
    italic: bool,
    underline: bool,
    color: [u8; 3],
}
#[derive(Debug, Clone, Copy)]
struct Inherit {
    text: Text,
    /// 0 start, 1 centre, 2 end.
    align: u8,
    /// Whether a page break can be laid out here: no container between here and the page.
    flow: bool,
    /// Inside a `thead`.
    head: bool,
    /// A row or table section changed the style and emitted no node for it yet.
    pending: bool,
}
/// What an element adds to what it inherited.
struct Own {
    inherit: Inherit,
    padding: f64,
    keep: bool,
    above: f64,
    below: f64,
    fill: Option<[u8; 3]>,
    rules: (bool, bool),
    table: crate::TableLook,
    text_changed: bool,
    box_changed: bool,
}
impl Own {
    fn styled(&self) -> bool {
        self.text_changed || self.box_changed
    }
}
enum Item {
    Text(String),
    Node {
        id: NodeId,
        block: bool,
        column: Option<u8>,
    },
    /// A table cell and the columns it spans.
    Cell(NodeId, usize),
    Row {
        cells: Vec<(NodeId, usize)>,
        head: bool,
    },
    Entry(NodeId),
}
impl Item {
    fn boundary(&self) -> bool {
        !matches!(self, Self::Text(_) | Self::Node { block: false, .. })
    }
}
/// The attributes of one element after evaluation.
#[derive(Default)]
struct Attrs {
    text: Vec<(String, String)>,
    /// The image value given as the `src` of an `<img>`.
    image: Option<Arc<[u8]>>,
}
impl Attrs {
    fn put(&mut self, name: &str, value: Option<String>) {
        self.text.retain(|(known, _)| known != name);
        if name == "src" {
            self.image = None;
        }
        if let Some(value) = value {
            self.text.push((name.into(), value));
        }
    }
    fn get(&self, name: &str) -> Option<&str> {
        let found = self.text.iter().find(|(known, _)| known == name);
        found.map(|(_, value)| value.as_str())
    }
    /// The value of one property of the `style` attribute, the last one written.
    fn style(&self, property: &str) -> Option<&str> {
        self.get("style")?
            .split(';')
            .filter_map(|item| item.split_once(':'))
            .filter(|(name, _)| name.trim() == property)
            .map(|(_, value)| value.trim())
            .next_back()
    }
    fn look(&self, tag: &str) -> Look {
        Look::of(tag, self.text.iter().map(|(n, v)| (n.as_str(), v.as_str())))
    }
}
/// Template operations with the names they were written under.
struct Closure<'p> {
    ops: &'p [Op],
    context: EvalContext,
    slot: Option<Rc<Closure<'p>>>,
}
struct Scope<'p> {
    context: EvalContext,
    /// The body of the `t-call` that entered this template (`t-out="0"`).
    slot: Option<Rc<Closure<'p>>>,
}
/// A `t-set` body as an expression value: printed by `t-out`, true when it has content.
#[derive(Debug)]
struct Fragment {
    index: usize,
}
impl ExprObject for Fragment {
    fn type_name(&self) -> &str {
        FRAGMENT
    }
    fn as_int(&self) -> ExprResult<i64> {
        Ok(i64::try_from(self.index).unwrap_or(-1))
    }
}
enum Source {
    Values(Seq),
    Texts(Vec<String>),
    Pairs(Vec<(ExprValue, ExprValue)>),
    Count,
    Records(Arc<dyn ExprObject>),
}
struct Run<'p> {
    program: &'p ReportProgram,
    records: &'p RecordStore,
    options: &'p EvalOptions,
    builder: DocumentBuilder,
    steps: usize,
    bytes: usize,
    started: Instant,
    nesting: usize,
    closures: Vec<Rc<Closure<'p>>>,
    header: Option<NodeId>,
    footer: Option<NodeId>,
    base: f64,
}

impl ReportProgram {
    /// Evaluate the program for one document.
    ///
    /// `context` binds the names the template reads (`docs`, `doc`, `company`, plain values);
    /// record values come from `records` (see [`RecordStore::recordset`]). The document's language
    /// is the program's. Evaluation stops with [`ReportError::Limit`] at the step limit of the
    /// program's [`crate::ReportLimits`], and with [`ReportError::DocumentLimit`] at a document
    /// limit.
    pub fn evaluate(
        &self,
        context: &EvalContext,
        records: &RecordStore,
        options: &EvalOptions,
        document: DocumentOptions,
    ) -> Result<DocTree, ReportError> {
        let document = document.with_language(self.language.iso())?;
        let base = Style::default().size();
        let mut run = Run {
            program: self,
            records,
            options,
            builder: DocumentBuilder::new(options.limits),
            steps: 0,
            bytes: 0,
            started: Instant::now(),
            nesting: 0,
            closures: Vec::new(),
            header: None,
            footer: None,
            base,
        };
        let inherit = Inherit {
            text: Text {
                size: base,
                bold: false,
                italic: false,
                underline: false,
                color: [0; 3],
            },
            align: 0,
            flow: true,
            head: false,
            pending: false,
        };
        // `image_data_uri(record.image_field)` is part of the profile, whatever the caller bound.
        let mut context = context.clone();
        context.set_function(IMAGE_HELPER, |args, kwargs, _| match args {
            [image @ ExprValue::Object(object)]
                if kwargs.is_empty() && object.type_name() == IMAGE =>
            {
                Ok(image.clone())
            }
            [other] if kwargs.is_empty() => Err(ExprError::type_error(format!(
                "{IMAGE_HELPER}() takes the value of an image field that is set, got '{}'",
                other.type_name()
            ))),
            _ => Err(ExprError::type_error(format!(
                "{IMAGE_HELPER}() takes one argument, the value of an image field"
            ))),
        });
        let mut scope = Scope {
            context,
            slot: None,
        };
        let mut items = Vec::new();
        run.ops(&self.templates[self.root], &mut scope, &inherit, &mut items)?;
        let at = At {
            template: self.name.clone(),
            line: 1,
        };
        let (children, _) = run.nodes(items, true, &at)?;
        let body = run.builder.add(Node::Flow { children })?;
        if let Some(header) = run.header.take() {
            run.builder.set_header(header)?;
        }
        if let Some(footer) = run.footer.take() {
            run.builder.set_footer(footer)?;
        }
        run.builder.finish(body, document)
    }
}

/// The bytes a copy of `context` holds of its own: values in an `Arc` are shared, not copied.
fn copy_size(context: &EvalContext) -> usize {
    context
        .iter()
        .map(|(name, value)| {
            let own = match value {
                ExprValue::Str(text) => text.len(),
                ExprValue::Bytes(bytes) => bytes.len(),
                _ => 0,
            };
            std::mem::size_of::<(String, ExprValue)>() + name.len() + own
        })
        .sum()
}
fn push_text(out: &mut Vec<Item>, text: &str) {
    match out.last_mut() {
        Some(Item::Text(last)) => last.push_str(text),
        _ => out.push(Item::Text(text.into())),
    }
}
fn extend(out: &mut Vec<Item>, items: Vec<Item>) {
    for item in items {
        match item {
            Item::Text(text) => push_text(out, &text),
            other => out.push(other),
        }
    }
}
/// HTML white space: runs of ASCII space, tab and line ends are one space. A no-break space is
/// content.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
            space = true;
        } else {
            if space {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}
fn blank(text: &str) -> bool {
    text.chars()
        .all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}'))
}
/// Collapse white space and drop it where a block starts or ends, as an HTML engine lays it out.
fn tidy(items: Vec<Item>, block: bool) -> Vec<Item> {
    fn trim_end(out: &mut Vec<Item>) {
        if let Some(Item::Text(last)) = out.last_mut() {
            let kept = last.trim_end_matches(' ').len();
            last.truncate(kept);
            if last.is_empty() {
                out.pop();
            }
        }
    }
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Item::Text(text) => {
                let text = collapse(&text);
                let at_boundary = out.last().map_or(block, Item::boundary);
                let text = if at_boundary {
                    text.trim_start_matches(' ')
                } else {
                    text.as_str()
                };
                if !text.is_empty() {
                    push_text(&mut out, text);
                }
            }
            other => {
                if other.boundary() {
                    trim_end(&mut out);
                }
                out.push(other);
            }
        }
    }
    if block {
        trim_end(&mut out);
    }
    out
}
fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "div"
            | "p"
            | "address"
            | "section"
            | "main"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "ul"
            | "ol"
            | "li"
            | "hr"
    ) || matches!(tag.as_bytes(), [b'h', b'1'..=b'6'])
}
/// The lines of a 12-column row, as index ranges of its cells. A row whose cells all have a width
/// (`col-N`) and add up to more than twelve wraps, as a Bootstrap row does; a row with a sharing
/// column (`col`, `col-auto`) stays one line.
fn row_lines(widths: &[Option<u8>]) -> Vec<std::ops::Range<usize>> {
    let fixed: Option<Vec<u32>> = widths
        .iter()
        .map(|w| w.filter(|n| *n > 0).map(u32::from))
        .collect();
    let Some(fixed) = fixed.filter(|fixed| fixed.iter().sum::<u32>() > 12) else {
        return std::iter::once(0..widths.len()).collect();
    };
    let mut lines = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (index, width) in fixed.iter().enumerate() {
        if used > 0 && used + width > 12 {
            lines.push(start..index);
            (start, used) = (index, 0);
        }
        used += width;
    }
    lines.push(start..fixed.len());
    lines
}
/// The widest `colspan` a cell may take, as in HTML.
const MAX_COLSPAN: usize = 1000;
/// Elements that carry style to their cells and emit no node themselves.
fn is_table_part(tag: &str) -> bool {
    matches!(tag, "thead" | "tbody" | "tfoot" | "tr")
}
/// Grid column weights: a `col-N` keeps N twelfths, the others share what is left.
fn weights(widths: &[Option<u8>]) -> Vec<f64> {
    let fixed: u32 = widths.iter().flatten().map(|n| u32::from(*n)).sum();
    let free = widths.iter().filter(|w| w.is_none_or(|n| n == 0)).count();
    let left = f64::from(12u32.saturating_sub(fixed));
    let share = match u32::try_from(free) {
        Ok(free) if free > 0 && left > 0.0 => left / f64::from(free),
        _ => 1.0,
    };
    widths
        .iter()
        .map(|w| match w {
            Some(n) if *n > 0 => f64::from(*n),
            _ => share,
        })
        .collect()
}
fn int(value: usize) -> ExprValue {
    ExprValue::Int(i64::try_from(value).unwrap_or(i64::MAX))
}

impl<'p> Run<'p> {
    fn step(&mut self) -> Result<(), ReportError> {
        self.steps += 1;
        if self.steps > self.program.limits.steps {
            return Err(ReportError::Limit {
                name: "evaluation_steps",
                maximum: self.program.limits.steps,
            });
        }
        // Reading the clock every step would cost more than the step itself.
        let deadline = self.program.limits.deadline;
        if self.steps.is_multiple_of(1024) && self.started.elapsed() > deadline {
            return Err(ReportError::Limit {
                name: "evaluation_deadline",
                maximum: millis(deadline),
            });
        }
        Ok(())
    }
    /// Charge bytes the evaluation buffers (printed text, copied names) when it takes them.
    fn charge(&mut self, bytes: usize) -> Result<(), ReportError> {
        let maximum = self.program.limits.bytes;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > maximum {
            return Err(ReportError::Limit {
                name: "evaluation_bytes",
                maximum,
            });
        }
        Ok(())
    }
    fn copy(&mut self, context: &EvalContext) -> Result<EvalContext, ReportError> {
        self.charge(copy_size(context))?;
        Ok(context.clone())
    }
    fn text(&mut self, out: &mut Vec<Item>, text: &str) -> Result<(), ReportError> {
        self.charge(text.len())?;
        push_text(out, text);
        Ok(())
    }
    fn eval(&mut self, code: &Code, scope: &Scope<'p>, at: &At) -> Result<ExprValue, ReportError> {
        self.step()?;
        eval_ast(&code.expr, &scope.context).map_err(|source| ReportError::Expression {
            template: at.template.as_ref().into(),
            line: at.line,
            expression: code.source.clone(),
            source,
        })
    }
    /// A format string: absent values print nothing.
    fn format(
        &mut self,
        parts: &[Part],
        scope: &Scope<'p>,
        at: &At,
    ) -> Result<String, ReportError> {
        let mut out = String::new();
        for part in parts {
            match part {
                Part::Literal(text) => out.push_str(text),
                Part::Value(code) => match self.eval(code, scope, at)? {
                    ExprValue::None | ExprValue::Bool(false) => {}
                    value => out.push_str(&value.to_py_str()),
                },
            }
        }
        Ok(out)
    }
    fn env(&self) -> widget::Env<'p> {
        let options: &'p EvalOptions = self.options;
        widget::Env {
            formats: &options.formats,
            utc_offset: &options.utc_offset,
            records: self.records,
        }
    }
    fn ops(
        &mut self,
        ops: &'p [Op],
        scope: &mut Scope<'p>,
        inherit: &Inherit,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        // Every recursion of the evaluator passes here; the bound keeps its stack finite whatever
        // fragments and calls a template chains at run time.
        let maximum = self.program.limits.program_depth.saturating_mul(4);
        self.nesting += 1;
        if self.nesting > maximum {
            return Err(ReportError::Limit {
                name: "evaluation_depth",
                maximum,
            });
        }
        for op in ops {
            self.op(op, scope, inherit, out)?;
        }
        self.nesting -= 1;
        Ok(())
    }
    fn op(
        &mut self,
        op: &'p Op,
        scope: &mut Scope<'p>,
        inherit: &Inherit,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        self.step()?;
        match op {
            Op::Text(text) => self.text(out, text)?,
            Op::Element(element) => self.element(element, scope, inherit, out)?,
            Op::If { branches, at } => {
                for branch in branches {
                    let taken = match &branch.test {
                        Some(test) => self.eval(test, scope, at)?.is_truthy(),
                        None => true,
                    };
                    if taken {
                        return self.ops(&branch.body, scope, inherit, out);
                    }
                }
            }
            Op::Foreach {
                items,
                name,
                body,
                at,
            } => self.foreach(items, name, body, at, scope, inherit, out)?,
            Op::Set {
                name,
                value,
                body,
                at,
            } => {
                let value = match value {
                    Some(code) => self.eval(code, scope, at)?,
                    None if body.is_empty() => ExprValue::str(""),
                    None => self.fragment(body, scope)?,
                };
                scope.context.set(name.clone(), value);
            }
            Op::Call {
                template,
                arguments,
                body,
                lang,
                at,
            } => {
                if let Some(lang) = lang {
                    self.language(lang, scope, at)?;
                }
                // The called template works on a copy of the caller's names; the call's body
                // keeps the caller's names as they are now, and prints where `0` is put out.
                let size = copy_size(&scope.context);
                let slot = Rc::new(Closure {
                    ops: body,
                    context: self.copy(&scope.context)?,
                    slot: scope.slot.clone(),
                });
                let mut callee = Scope {
                    context: self.copy(&scope.context)?,
                    slot: Some(Rc::clone(&slot)),
                };
                for (name, argument) in arguments {
                    let value = match argument {
                        Argument::Value(code) => self.eval(code, scope, at)?,
                        Argument::Format(parts) => ExprValue::str(self.format(parts, scope, at)?),
                    };
                    callee.context.set(name.clone(), value);
                }
                let program: &'p ReportProgram = self.program;
                self.ops(&program.templates[*template], &mut callee, inherit, out)?;
                // The callee's copy ends here; the body's copy too unless a fragment kept it.
                drop(callee);
                let kept = if Rc::strong_count(&slot) > 1 { size } else { 0 };
                self.bytes -= 2 * size - kept;
            }
        }
        Ok(())
    }
    /// `t-lang` names the language of the called template. A program is compiled per language, so
    /// the caller picks the program of each record's language; another language here is refused.
    fn language(&mut self, lang: &Code, scope: &Scope<'p>, at: &At) -> Result<(), ReportError> {
        let compiled = self.program.language.code();
        match self.eval(lang, scope, at)? {
            ExprValue::None | ExprValue::Bool(false) => Ok(()),
            ExprValue::Str(code) if code == compiled => Ok(()),
            ExprValue::Str(code) => Err(at.evaluate(format!(
                "t-lang is `{code}` but this program was compiled for `{compiled}`; evaluate a record with the program of its language"
            ))),
            other => Err(at.evaluate(format!(
                "t-lang must be a language code, got a value of type {}",
                other.type_name()
            ))),
        }
    }
    fn fragment(&mut self, body: &'p [Op], scope: &Scope<'p>) -> Result<ExprValue, ReportError> {
        let maximum = self.program.limits.fragments;
        if self.closures.len() >= maximum {
            return Err(ReportError::Limit {
                name: "template_fragments",
                maximum,
            });
        }
        let context = self.copy(&scope.context)?;
        self.closures.push(Rc::new(Closure {
            ops: body,
            context,
            slot: scope.slot.clone(),
        }));
        Ok(ExprValue::Object(Arc::new(Fragment {
            index: self.closures.len() - 1,
        })))
    }
    fn closure(
        &mut self,
        closure: &Rc<Closure<'p>>,
        inherit: &Inherit,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        let size = copy_size(&closure.context);
        let mut scope = Scope {
            context: self.copy(&closure.context)?,
            slot: closure.slot.clone(),
        };
        self.ops(closure.ops, &mut scope, inherit, out)?;
        self.bytes -= size;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn foreach(
        &mut self,
        items: &Code,
        name: &str,
        body: &'p [Op],
        at: &At,
        scope: &mut Scope<'p>,
        inherit: &Inherit,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        let value = self.eval(items, scope, at)?;
        let (source, size) = match value {
            ExprValue::None | ExprValue::Bool(false) => (Source::Count, 0),
            ExprValue::List(seq) | ExprValue::Tuple(seq) => {
                let size = seq.len();
                (Source::Values(seq), size)
            }
            ExprValue::Dict(map) => {
                let pairs: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let size = pairs.len();
                (Source::Pairs(pairs), size)
            }
            ExprValue::Int(count) => (Source::Count, usize::try_from(count).unwrap_or(0)),
            ExprValue::Str(text) => {
                let texts: Vec<String> = text.chars().map(String::from).collect();
                let size = texts.len();
                (Source::Texts(texts), size)
            }
            ExprValue::Object(object) if object.record_ids().is_some() => {
                let size = object.record_ids().map_or(0, |ids| ids.len());
                (Source::Records(object), size)
            }
            other => {
                return Err(at.evaluate(format!(
                    "t-foreach cannot loop over a value of type {}",
                    other.type_name()
                )));
            }
        };
        // The loop's names stay in the template's scope after it, as in the reference.
        scope.context.set(format!("{name}_size"), int(size));
        for index in 0..size {
            self.step()?;
            let (item, inner) = match &source {
                Source::Values(seq) => {
                    let item = seq.items()[index].clone();
                    (item.clone(), item)
                }
                Source::Texts(texts) => {
                    let item = ExprValue::str(texts[index].as_str());
                    (item.clone(), item)
                }
                Source::Pairs(pairs) => pairs[index].clone(),
                Source::Count => (int(index), int(index)),
                Source::Records(object) => {
                    let mut budget = erp_expr::Budget::new(scope.context.limits());
                    let item = object
                        .get_item(&int(index), &mut budget)
                        .map_err(|source| ReportError::Expression {
                            template: at.template.as_ref().into(),
                            line: at.line,
                            expression: items.source.clone(),
                            source,
                        })?;
                    (item.clone(), item)
                }
            };
            let odd = index % 2 == 1;
            let context = &mut scope.context;
            context.set(name, item);
            context.set(format!("{name}_value"), inner);
            context.set(format!("{name}_index"), int(index));
            context.set(format!("{name}_first"), ExprValue::Bool(index == 0));
            context.set(format!("{name}_last"), ExprValue::Bool(index + 1 == size));
            context.set(format!("{name}_odd"), ExprValue::Int(i64::from(odd)));
            context.set(format!("{name}_even"), ExprValue::Bool(!odd));
            context.set(
                format!("{name}_parity"),
                ExprValue::str(if odd { "odd" } else { "even" }),
            );
            self.ops(body, scope, inherit, out)?;
        }
        Ok(())
    }
    /// An evaluated attribute must be in the profile like a written one.
    fn checked(
        &self,
        element: &Element,
        name: &str,
        value: ExprValue,
    ) -> Result<Option<String>, ReportError> {
        let text = match value {
            ExprValue::None | ExprValue::Bool(false) => return Ok(None),
            other => other.to_py_str(),
        };
        match validate_attribute(&element.tag, name, &text, self.program.limits) {
            Ok(_) => Ok(Some(text)),
            Err(ReportError::Profile(found)) => Err(ReportError::Profile(Diagnostic {
                template: element.at.template.as_ref().into(),
                line: element.at.line,
                token: found.token().into(),
                kind: found.kind(),
            })),
            Err(other) => Err(other),
        }
    }
    /// One evaluated attribute. An image value is the `src` of an `<img>` and of nothing else:
    /// it is kept beside the text attributes, which go through the profile.
    fn evaluated(
        &self,
        element: &Element,
        attrs: &mut Attrs,
        name: &str,
        value: ExprValue,
    ) -> Result<(), ReportError> {
        if element.tag == "img"
            && name == "src"
            && let Some(image) = self.records.image(&value)
        {
            attrs.put(name, None);
            attrs.image = Some(image);
            return Ok(());
        }
        attrs.put(name, self.checked(element, name, value)?);
        Ok(())
    }
    /// The element's attributes after evaluation, a later one replacing an earlier one of its name.
    fn attributes(
        &mut self,
        element: &'p Element,
        scope: &Scope<'p>,
    ) -> Result<Attrs, ReportError> {
        let at = &element.at;
        let mut attrs = Attrs::default();
        for attr in &element.attrs {
            match attr {
                Attr::Static { name, value } => attrs.put(name, Some(value.clone())),
                Attr::Value { name, code } => {
                    let value = self.eval(code, scope, at)?;
                    self.evaluated(element, &mut attrs, name, value)?;
                }
                Attr::Format { name, parts } => {
                    let value = ExprValue::str(self.format(parts, scope, at)?);
                    self.evaluated(element, &mut attrs, name, value)?;
                }
                Attr::Mapping(code) => match self.eval(code, scope, at)? {
                    ExprValue::None | ExprValue::Bool(false) => {}
                    ExprValue::Dict(map) => {
                        for (name, value) in map.iter() {
                            let ExprValue::Str(name) = name else {
                                return Err(at.evaluate("t-att names must be strings"));
                            };
                            self.evaluated(element, &mut attrs, name, value.clone())?;
                        }
                    }
                    other => {
                        return Err(at.evaluate(format!(
                            "t-att needs a mapping, got a value of type {}",
                            other.type_name()
                        )));
                    }
                },
            }
        }
        Ok(attrs)
    }
    /// The element's attributes after evaluation, as its appearance.
    fn resolve(&mut self, element: &'p Element, scope: &Scope<'p>) -> Result<Look, ReportError> {
        Ok(self.attributes(element, scope)?.look(&element.tag))
    }
    /// `<img>`: the image of an embedded `src` or of an image field, at the size its attributes
    /// and style give. Without a source it prints nothing, as the reference leaves it empty.
    fn image(
        &mut self,
        element: &'p Element,
        scope: &Scope<'p>,
        inherit: &Inherit,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        let at = &element.at;
        let attrs = self.attributes(element, scope)?;
        if attrs.look("img").hidden {
            return Ok(());
        }
        let data: Arc<[u8]> = match (&attrs.image, attrs.get("src")) {
            (Some(image), _) => image.clone(),
            (None, Some(source)) if embedded_image(source) => {
                let bytes = embedded_bytes(source, &self.options.limits)?;
                self.charge(bytes.len())?;
                bytes.into()
            }
            // The data stage does not read an image by its path yet.
            (None, Some(_)) => {
                return Err(ReportError::Unsupported {
                    feature: "images read by their path while printing",
                    task: "a later release",
                });
            }
            (None, None) => return Ok(()),
        };
        let (current, base) = (inherit.text.size, self.base);
        // The style wins over the `width` and `height` attributes; a bare number is pixels.
        let sides = |name: &str| {
            let styled = attrs.style(name).filter(|value| *value != "auto");
            let written = styled.or_else(|| attrs.get(name).filter(|_| !name.starts_with("max")));
            written
                .and_then(|value| match value.parse::<u32>() {
                    Ok(pixels) => Some(Size::Points(f64::from(pixels) * POINTS_PER_PIXEL)),
                    Err(_) => look::length(value, false),
                })
                .map(|size| size.points(current, base))
        };
        let wanted = [
            sides("width"),
            sides("height"),
            sides("max-width"),
            sides("max-height"),
        ];
        let alt = attrs.get("alt").unwrap_or_default().to_owned();
        self.picture(data, wanted, alt, at, out)
    }
    /// An image node of `data`: `[width, height, max-width, max-height]` in points size it, the
    /// side that is not given follows the image's proportions, and nothing given is its natural
    /// size (a pixel is 0.75 points). A user SVG has no pixel size: it takes what is given.
    fn picture(
        &mut self,
        data: Arc<[u8]>,
        [width, height, max_width, max_height]: [Option<f64>; 4],
        alt: String,
        at: &At,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        let info = probe_image(&data, &self.options.limits)?;
        let natural = (info.width() > 0 && info.height() > 0).then(|| {
            (
                f64::from(info.width()) * POINTS_PER_PIXEL,
                f64::from(info.height()) * POINTS_PER_PIXEL,
            )
        });
        let (width, height) = match natural {
            Some((wide, high)) => {
                let (mut w, mut h) = match (width, height) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => (w, w * high / wide),
                    (None, Some(h)) => (h * wide / high, h),
                    (None, None) => (wide, high),
                };
                // A maximum shrinks both sides alike, as `height: auto` does beside it.
                let scale = [max_width.map(|max| max / w), max_height.map(|max| max / h)]
                    .into_iter()
                    .flatten()
                    .fold(1.0, f64::min);
                w *= scale;
                h *= scale;
                (Some(w), Some(h))
            }
            None => (width.or(max_width), height.or(max_height)),
        };
        if [width, height]
            .into_iter()
            .flatten()
            .any(|side| !side.is_finite() || side <= 0.0 || side > IMAGE_POINTS_MAX)
        {
            return Err(at.evaluate(format!(
                "the width and height of an image must be in (0, {IMAGE_POINTS_MAX}] points"
            )));
        }
        self.node(
            Node::Image {
                data,
                width,
                height,
                alt,
            },
            false,
            out,
        )
    }
    fn own(&self, tag: &str, look: &Look, inherit: &Inherit) -> Own {
        let mut text = inherit.text;
        if let Some(size) = look.size {
            text.size = size.points(text.size, self.base);
        }
        text.bold = look.bold.unwrap_or(text.bold);
        text.italic = look.italic.unwrap_or(text.italic);
        text.underline |= look.underline;
        text.color = match look.color {
            Some(Tint::Rgb(color)) => color,
            Some(Tint::Primary) => self.options.primary,
            Some(Tint::Secondary) => self.options.secondary,
            None => text.color,
        };
        let align = match look.align {
            Some(Alignment::Start) => 0,
            Some(Alignment::Center) => 1,
            Some(Alignment::End) => 2,
            None => inherit.align,
        };
        let padding = look
            .padding
            .map_or(0.0, |padding| padding.points(text.size, self.base));
        let space = |size: Option<Size>| size.map_or(0.0, |s| s.points(text.size, self.base));
        let (above, below) = (space(look.above), space(look.below));
        let rules = (look.rule_above, look.rule_below);
        let table = look.table.unwrap_or_default();
        let decorated = crate::document::decorates(above, below, look.fill, rules, table);
        let text_changed = text != inherit.text;
        let box_changed = align != inherit.align || padding > 0.0 || look.keep || decorated;
        let plain_block = is_block(tag)
            && padding == 0.0
            && !look.keep
            && !decorated
            && look.role == Role::Plain
            && !matches!(
                tag,
                "table" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" | "ul" | "ol" | "li"
            );
        Own {
            inherit: Inherit {
                text,
                align,
                flow: inherit.flow && (tag == "t" || plain_block),
                head: tag != "table" && (inherit.head || tag == "thead"),
                pending: is_table_part(tag) && (inherit.pending || text_changed || box_changed),
            },
            padding,
            keep: look.keep,
            above,
            below,
            fill: look.fill,
            rules,
            table,
            text_changed,
            box_changed,
        }
    }
    fn style(&self, own: &Own, boxed: bool) -> Result<Style, ReportError> {
        let text = own.inherit.text;
        let style = Style::default()
            .with_size(text.size)?
            .with_bold(text.bold)
            .with_italic(text.italic)
            .with_underline(text.underline)
            .with_color(text.color);
        if !boxed {
            return Ok(style);
        }
        Ok(style
            .with_padding(own.padding)?
            .with_alignment(match own.inherit.align {
                1 => Alignment::Center,
                2 => Alignment::End,
                _ => Alignment::Start,
            })
            .with_keep_together(own.keep)
            .with_spacing(own.above, own.below)?
            .with_fill(own.fill)
            .with_rules(own.rules.0, own.rules.1)
            .with_table_look(own.table))
    }
    fn node(&mut self, node: Node, block: bool, out: &mut Vec<Item>) -> Result<(), ReportError> {
        let id = self.builder.add(node)?;
        out.push(Item::Node {
            id,
            block,
            column: None,
        });
        Ok(())
    }
    fn page_break(
        &mut self,
        inherit: &Inherit,
        at: &At,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        if !inherit.flow {
            return Err(at.evaluate(
                "a page break inside a table, grid, list, inline element or padded block cannot be laid out",
            ));
        }
        self.node(Node::PageBreak, true, out)
    }
    /// Turn items into nodes; the flag says whether every one is a block.
    fn nodes(
        &mut self,
        items: Vec<Item>,
        block: bool,
        at: &At,
    ) -> Result<(Vec<NodeId>, bool), ReportError> {
        let mut ids = Vec::new();
        let mut blocks = true;
        for item in tidy(items, block) {
            match item {
                Item::Text(text) => {
                    blocks = false;
                    ids.push(self.builder.add(Node::Text(text))?);
                }
                Item::Node { id, block, .. } => {
                    blocks &= block;
                    ids.push(id);
                }
                Item::Cell(..) | Item::Row { .. } | Item::Entry(_) => {
                    return Err(at.evaluate(
                        "a table row, table cell or list item outside its table or list",
                    ));
                }
            }
        }
        Ok((ids, blocks))
    }
    /// The content of a cell, list item or page region as one node.
    fn single(
        &mut self,
        items: Vec<Item>,
        own: &Own,
        styled: bool,
        at: &At,
    ) -> Result<NodeId, ReportError> {
        let (mut ids, _) = self.nodes(items, true, at)?;
        if !styled && ids.len() == 1 {
            return Ok(ids.remove(0));
        }
        if ids.is_empty() {
            ids.push(self.builder.add(Node::Text(String::new()))?);
        }
        let style = self.style(own, true)?;
        self.builder.add(Node::Block {
            children: ids,
            style,
        })
    }
    /// Wrap a table, grid or list in a block when its element changed the style.
    fn wrapped(
        &mut self,
        id: NodeId,
        own: &Own,
        look: &Look,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        let id = if own.styled() {
            let style = self.style(own, true)?;
            self.builder.add(Node::Block {
                children: vec![id],
                style,
            })?
        } else {
            id
        };
        out.push(Item::Node {
            id,
            block: true,
            column: look.column,
        });
        Ok(())
    }
    fn element(
        &mut self,
        element: &'p Element,
        scope: &mut Scope<'p>,
        inherit: &Inherit,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        let at = &element.at;
        let tag = element.tag.as_str();
        if tag == "img" {
            return self.image(element, scope, inherit, out);
        }
        let resolved;
        let look = match &element.look {
            Some(look) => look,
            None => {
                resolved = self.resolve(element, scope)?;
                &resolved
            }
        };
        // A hidden element prints nothing and its content is not evaluated.
        if look.hidden {
            return Ok(());
        }
        if look.icon {
            return Err(ReportError::Unsupported {
                feature: "icons",
                task: VECTORS_TASK,
            });
        }
        if look.spans {
            return Err(ReportError::Unsupported {
                feature: "table row spans",
                task: "a later release",
            });
        }
        let own = self.own(tag, look, inherit);
        if ![own.above, own.below]
            .iter()
            .all(|v| v.is_finite() && (0.0..=1440.0).contains(v))
        {
            return Err(at.evaluate(format!(
                "the spacing of <{tag}> must be in [0, 1440] points (margin or m-/mt-/mb-/my- class)"
            )));
        }
        let mut kids = Vec::new();
        if !self.content(element, scope, &own.inherit, &mut kids)? {
            return Ok(());
        }
        if look.break_before {
            self.page_break(inherit, at, out)?;
        }
        self.place(tag, look, &own, inherit, kids, at, out)?;
        if look.break_after {
            self.page_break(inherit, at, out)?;
        }
        Ok(())
    }
    fn options_of(
        &mut self,
        options: Option<&Code>,
        scope: &Scope<'p>,
        at: &At,
    ) -> Result<Option<Map>, ReportError> {
        let Some(options) = options else {
            return Ok(None);
        };
        match self.eval(options, scope, at)? {
            ExprValue::None | ExprValue::Bool(false) => Ok(None),
            ExprValue::Dict(map) => Ok(Some(map)),
            other => Err(at.evaluate(format!(
                "t-options needs a mapping, got a value of type {}",
                other.type_name()
            ))),
        }
    }
    /// Evaluate what the element prints; `false` when it prints nothing and has no default body,
    /// in which case the element itself is left out, as in the reference.
    fn content(
        &mut self,
        element: &'p Element,
        scope: &mut Scope<'p>,
        inherit: &Inherit,
        kids: &mut Vec<Item>,
    ) -> Result<bool, ReportError> {
        let at = &element.at;
        let (printed, default) = match &element.content {
            Content::Children(ops) => {
                self.ops(ops, scope, inherit, kids)?;
                return Ok(true);
            }
            Content::Out { value: None, .. } => {
                if let Some(slot) = scope.slot.clone() {
                    self.closure(&slot, inherit, kids)?;
                }
                return Ok(true);
            }
            Content::Out {
                value: Some(code),
                options,
                default,
            } => {
                let value = self.eval(code, scope, at)?;
                let options = self.options_of(options.as_ref(), scope, at)?;
                let printed = match &value {
                    ExprValue::None | ExprValue::Bool(false) => None,
                    ExprValue::Object(object) if object.type_name() == FRAGMENT => {
                        let closure = object
                            .as_int()
                            .ok()
                            .and_then(|index| usize::try_from(index).ok())
                            .and_then(|index| self.closures.get(index))
                            .cloned()
                            .ok_or_else(|| {
                                at.evaluate("a template fragment of another document")
                            })?;
                        self.closure(&closure, inherit, kids)?;
                        return Ok(true);
                    }
                    value => widget::value(&self.env(), value, options.as_ref(), at)?,
                };
                (printed, default)
            }
            Content::Field {
                record,
                field,
                options,
                default,
            } => {
                let value = self.eval(record, scope, at)?;
                let options = self.options_of(options.as_ref(), scope, at)?;
                let ExprValue::Object(object) = &value else {
                    return Err(at.evaluate(format!(
                        "t-field needs a record, got a value of type {}",
                        value.type_name()
                    )));
                };
                let model = object.type_name();
                let ids = object
                    .record_ids()
                    .filter(|_| object.is_record())
                    .ok_or_else(|| at.evaluate(format!("t-field needs a record, got {model}")))?;
                let printed = match ids.as_ref() {
                    [] => None,
                    [id] => {
                        let loaded = self
                            .records
                            .get(model, *id)
                            .and_then(|record| record.fields.get(field))
                            .ok_or_else(|| {
                                at.evaluate(format!(
                                    "field {model}.{field} of record {id} was not loaded for this report"
                                ))
                            })?;
                        widget::field(&self.env(), (model, *id), loaded, options.as_ref(), at)?
                    }
                    more => {
                        return Err(at.evaluate(format!(
                            "t-field needs one record, got {} of {model}",
                            more.len()
                        )));
                    }
                };
                (printed, default)
            }
        };
        match printed {
            Some(pieces) => {
                for piece in pieces {
                    match piece {
                        Piece::Text(text) => self.text(kids, &text)?,
                        Piece::Break => self.node(Node::LineBreak, false, kids)?,
                        Piece::Image {
                            data,
                            max: [wide, high],
                        } => {
                            let sides = [None, None, wide, high];
                            self.picture(data, sides, String::new(), at, kids)?;
                        }
                    }
                }
                Ok(true)
            }
            None if default.is_empty() => Ok(false),
            None => {
                self.ops(default, scope, inherit, kids)?;
                Ok(true)
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        tag: &str,
        look: &Look,
        own: &Own,
        inherit: &Inherit,
        kids: Vec<Item>,
        at: &At,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        match look.role {
            Role::PageNumber => {
                return self.node(Node::PageCounter(PageCounter::Current), false, out);
            }
            Role::PageTotal => return self.node(Node::PageCounter(PageCounter::Total), false, out),
            Role::Header | Role::Footer => {
                let id = self.single(kids, own, own.styled(), at)?;
                let (region, name) = match look.role {
                    Role::Header => (&mut self.header, "header"),
                    _ => (&mut self.footer, "footer"),
                };
                if region.replace(id).is_some() {
                    return Err(at.evaluate(format!("a second page {name} in one document")));
                }
                return Ok(());
            }
            Role::Row | Role::Flex => {
                let mut cells = Vec::new();
                let mut widths = Vec::new();
                for item in tidy(kids, true) {
                    match item {
                        Item::Text(text) => {
                            cells.push(self.builder.add(Node::Text(text))?);
                            widths.push(None);
                        }
                        Item::Node { id, column, .. } => {
                            cells.push(id);
                            widths.push(column.filter(|_| look.role == Role::Row));
                        }
                        Item::Cell(..) | Item::Row { .. } | Item::Entry(_) => {
                            return Err(at.evaluate(
                                "a table row, table cell or list item outside its table or list",
                            ));
                        }
                    }
                }
                if cells.is_empty() {
                    return Ok(());
                }
                let gutter = match look.role {
                    Role::Row => 1.5 * self.base,
                    _ => look
                        .gap
                        .map_or(0.0, |gap| gap.points(own.inherit.text.size, self.base)),
                };
                let align = look.cross.unwrap_or_default();
                let lines = match look.role {
                    Role::Row => row_lines(&widths),
                    _ => std::iter::once(0..cells.len()).collect(),
                };
                if let [_] = lines.as_slice() {
                    let grid = self.builder.add(Node::Grid {
                        columns: weights(&widths),
                        gutter,
                        cells,
                        align,
                    })?;
                    return self.wrapped(grid, own, look, out);
                }
                // Columns adding up to more than twelve go on to further lines, each on the same
                // twelve-column grid: a short last line keeps its widths and leaves the rest empty.
                let mut grids = Vec::with_capacity(lines.len());
                for line in lines {
                    let mut line_cells = cells[line.clone()].to_vec();
                    let mut line_widths = widths[line].to_vec();
                    let used: u32 = line_widths.iter().flatten().map(|n| u32::from(*n)).sum();
                    if let Ok(rest) = u8::try_from(12u32.saturating_sub(used))
                        && rest > 0
                    {
                        line_cells.push(self.builder.add(Node::Text(String::new()))?);
                        line_widths.push(Some(rest));
                    }
                    grids.push(self.builder.add(Node::Grid {
                        columns: weights(&line_widths),
                        gutter,
                        cells: line_cells,
                        align,
                    })?);
                }
                let lines = self.builder.add(Node::Block {
                    children: grids,
                    style: Style::default(),
                })?;
                return self.wrapped(lines, own, look, out);
            }
            Role::Plain => {}
        }
        let misplaced = |what: &str| at.evaluate(format!("{what} holds something else"));
        match tag {
            "t" => extend(out, kids),
            "br" => self.node(Node::LineBreak, false, out)?,
            "hr" => self.node(Node::Rule, true, out)?,
            "td" | "th" => {
                let styled = own.styled() || inherit.pending;
                let id = self.single(kids, own, styled, at)?;
                let span = match &look.colspan {
                    None => 1,
                    Some(Ok(span)) if *span <= MAX_COLSPAN => *span,
                    Some(Ok(_) | Err(_)) => {
                        return Err(at.evaluate(format!(
                            "colspan must be a whole number from 1 to {MAX_COLSPAN}"
                        )));
                    }
                };
                let id = if span > 1 {
                    self.builder.add(Node::Cell {
                        child: id,
                        colspan: span,
                        rowspan: 1,
                    })?
                } else {
                    id
                };
                out.push(Item::Cell(id, span));
            }
            "li" => out.push(Item::Entry(self.single(kids, own, own.styled(), at)?)),
            "tr" => {
                let mut cells = Vec::new();
                for item in kids {
                    match item {
                        Item::Cell(id, span) => cells.push((id, span)),
                        Item::Text(text) if blank(&text) => {}
                        _ => return Err(misplaced("a table row that should hold cells")),
                    }
                }
                out.push(Item::Row {
                    cells,
                    head: inherit.head,
                });
            }
            "thead" | "tbody" | "tfoot" => {
                for item in kids {
                    match item {
                        Item::Row { .. } => out.push(item),
                        Item::Text(text) if blank(&text) => {}
                        _ => return Err(misplaced("a table section that should hold rows")),
                    }
                }
            }
            "table" => {
                let mut rows = Vec::new();
                let mut columns = 0;
                for item in kids {
                    match item {
                        Item::Row { cells, head } => {
                            columns = columns.max(cells.iter().map(|(_, span)| span).sum());
                            rows.push((cells, head));
                        }
                        Item::Text(text) if blank(&text) => {}
                        _ => return Err(misplaced("a table that should hold rows")),
                    }
                }
                if columns == 0 {
                    return Ok(());
                }
                let mut header = Vec::new();
                let mut cells = Vec::new();
                for (row, head) in rows {
                    let mut width: usize = row.iter().map(|(_, span)| span).sum();
                    let mut row: Vec<NodeId> = row.into_iter().map(|(id, _)| id).collect();
                    // A short row is filled with empty cells so that the next row starts a line.
                    while width < columns {
                        row.push(self.builder.add(Node::Text(String::new()))?);
                        width += 1;
                    }
                    if head {
                        header.extend(row);
                    } else {
                        cells.extend(row);
                    }
                }
                let table = self.builder.add(Node::Table {
                    columns,
                    header,
                    cells,
                })?;
                self.wrapped(table, own, look, out)?;
            }
            "ul" | "ol" => {
                let mut items = Vec::new();
                for item in kids {
                    match item {
                        Item::Entry(id) => items.push(id),
                        Item::Text(text) if blank(&text) => {}
                        _ => return Err(misplaced("a list that should hold list items")),
                    }
                }
                if items.is_empty() {
                    return Ok(());
                }
                let list = self.builder.add(Node::List {
                    ordered: tag == "ol",
                    items,
                })?;
                self.wrapped(list, own, look, out)?;
            }
            _ if is_block(tag) => {
                let kids = tidy(kids, true);
                let only_blocks = kids
                    .iter()
                    .all(|kid| matches!(kid, Item::Node { block: true, .. }));
                // A block that changes nothing and holds only blocks is a wrapper: no node.
                if !own.styled() && look.column.is_none() && only_blocks {
                    out.extend(kids);
                    return Ok(());
                }
                let (mut ids, blocks) = self.nodes(kids, true, at)?;
                if ids.is_empty() && look.column.is_none() {
                    return Ok(());
                }
                let id = if !own.styled() && blocks && ids.len() == 1 {
                    ids.remove(0)
                } else {
                    if ids.is_empty() {
                        ids.push(self.builder.add(Node::Text(String::new()))?);
                    }
                    let style = self.style(own, true)?;
                    self.builder.add(Node::Block {
                        children: ids,
                        style,
                    })?
                };
                out.push(Item::Node {
                    id,
                    block: true,
                    column: look.column,
                });
            }
            _ => {
                // An inline element is a node only when it changes the text style.
                if !own.text_changed {
                    extend(out, kids);
                    return Ok(());
                }
                let kids = tidy(kids, false);
                if kids.is_empty() {
                    return Ok(());
                }
                // Inline content that holds a block is laid out as a block by what surrounds it.
                let block = kids.iter().any(Item::boundary);
                let (children, _) = self.nodes(kids, false, at)?;
                let style = self.style(own, false)?;
                let id = self.builder.add(Node::Inline { children, style })?;
                out.push(Item::Node {
                    id,
                    block,
                    column: None,
                });
            }
        }
        Ok(())
    }
}
