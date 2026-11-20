// SPDX-License-Identifier: MIT OR Apache-2.0
//! The line model (design §6): generic line ids, typed cells, lines and expansions.
//!
//! ```text
//! LineId  = Segment ("|" Segment)*
//! Segment = kind "~" model "~" value     kind: root | line:<code> | group:<field> | total | more
//! ```
//!
//! `|`, `~` and `%` inside a value are percent-escaped. [`LineId::parse`] checks the syntax only; the
//! provider checks an id against its report (group-by chain, comodels, depth) before any query.
use std::fmt;

use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};

/// Bytes of one line id: a protocol bound, not an administrator's choice;
/// the depth bound is the `reporting.max_unfold_depth` setting.
pub const MAX_LINE_ID_BYTES: usize = 2048;
const MAX_NAME_BYTES: usize = 64;

/// What a segment stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SegmentKind {
    /// The report itself: `root~reporting.report~<id>`.
    Root,
    /// A definition line, by its code.
    Line(String),
    /// A group of the records behind the parent, by the grouped field.
    Group(String),
    /// The total line below an unfolded line.
    Total,
    /// The load-more line of a paged group.
    More,
}

/// One segment of a [`LineId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    kind: SegmentKind,
    model: String,
    value: String,
}

impl Segment {
    /// A definition line.
    pub fn line(code: &str) -> Self {
        Self::new(SegmentKind::Line(code.to_owned()), "", "")
    }
    /// A group of `field`: `model` is the comodel of a relational field (empty otherwise),
    /// `value` the record id or the scalar value, empty for the group of records without one.
    pub fn group(field: &str, model: &str, value: &str) -> Self {
        Self::new(SegmentKind::Group(field.to_owned()), model, value)
    }
    /// The total line.
    pub fn total() -> Self {
        Self::new(SegmentKind::Total, "", "")
    }
    /// The load-more line.
    pub fn more() -> Self {
        Self::new(SegmentKind::More, "", "")
    }
    fn new(kind: SegmentKind, model: &str, value: &str) -> Self {
        Self {
            kind,
            model: model.to_owned(),
            value: value.to_owned(),
        }
    }
    /// The kind.
    pub fn kind(&self) -> &SegmentKind {
        &self.kind
    }
    /// The model, empty when none.
    pub fn model(&self) -> &str {
        &self.model
    }
    /// The value, unescaped; empty when none.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// The id of a line: the chain of segments naming it and every ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineId(Vec<Segment>);

impl LineId {
    /// The root of report `report`.
    pub fn root(report: i64) -> Self {
        Self(vec![Segment::new(
            SegmentKind::Root,
            "reporting.report",
            &report.to_string(),
        )])
    }
    /// This id with `segment` appended.
    #[must_use]
    pub fn child(&self, segment: Segment) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment);
        Self(segments)
    }
    /// The parent's id; `None` for a root.
    pub fn parent(&self) -> Option<Self> {
        (self.0.len() > 1).then(|| Self(self.0[..self.0.len() - 1].to_vec()))
    }
    /// The segments, root first.
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }
    /// The report id of the root segment.
    pub fn report(&self) -> i64 {
        // parse and root guarantee a positive id in the root segment
        self.0
            .first()
            .and_then(|root| root.value.parse().ok())
            .unwrap_or(0)
    }
    /// Lines below the report: 0 for a top-level definition line.
    pub fn level(&self) -> u32 {
        u32::try_from(self.0.len().saturating_sub(2)).unwrap_or(u32::MAX)
    }
    /// Whether `field`'s group is among the segments.
    pub fn has_group(&self, field: &str) -> bool {
        self.0
            .iter()
            .any(|s| matches!(&s.kind, SegmentKind::Group(f) if f == field))
    }

    /// Parse an id sent by the browser (syntax only).
    ///
    /// # Errors
    /// `Validation` naming the segment for an id over [`MAX_LINE_ID_BYTES`], a segment that is not
    /// `kind~model~value`, an unknown kind, a malformed name or escape, or a first segment that is
    /// not the root of a report.
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_LINE_ID_BYTES {
            return Err(Error::validation(format!(
                "A line id is longer than {MAX_LINE_ID_BYTES} bytes."
            )));
        }
        let mut segments = Vec::new();
        for (index, raw) in text.split('|').enumerate() {
            let bad = |what: &str| {
                Error::validation(format!("Line id segment {} ('{raw}') {what}.", index + 1))
            };
            let parts: Vec<&str> = raw.split('~').collect();
            let [kind, model, value] = parts.as_slice() else {
                return Err(bad("is not kind~model~value"));
            };
            let kind = match (*kind, kind.split_once(':')) {
                ("root", _) => SegmentKind::Root,
                ("total", _) => SegmentKind::Total,
                ("more", _) => SegmentKind::More,
                (_, Some(("line", code))) if name(code) => SegmentKind::Line(code.to_owned()),
                (_, Some(("group", field))) if name(field) => SegmentKind::Group(field.to_owned()),
                _ => return Err(bad("has an unknown kind")),
            };
            if !(model.is_empty() || model_name(model)) {
                return Err(bad("has a malformed model"));
            }
            let value = unescape(value).ok_or_else(|| bad("has a malformed escape"))?;
            let root = matches!(kind, SegmentKind::Root);
            if root != (index == 0)
                || root
                    && (*model != "reporting.report"
                        || !value.parse::<i64>().is_ok_and(|id| id >= 1))
            {
                return Err(bad("is not where a root of a report belongs"));
            }
            segments.push(Segment::new(kind, model, &value));
        }
        Ok(Self(segments))
    }
}

impl fmt::Display for LineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str("|")?;
            }
            match &segment.kind {
                SegmentKind::Root => f.write_str("root")?,
                SegmentKind::Line(code) => write!(f, "line:{code}")?,
                SegmentKind::Group(field) => write!(f, "group:{field}")?,
                SegmentKind::Total => f.write_str("total")?,
                SegmentKind::More => f.write_str("more")?,
            }
            write!(f, "~{}~{}", segment.model, escape(&segment.value))?;
        }
        Ok(())
    }
}

fn name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_NAME_BYTES
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn model_name(text: &str) -> bool {
    text.len() <= MAX_NAME_BYTES
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' => out.push_str("%25"),
            '|' => out.push_str("%7C"),
            '~' => out.push_str("%7E"),
            other => out.push(other),
        }
    }
    out
}

fn unescape(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The kind of a cell's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CellKind {
    /// An amount in the cell's currency.
    Monetary,
    /// A decimal number at the cell's scale.
    Decimal,
    /// A whole number.
    Integer,
    /// A percentage at the cell's scale.
    Percent,
    /// A date (`YYYY-MM-DD` text).
    Date,
    /// Text.
    Text,
    /// Nothing.
    Empty,
}

impl CellKind {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Monetary => "monetary",
            Self::Decimal => "decimal",
            Self::Integer => "integer",
            Self::Percent => "percent",
            Self::Date => "date",
            Self::Text => "text",
            Self::Empty => "empty",
        }
    }
    /// Parse a column kind of a definition.
    ///
    /// # Errors
    /// `Validation` naming the kind.
    pub fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "monetary" => Self::Monetary,
            "decimal" => Self::Decimal,
            "integer" => Self::Integer,
            "percent" => Self::Percent,
            "date" => Self::Date,
            "text" => Self::Text,
            "empty" => Self::Empty,
            other => return Err(Error::validation(format!("Unknown cell kind '{other}'."))),
        })
    }
}

/// One cell: a number travels as a decimal string at its scale, never as a float.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    kind: CellKind,
    number: Option<Decimal>,
    text: Option<String>,
    scale: u32,
    currency: Option<i64>,
}

impl Cell {
    /// A number of `kind` at `scale`; `None` shows nothing.
    pub fn number(kind: CellKind, value: Option<Decimal>, scale: u32) -> Self {
        let scale = if kind == CellKind::Integer { 0 } else { scale };
        Self {
            kind,
            number: value,
            text: None,
            scale,
            currency: None,
        }
    }
    /// An amount in `currency`, at that currency's decimal places.
    pub fn monetary(value: Option<Decimal>, scale: u32, currency: Option<i64>) -> Self {
        Self {
            currency,
            ..Self::number(CellKind::Monetary, value, scale)
        }
    }
    /// A text cell.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: CellKind::Text,
            number: None,
            text: Some(text.into()),
            scale: 0,
            currency: None,
        }
    }
    /// A calendar date, carried as ISO text with the date wire kind.
    pub fn date(value: Date) -> Self {
        let (year, month, day) = value.ymd();
        Self {
            kind: CellKind::Date,
            text: Some(format!("{year:04}-{month:02}-{day:02}")),
            ..Self::empty()
        }
    }
    /// An empty cell.
    pub fn empty() -> Self {
        Self::number(CellKind::Empty, None, 0)
    }
    /// The number, if any.
    pub fn value(&self) -> Option<Decimal> {
        self.number
    }
    /// The decimals the number is shown with.
    pub fn scale(&self) -> u32 {
        self.scale
    }
    /// The text of a text or date cell.
    pub fn text_value(&self) -> Option<&str> {
        self.text.as_deref()
    }
    /// What the cell shows: the number at its scale, the text, or nothing.
    pub fn display(&self) -> String {
        match (&self.number, &self.text) {
            (Some(n), _) => format!("{:.*}", self.scale as usize, n.round_dp(self.scale)),
            (None, Some(text)) => text.clone(),
            (None, None) => String::new(),
        }
    }
    /// The kind.
    pub fn kind(&self) -> CellKind {
        self.kind
    }
    /// Zero at the cell's own precision; text and empty cells count as zero for
    /// hiding lines.
    pub fn is_zero(&self) -> bool {
        self.number.is_none_or(|n| n.round_dp(self.scale).is_zero())
    }
    /// This cell with nothing shown when it is zero (a column's `blank_if_zero`).
    #[must_use]
    pub fn blank_if_zero(mut self) -> Self {
        if self.is_zero() {
            self.number = None;
        }
        self
    }
    fn to_json(&self) -> Json {
        let value = match (&self.number, &self.text) {
            (Some(n), _) => json!(format!(
                "{:.*}",
                self.scale as usize,
                n.round_dp(self.scale)
            )),
            (None, Some(text)) => json!(text),
            (None, None) => Json::Null,
        };
        json!({"kind": self.kind.as_str(), "value": value, "scale": self.scale,
               "currency": self.currency, "zero": self.is_zero()})
    }
}

/// What a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LineKind {
    /// A definition line.
    Section,
    /// A group that unfolds further.
    Group,
    /// A group at the last level.
    Detail,
    /// The total below an unfolded line.
    Total,
    /// The load-more line.
    More,
}

impl LineKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Group => "group",
            Self::Detail => "detail",
            Self::Total => "total",
            Self::More => "more",
        }
    }
}

/// One line of the answer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Line {
    /// Its id; the parent and the level follow from it.
    pub id: LineId,
    /// The name shown in the first column.
    pub name: String,
    /// What it is.
    pub kind: LineKind,
    /// Whether it can be unfolded.
    pub unfoldable: bool,
    /// Whether its children follow in this answer.
    pub unfolded: bool,
    /// One cell per column of the report options' `columns`.
    pub cells: Vec<Cell>,
    /// The drill items of its caret menu (`open`).
    pub caret: Vec<&'static str>,
    /// For a load-more line: the children not loaded yet.
    pub remaining: Option<u64>,
    /// For a load-more line: the offset of the next page.
    pub next_offset: Option<u64>,
}

impl Line {
    /// A line with no caret item, folded.
    pub fn new(id: LineId, name: impl Into<String>, kind: LineKind, cells: Vec<Cell>) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            unfoldable: false,
            unfolded: false,
            cells,
            caret: Vec::new(),
            remaining: None,
            next_offset: None,
        }
    }
    /// Whether every cell is zero.
    pub fn is_zero(&self) -> bool {
        self.cells.iter().all(Cell::is_zero)
    }
    /// The line as the screen gets it.
    pub fn to_json(&self) -> Json {
        json!({
            "id": self.id.to_string(),
            "parent": self.id.parent().filter(|p| p.segments().len() > 1).map(|p| p.to_string()),
            "name": self.name,
            "level": self.id.level(),
            "kind": self.kind.as_str(),
            "unfoldable": self.unfoldable,
            "unfolded": self.unfolded,
            "cells": self.cells.iter().map(Cell::to_json).collect::<Vec<_>>(),
            "caret": self.caret,
            "remaining": self.remaining,
            "next_offset": self.next_offset,
        })
    }
}

/// One page of children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExpandPage {
    /// Children already loaded before this page.
    pub offset: u64,
    /// Children in this page at most.
    pub limit: u64,
}

/// The children of a line, one page, a load-more line last when more exist.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Expansion {
    /// The children.
    pub lines: Vec<Line>,
    /// The names of optional lines left out because the caller may not read them.
    pub hidden: Vec<String>,
}

impl Expansion {
    /// The children, nothing hidden.
    pub fn new(lines: Vec<Line>) -> Self {
        Self {
            lines,
            hidden: Vec::new(),
        }
    }
}

/// What a drill-down asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DrillTarget {
    /// A caret item of the line (`open`).
    Item(String),
    /// The records behind one cell, by its index in the columns.
    Cell(usize),
}

impl DrillTarget {
    /// `{"item": "open"}` or `{"cell": <index>}`.
    ///
    /// # Errors
    /// `Validation` for any other shape.
    pub fn parse(target: &Json) -> Result<Self> {
        let refused =
            || Error::validation("A drill target is {\"item\": name} or {\"cell\": index}.");
        let object = target
            .as_object()
            .filter(|o| o.len() == 1)
            .ok_or_else(refused)?;
        match (object.get("item"), object.get("cell")) {
            (Some(Json::String(item)), None) if name(item) => Ok(Self::Item(item.clone())),
            (None, Some(cell)) => cell
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .map(Self::Cell)
                .ok_or_else(refused),
            _ => Err(refused()),
        }
    }
}

#[cfg(test)]
#[path = "tests/line.rs"]
mod tests;
