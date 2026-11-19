// SPDX-License-Identifier: MIT OR Apache-2.0
//! Report definitions and the definition-driven provider `reporting.definition` (design §7).
//!
//! [`load`] reads a `reporting.report` with its columns, lines and expressions as the caller and
//! checks it whole; the same check runs when a definition record is saved (`strict = false` there:
//! a reference to a line or label that the next record of a data file creates is not refused yet).
//!
//! Engines: `aggregate` is one `read_group` over the expression's model, domain, period and
//! companies; unfolding a line groups by the next field of its group-by chain with the parent keys
//! in the domain. `formula` evaluates over the values of the same period ([`crate::formula`]).
use std::collections::BTreeMap;

use erp_sdk::prelude::*;
use erp_sdk::serde_json::{self, Value as Json, json};
use erp_sdk::{BoxFuture, ErrorKind, Load};

use crate::formula::{self, EvalError, Ref, Term};
use crate::line::{
    Cell, CellKind, DrillTarget, ExpandPage, Expansion, Line, LineId, LineKind, Segment,
    SegmentKind,
};
use crate::options::{DateMode, Options, Period, Preset};
use crate::provider::{ProviderSpec, ReportCx, ReportProvider};
use crate::settings::{self, Bounds};

/// The name of the definition-driven provider.
pub const DEFINITION: &str = "reporting.definition";
const MIN_LOAD_MORE: u64 = 10;
// decimals a column may show: rust_decimal holds 28; twelve is far past any currency or unit
const MAX_SCALE: u32 = 12;
const MAX_DOMAIN_BYTES: usize = 16 * 1024;

/// When zero lines may be hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HideZero {
    /// Never.
    Never,
    /// Offered, off at first.
    Optional,
    /// Offered, on at first.
    ByDefault,
}

/// A report column.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ColumnDef {
    /// Header text.
    pub name: String,
    /// The expression label its cells show.
    pub label: String,
    /// The cell kind.
    pub kind: CellKind,
    /// Decimals (monetary cells use the currency's).
    pub scale: u32,
    /// Lines may be sorted by it.
    pub sortable: bool,
    /// Zero shows as nothing.
    pub blank_if_zero: bool,
    /// `rollup = none`: cells show only on lines grouped by this field (quantities of one unit).
    pub rollup_field: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Foldable {
    OnDemand,
    Always,
    Never,
}

#[derive(Debug, Clone)]
struct Agg {
    model: String,
    domain_text: String,
    domain: Domain,
    field: Option<String>,
    // the date field and whether it is a datetime
    date: Option<(String, bool)>,
    from_beginning: bool,
}

#[derive(Debug, Clone)]
enum Engine {
    Aggregate(Agg),
    Formula { term: Term, zero_empty: bool },
}

#[derive(Debug, Clone)]
struct ExprDef {
    label: String,
    engine: Engine,
}

#[derive(Debug, Clone)]
struct GroupField {
    name: String,
    comodel: Option<String>,
    integer: bool,
}

#[derive(Debug, Clone)]
struct LineDef {
    code: String,
    name: String,
    parent: Option<usize>,
    children: Vec<usize>,
    groupby: Vec<String>,
    // the group-by fields resolved on the aggregate model (empty while the line has none)
    group: Vec<GroupField>,
    foldable: Foldable,
    hide_if_zero: bool,
    optional: bool,
    exprs: Vec<ExprDef>,
}

/// A loaded, checked report definition.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReportDef {
    /// The record id.
    pub id: i64,
    /// The name.
    pub name: String,
    /// The provider name.
    pub provider: String,
    pub(crate) date_mode: DateMode,
    pub(crate) date_default: Preset,
    pub(crate) comparison: bool,
    pub(crate) multi_company: bool,
    pub(crate) unfold_all: bool,
    pub(crate) hide_zero: HideZero,
    pub(crate) search_bar: bool,
    pub(crate) totals_below: bool,
    /// Children per unfold page.
    pub load_more_limit: u64,
    /// The columns.
    pub columns: Vec<ColumnDef>,
    lines: Vec<LineDef>,
    // formula expressions in evaluation order: (line, expression)
    order: Vec<(usize, usize)>,
}

impl ReportDef {
    /// Whether a grouped line computes `label` with a formula (such a column is not sortable).
    pub(crate) fn is_formula_label(&self, label: &str) -> bool {
        self.lines.iter().any(|l| {
            !l.groupby.is_empty()
                && l.exprs
                    .iter()
                    .any(|e| e.label == label && matches!(e.engine, Engine::Formula { .. }))
        })
    }
    /// The filter bar this report offers.
    pub(crate) fn filters(&self) -> Json {
        json!({
            "date_mode": match self.date_mode { DateMode::Range => "range", DateMode::Single => "single" },
            "comparison": self.comparison,
            "multi_company": self.multi_company,
            "unfold_all": self.unfold_all,
            "hide_zero": match self.hide_zero {
                HideZero::Never => "never", HideZero::Optional => "optional", HideZero::ByDefault => "by_default" },
            "search_bar": self.search_bar,
            "totals_below": self.totals_below,
        })
    }
    pub(crate) fn totals_below(&self) -> bool {
        self.totals_below
    }
}

/// `[A-Z][A-Z0-9_]*`, at most 64 bytes.
pub(crate) fn is_code(text: &str) -> bool {
    text.len() <= 64
        && text.starts_with(|c: char| c.is_ascii_uppercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}
fn is_label(text: &str) -> bool {
    text.len() <= 64
        && text.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

type Row = BTreeMap<String, Value>;

fn field<'a>(row: &'a Row, key: &str) -> Result<&'a Value> {
    row.get(key)
        .ok_or_else(|| Error::internal(format!("reporting: a definition read lacks {key}")))
}
fn text(row: &Row, key: &str) -> Result<String> {
    match field(row, key)? {
        Value::Str(s) => Ok(s.to_string()),
        Value::False => Ok(String::new()),
        other => Err(Error::internal(format!(
            "reporting: {key} reads as {other:?}"
        ))),
    }
}
fn int(row: &Row, key: &str) -> Result<i64> {
    match field(row, key)? {
        Value::Int(n) => Ok(*n),
        Value::False => Ok(0),
        other => Err(Error::internal(format!(
            "reporting: {key} reads as {other:?}"
        ))),
    }
}
fn flag(row: &Row, key: &str) -> Result<bool> {
    match field(row, key)? {
        Value::Bool(b) => Ok(*b),
        Value::False => Ok(false),
        other => Err(Error::internal(format!(
            "reporting: {key} reads as {other:?}"
        ))),
    }
}

async fn rows(
    env: &Env,
    tx: &mut Transaction,
    model: &str,
    domain: Json,
    fields: &[&str],
) -> Result<Vec<Row>> {
    let found = env
        .records(model)?
        .search(tx, &Domain::from_json(&domain)?)
        .await?;
    found.read_with(tx, fields, Load::Raw).await
}

fn invalid(report: &str, what: String) -> Error {
    Error::validation(format!("Report '{report}': {what}"))
}

/// Load and check report `id` as the caller.
///
/// # Errors
/// `Access` when the caller may not read the definition; `Validation` naming the report and the
/// part for a definition that breaks a rule (design §7), or for a report that does not exist.
pub async fn load(
    env: &Env,
    tx: &mut Transaction,
    id: i64,
    strict: bool,
    bounds: &Bounds,
) -> Result<ReportDef> {
    let report = env
        .records("reporting.report")?
        .browse([id])?
        .read_with(
            tx,
            &[
                "name",
                "provider",
                "date_mode",
                "date_default",
                "comparison",
                "multi_company",
                "unfold_all",
                "hide_zero",
                "search_bar",
                "totals_below",
                "load_more_limit",
            ],
            Load::Raw,
        )
        .await?;
    let report = report
        .first()
        .ok_or_else(|| Error::validation(format!("Report {id} does not exist.")))?;
    let name = text(report, "name")?;
    let fail = |what: String| invalid(&name, what);
    let provider = text(report, "provider")?;
    crate::provider::find(env, &provider)?;
    // a negative limit maps to 0, which the range check below refuses by name
    let load_more_limit = u64::try_from(int(report, "load_more_limit")?).unwrap_or(0);
    if !(MIN_LOAD_MORE..=bounds.max_load_more_limit).contains(&load_more_limit) {
        return Err(fail(format!(
            "Load More Limit must be from {MIN_LOAD_MORE} to {} (setting {}).",
            bounds.max_load_more_limit,
            settings::MAX_LOAD_MORE_LIMIT
        )));
    }
    let date_mode = match text(report, "date_mode")?.as_str() {
        "range" => DateMode::Range,
        "single" => DateMode::Single,
        other => return Err(fail(format!("date mode '{other}' is unknown."))),
    };
    let date_default = Preset::parse(&text(report, "date_default")?, "Default Period")?;
    if date_default == Preset::Custom || !date_default.offered(date_mode) {
        return Err(fail(
            "the default period is not offered in its date mode.".into(),
        ));
    }
    let hide_zero = match text(report, "hide_zero")?.as_str() {
        "never" => HideZero::Never,
        "optional" => HideZero::Optional,
        "by_default" => HideZero::ByDefault,
        other => return Err(fail(format!("hide zero '{other}' is unknown."))),
    };

    let mut columns = Vec::new();
    for row in rows(
        env,
        tx,
        "reporting.report.column",
        json!([["report_id", "=", id]]),
        &[
            "name",
            "label",
            "kind",
            "scale",
            "sortable",
            "blank_if_zero",
            "rollup",
            "rollup_field",
        ],
    )
    .await?
    {
        let label = text(&row, "label")?;
        if !is_label(&label) || columns.iter().any(|c: &ColumnDef| c.label == label) {
            return Err(fail(format!(
                "column label '{label}' is not a unique lower-case name."
            )));
        }
        // a negative scale is refused with the out-of-range one
        let scale = u32::try_from(int(&row, "scale")?)
            .ok()
            .filter(|s| *s <= MAX_SCALE)
            .ok_or_else(|| {
                fail(format!(
                    "column {label}: scale must be from 0 to {MAX_SCALE}."
                ))
            })?;
        let rollup_field = match text(&row, "rollup")?.as_str() {
            "sum" => None,
            "none" => Some(text(&row, "rollup_field")?)
                .filter(|f| !f.is_empty())
                .ok_or_else(|| {
                    fail(format!(
                        "column {label}: a column that does not roll up names its Rollup Field."
                    ))
                })
                .map(Some)?,
            other => {
                return Err(fail(format!(
                    "column {label}: rollup '{other}' is unknown."
                )));
            }
        };
        columns.push(ColumnDef {
            name: text(&row, "name")?,
            kind: CellKind::parse(&text(&row, "kind")?)?,
            scale,
            sortable: flag(&row, "sortable")?,
            blank_if_zero: flag(&row, "blank_if_zero")?,
            rollup_field,
            label,
        });
    }

    let line_rows = rows(
        env,
        tx,
        "reporting.report.line",
        json!([["report_id", "=", id]]),
        &[
            "name",
            "code",
            "parent_id",
            "groupby",
            "foldable",
            "hide_if_zero",
            "optional",
        ],
    )
    .await?;
    let ids: Vec<i64> = line_rows
        .iter()
        .map(|r| int(r, "id"))
        .collect::<Result<_>>()?;
    let mut lines = Vec::with_capacity(line_rows.len());
    for row in &line_rows {
        let code = text(row, "code")?;
        if !is_code(&code) || lines.iter().any(|l: &LineDef| l.code == code) {
            return Err(fail(format!(
                "line code '{code}' is not a unique code (A-Z, 0-9, _)."
            )));
        }
        let parent = match int(row, "parent_id")? {
            0 => None,
            p => Some(ids.iter().position(|i| *i == p).ok_or_else(|| {
                fail(format!(
                    "the parent of line {code} belongs to another report."
                ))
            })?),
        };
        let groupby: Vec<String> = text(row, "groupby")?
            .split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_owned)
            .collect();
        if groupby.len() > bounds.max_unfold_depth {
            return Err(fail(format!(
                "line {code} groups by more than {} fields (setting {}).",
                bounds.max_unfold_depth,
                settings::MAX_UNFOLD_DEPTH
            )));
        }
        let foldable = match text(row, "foldable")?.as_str() {
            "foldable" => Foldable::OnDemand,
            "always_unfolded" => Foldable::Always,
            "never_unfolded" => Foldable::Never,
            other => return Err(fail(format!("line {code}: foldable '{other}' is unknown."))),
        };
        lines.push(LineDef {
            code,
            name: text(row, "name")?,
            parent,
            children: Vec::new(),
            groupby,
            group: Vec::new(),
            foldable,
            hide_if_zero: flag(row, "hide_if_zero")?,
            optional: flag(row, "optional")?,
            exprs: Vec::new(),
        });
    }
    for index in 0..lines.len() {
        let mut seen = 0;
        let mut at = lines[index].parent;
        while let Some(p) = at {
            seen += 1;
            if p == index || seen > lines.len() {
                return Err(fail(format!(
                    "the parents of line {} form a cycle.",
                    lines[index].code
                )));
            }
            at = lines[p].parent;
        }
        if let Some(p) = lines[index].parent {
            lines[p].children.push(index);
        }
    }

    for row in rows(
        env,
        tx,
        "reporting.report.expression",
        json!([["line_id", "in", ids]]),
        &[
            "line_id",
            "label",
            "engine",
            "model",
            "domain",
            "field",
            "formula",
            "date_field",
            "date_scope",
            "zero_division",
        ],
    )
    .await?
    {
        let line_id = int(&row, "line_id")?;
        let line = ids
            .iter()
            .position(|i| *i == line_id)
            .ok_or_else(|| Error::internal("reporting: an expression of an unread line"))?;
        let label = text(&row, "label")?;
        let code = lines[line].code.clone();
        if !is_label(&label) || lines[line].exprs.iter().any(|e| e.label == label) {
            return Err(fail(format!(
                "line {code}: label '{label}' is not a unique lower-case name."
            )));
        }
        let engine = match text(&row, "engine")?.as_str() {
            "aggregate" => Engine::Aggregate(
                aggregate(env, &row)
                    .map_err(|e| fail(format!("line {code}, {label}: {}", e.message())))?,
            ),
            "formula" => Engine::Formula {
                term: formula::parse(&text(&row, "formula")?)
                    .map_err(|e| fail(format!("line {code}, {label}: {}", e.message())))?,
                zero_empty: text(&row, "zero_division")? == "empty",
            },
            other => {
                return Err(fail(format!(
                    "line {code}, {label}: engine '{other}' is unknown."
                )));
            }
        };
        lines[line].exprs.push(ExprDef { label, engine });
    }

    let groups = lines
        .iter()
        .map(|line| check_line(env, line, &lines, strict))
        .collect::<std::result::Result<Vec<_>, String>>()
        .map_err(&fail)?;
    for (line, group) in lines.iter_mut().zip(groups) {
        line.group = group;
    }
    let order = evaluation_order(&lines).map_err(&fail)?;
    Ok(ReportDef {
        id,
        name,
        provider,
        date_mode,
        date_default,
        comparison: flag(report, "comparison")?,
        multi_company: flag(report, "multi_company")?,
        unfold_all: flag(report, "unfold_all")?,
        hide_zero,
        search_bar: flag(report, "search_bar")?,
        totals_below: flag(report, "totals_below")?,
        load_more_limit,
        columns,
        lines,
        order,
    })
}

fn aggregate(env: &Env, row: &Row) -> Result<Agg> {
    let model = text(row, "model")?;
    let spec = env
        .registry()
        .model(&model)
        .ok_or_else(|| Error::validation(format!("model '{model}' does not exist.")))?;
    let typed = |name: &str, kinds: &[&str], what: &str| -> Result<String> {
        let found = spec
            .field(name)
            .ok_or_else(|| Error::validation(format!("{model} has no field {name}.")))?;
        if !kinds.contains(&found.ty.type_name()) {
            return Err(Error::validation(format!("{model}.{name} is not {what}.")));
        }
        Ok(found.ty.type_name().to_owned())
    };
    let summed = text(row, "field")?;
    let field = if summed.is_empty() {
        None
    } else {
        typed(
            &summed,
            &["integer", "float", "monetary"],
            "a number to sum",
        )?;
        Some(summed)
    };
    let date_field = text(row, "date_field")?;
    let date = if date_field.is_empty() {
        None
    } else {
        let kind = typed(&date_field, &["date", "datetime"], "a date or a datetime")?;
        Some((date_field, kind == "datetime"))
    };
    let domain_text = text(row, "domain")?;
    if domain_text.len() > MAX_DOMAIN_BYTES {
        return Err(Error::validation(format!(
            "the domain is longer than {MAX_DOMAIN_BYTES} bytes."
        )));
    }
    let domain = if domain_text.trim().is_empty() {
        Domain::constant(true)
    } else {
        let json: Json = serde_json::from_str(&domain_text)
            .map_err(|e| Error::validation(format!("the domain is not a JSON domain: {e}")))?;
        Domain::from_json(&json)
            .map_err(|e| Error::validation(format!("the domain is invalid: {e}")))?
    };
    Ok(Agg {
        model,
        domain_text,
        domain,
        field,
        date,
        from_beginning: text(row, "date_scope")? == "from_beginning",
    })
}

fn aggs(line: &LineDef) -> impl Iterator<Item = (&str, &Agg)> {
    line.exprs.iter().filter_map(|e| match &e.engine {
        Engine::Aggregate(agg) => Some((e.label.as_str(), agg)),
        Engine::Formula { .. } => None,
    })
}

fn check_line(
    env: &Env,
    line: &LineDef,
    lines: &[LineDef],
    strict: bool,
) -> std::result::Result<Vec<GroupField>, String> {
    let code = &line.code;
    let mut group = Vec::new();
    if !line.groupby.is_empty() {
        if !line.children.is_empty() {
            return Err(format!(
                "line {code} has child lines and a group-by; it can have one of them."
            ));
        }
        let mut found = aggs(line);
        match found.next() {
            None if strict => {
                return Err(format!(
                    "line {code} groups records but has no aggregate expression."
                ));
            }
            None => {}
            Some((_, first)) => {
                if found.any(|(_, a)| {
                    a.model != first.model
                        || a.domain_text != first.domain_text
                        || a.date != first.date
                        || a.from_beginning != first.from_beginning
                }) {
                    return Err(format!(
                        "the aggregate expressions of grouped line {code} use different models, domains or dates."
                    ));
                }
                let spec = env
                    .registry()
                    .model(&first.model)
                    .ok_or_else(|| format!("model '{}' does not exist.", first.model))?;
                for name in &line.groupby {
                    let ty = &spec
                        .field(name)
                        .ok_or_else(|| {
                            format!(
                                "line {code}: {} has no field {name} to group by.",
                                first.model
                            )
                        })?
                        .ty;
                    let kind = ty.type_name();
                    if !matches!(kind, "many2one" | "char" | "selection" | "integer") {
                        return Err(format!(
                            "line {code}: {name} is a {kind} field; groups are by many2one, char, selection or integer fields."
                        ));
                    }
                    group.push(GroupField {
                        name: name.clone(),
                        comodel: ty.comodel().map(str::to_owned),
                        integer: kind == "integer",
                    });
                }
            }
        }
    }
    for expr in &line.exprs {
        let Engine::Formula { term, .. } = &expr.engine else {
            continue;
        };
        let (mut refs, mut sum_children) = (Vec::new(), false);
        term.refs(&mut refs, &mut sum_children);
        if !line.groupby.is_empty() && (sum_children || refs.iter().any(|r| r.code.is_some())) {
            return Err(format!(
                "line {code}, {}: a formula of a grouped line uses only its own labels.",
                expr.label
            ));
        }
        for r in &refs {
            let target = match &r.code {
                None => Some(line),
                Some(c) => lines.iter().find(|l| &l.code == c),
            };
            let known = target.is_some_and(|t| t.exprs.iter().any(|e| e.label == r.label));
            if strict && !known {
                return Err(format!(
                    "line {code}, {}: {} is not a label of the report.",
                    expr.label,
                    show(r, code)
                ));
            }
        }
    }
    Ok(group)
}

fn show(r: &Ref, own: &str) -> String {
    format!("{}.{}", r.code.as_deref().unwrap_or(own), r.label)
}

/// The formula expressions in an order where each comes after what it reads; a cycle is an error
/// naming its members.
fn evaluation_order(lines: &[LineDef]) -> std::result::Result<Vec<(usize, usize)>, String> {
    let nodes: Vec<(usize, usize)> = lines
        .iter()
        .enumerate()
        .flat_map(|(l, line)| {
            line.exprs
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.engine, Engine::Formula { .. }))
                .map(move |(e, _)| (l, e))
        })
        .collect();
    let index = |l: usize, label: &str| {
        nodes
            .iter()
            .position(|(nl, ne)| *nl == l && lines[l].exprs[*ne].label == label)
    };
    // edges: what each node reads among the formula nodes
    let mut reads: Vec<Vec<usize>> = Vec::with_capacity(nodes.len());
    for (l, e) in &nodes {
        let expr = &lines[*l].exprs[*e];
        let Engine::Formula { term, .. } = &expr.engine else {
            continue;
        };
        let (mut refs, mut sum_children) = (Vec::new(), false);
        term.refs(&mut refs, &mut sum_children);
        let mut deps: Vec<usize> = refs
            .iter()
            .filter_map(|r| match &r.code {
                None => index(*l, &r.label),
                Some(code) => lines
                    .iter()
                    .position(|x| &x.code == code)
                    .and_then(|t| index(t, &r.label)),
            })
            .collect();
        if sum_children {
            deps.extend(
                lines[*l]
                    .children
                    .iter()
                    .filter_map(|c| index(*c, &expr.label)),
            );
        }
        reads.push(deps);
    }
    let mut done = vec![false; nodes.len()];
    let mut order = Vec::with_capacity(nodes.len());
    while order.len() < nodes.len() {
        let ready: Vec<usize> = (0..nodes.len())
            .filter(|n| !done[*n] && reads[*n].iter().all(|d| done[*d]))
            .collect();
        if ready.is_empty() {
            let cycle: Vec<String> = (0..nodes.len())
                .filter(|n| !done[*n])
                .map(|n| {
                    format!(
                        "{}.{}",
                        lines[nodes[n].0].code, lines[nodes[n].0].exprs[nodes[n].1].label
                    )
                })
                .collect();
            return Err(format!(
                "the formulas of {} form a cycle.",
                cycle.join(", ")
            ));
        }
        for n in ready {
            done[n] = true;
            order.push(nodes[n]);
        }
    }
    Ok(order)
}

// ---------------------------------------------------------------------------------------------
// The provider

/// The definition-driven provider.
pub struct DefinitionProvider;
const SPEC: ProviderSpec = ProviderSpec::new(DEFINITION, 1);

/// Values of one row: label -> number (`None`: nothing to show).
type Values = BTreeMap<String, Option<Decimal>>;

/// A group key of one segment.
#[derive(Debug, Clone, PartialEq)]
enum Key {
    Id(Option<i64>),
    Text(Option<String>),
    Int(Option<i64>),
}

impl Key {
    fn domain(&self) -> Json {
        match self {
            Key::Id(Some(n)) | Key::Int(Some(n)) => json!(n),
            Key::Text(Some(s)) => json!(s),
            Key::Id(None) | Key::Int(None) | Key::Text(None) => json!(false),
        }
    }
    fn segment(&self) -> String {
        match self {
            Key::Id(Some(n)) | Key::Int(Some(n)) => n.to_string(),
            Key::Text(Some(s)) => s.clone(),
            Key::Id(None) | Key::Int(None) | Key::Text(None) => String::new(),
        }
    }
    fn of(value: &GroupValue, relational: bool, integer: bool) -> Result<Self> {
        Ok(match value {
            GroupValue::Records(_) => Key::Id(value.record_id().map(|r| r.get())),
            GroupValue::False | GroupValue::Null if relational => Key::Id(None),
            GroupValue::False | GroupValue::Null if integer => Key::Int(None),
            GroupValue::False | GroupValue::Null => Key::Text(None),
            GroupValue::Int(n) => Key::Int(Some(*n)),
            GroupValue::Str(s) => Key::Text(Some(s.to_string())),
            other => {
                return Err(Error::internal(format!(
                    "reporting: a group key reads as {other:?}"
                )));
            }
        })
    }
}

/// Where a line id points in the definition.
struct Located {
    line: usize,
    keys: Vec<(String, Key)>,
}

/// Check `id` against the definition: definition lines along their parents, then group segments
/// along the line's group-by chain with the field's comodel, at most `reporting.max_unfold_depth`.
fn locate(def: &ReportDef, id: &LineId, bounds: &Bounds) -> Result<Located> {
    let refuse = |index: usize, what: &str| {
        Error::validation(format!("Line id segment {} of '{id}' {what}.", index + 1))
    };
    if id.report() != def.id {
        return Err(refuse(0, "is not the root of this report"));
    }
    let mut line: Option<usize> = None;
    let mut keys = Vec::new();
    for (index, segment) in id.segments().iter().enumerate().skip(1) {
        match segment.kind() {
            SegmentKind::Line(code) if keys.is_empty() => {
                let found = def
                    .lines
                    .iter()
                    .position(|l| &l.code == code && l.parent == line)
                    .ok_or_else(|| refuse(index, "is not a line here"))?;
                if !segment.model().is_empty() || !segment.value().is_empty() {
                    return Err(refuse(index, "has a model or a value"));
                }
                line = Some(found);
            }
            SegmentKind::Group(field) => {
                let at = line.ok_or_else(|| refuse(index, "has no line above it"))?;
                if keys.len() >= bounds.max_unfold_depth {
                    return Err(refuse(
                        index,
                        &format!(
                            "is deeper than {} (setting {})",
                            bounds.max_unfold_depth,
                            settings::MAX_UNFOLD_DEPTH
                        ),
                    ));
                }
                let group = def.lines[at]
                    .group
                    .get(keys.len())
                    .filter(|g| &g.name == field)
                    .ok_or_else(|| refuse(index, "groups by a field not in the line's group-by"))?;
                if segment.model() != group.comodel.as_deref().unwrap_or("") {
                    return Err(refuse(
                        index,
                        "names a model that is not the field's comodel",
                    ));
                }
                let value = segment.value();
                let key =
                    if group.comodel.is_some() {
                        Key::Id(match value {
                            "" => None,
                            v => Some(v.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                                refuse(index, "has a value that is not a record id")
                            })?),
                        })
                    } else if group.integer {
                        Key::Int(match value {
                            "" => None,
                            v => Some(v.parse().map_err(|_| {
                                refuse(index, "has a value that is not a whole number")
                            })?),
                        })
                    } else {
                        Key::Text((!value.is_empty()).then(|| value.to_owned()))
                    };
                keys.push((field.clone(), key));
            }
            _ => return Err(refuse(index, "cannot be unfolded or drilled")),
        }
    }
    Ok(Located {
        line: line.ok_or_else(|| refuse(0, "names no line"))?,
        keys,
    })
}

fn bad(message: impl Into<String>) -> Error {
    Error::validation(message.into())
}

fn spec(agg: &Agg) -> String {
    match &agg.field {
        Some(field) => format!("{field}:sum"),
        None => "__count".to_owned(),
    }
}

fn numeric(value: Option<&GroupValue>) -> Result<Decimal> {
    match value {
        // a sum over no record is zero (PostgreSQL's NULL), as the dashboard reads it
        None | Some(GroupValue::Null | GroupValue::False) => Ok(Decimal::ZERO),
        Some(GroupValue::Int(n)) => Ok(Decimal::from(*n)),
        Some(GroupValue::Decimal(n)) => Ok(*n),
        Some(GroupValue::Float(n)) => Decimal::try_from(*n)
            .map_err(|_| Error::validation("A report sum is not a finite number.")),
        Some(other) => Err(Error::internal(format!(
            "reporting: an aggregate reads as {other:?}"
        ))),
    }
}

fn next_day(date: Date) -> Result<Date> {
    date.days()
        .checked_add(1)
        .map(Date::from_days)
        .ok_or_else(|| bad("A report date is out of the calendar."))
}

/// The expression's domain, its period and the selected companies.
fn scope(env: &Env, agg: &Agg, options: &Options, group: usize) -> Result<Domain> {
    let period: &Period = options
        .column_groups
        .get(group)
        .ok_or_else(|| Error::internal("reporting: a column group out of range"))?;
    let mut items = Vec::new();
    if let Some((field, datetime)) = &agg.date {
        let day = |d: Date| crate::options::text(d);
        let start = |d: Date| {
            if *datetime {
                format!("{} 00:00:00", day(d))
            } else {
                day(d)
            }
        };
        items.push(if *datetime {
            json!([field, "<", start(next_day(period.to)?)])
        } else {
            json!([field, "<=", day(period.to)])
        });
        if let (Some(from), false) = (period.from, agg.from_beginning) {
            items.push(json!([field, ">=", start(from)]));
        }
    }
    let company = env
        .registry()
        .model(&agg.model)
        .and_then(|m| m.field("company_id"))
        .is_some_and(|f| f.ty.comodel() == Some("res.company"));
    if company {
        // shared records (no company) count too, as the multi-company record rules read them
        let mut companies: Vec<Json> = options.companies.iter().map(|c| json!(c)).collect();
        companies.push(json!(false));
        items.push(json!(["company_id", "in", companies]));
    }
    Ok(agg.domain.clone() & Domain::from_json(&Json::Array(items))?)
}

fn keys_domain(keys: &[(String, Key)]) -> Result<Domain> {
    let items: Vec<Json> = keys
        .iter()
        .map(|(f, k)| json!([f, "=", k.domain()]))
        .collect();
    Ok(Domain::from_json(&Json::Array(items))?)
}

fn in_domain(field: &str, keys: &[Key]) -> Result<Domain> {
    let values: Vec<Json> = keys.iter().map(Key::domain).collect();
    Ok(Domain::from_json(&json!([[field, "in", values]]))?)
}

/// Evaluate the formulas of `only` (every line when `None`) into `values` (line -> label -> value).
fn formulas(
    def: &ReportDef,
    values: &mut BTreeMap<usize, Values>,
    only: Option<usize>,
) -> Result<()> {
    for (l, e) in &def.order {
        if only.is_some_and(|o| o != *l) || !values.contains_key(l) {
            continue;
        }
        let line = &def.lines[*l];
        let expr = &line.exprs[*e];
        let Engine::Formula { term, zero_empty } = &expr.engine else {
            continue;
        };
        let lookup = |r: &Ref| {
            let target = match &r.code {
                None => Some(*l),
                Some(code) => def.lines.iter().position(|x| &x.code == code),
            };
            target
                .and_then(|t| values.get(&t))
                .and_then(|v| v.get(&r.label))
                .copied()
                .flatten()
        };
        let children = line
            .children
            .iter()
            .filter_map(|c| {
                values
                    .get(c)
                    .and_then(|v| v.get(&expr.label))
                    .copied()
                    .flatten()
            })
            .try_fold(None::<Decimal>, |sum, n| {
                sum.unwrap_or(Decimal::ZERO).checked_add(n).map(Some)
            });
        let value = match (children, term.eval(&lookup, children.flatten())) {
            (None, _) | (_, Err(EvalError::Overflow)) => {
                return Err(bad(format!(
                    "Line {}, {}: the result is too large.",
                    line.code, expr.label
                )));
            }
            (_, Err(EvalError::DivisionByZero)) if *zero_empty => None,
            (_, Err(EvalError::DivisionByZero)) => {
                return Err(bad(format!(
                    "Line {}, {}: division by zero.",
                    line.code, expr.label
                )));
            }
            (_, Ok(value)) => value,
        };
        values
            .entry(*l)
            .or_default()
            .insert(expr.label.clone(), value);
    }
    Ok(())
}

/// The company currency's decimal places and id when a column is monetary.
async fn money(
    cx: &mut ReportCx<'_>,
    def: &ReportDef,
    options: &Options,
) -> Result<(u32, Option<i64>)> {
    if !def.columns.iter().any(|c| c.kind == CellKind::Monetary) {
        return Ok((0, None));
    }
    let company = *options
        .companies
        .first()
        .ok_or_else(|| bad("A report with amounts needs a company."))?;
    let (env, tx) = cx.parts();
    let read = rows(
        env,
        tx,
        "res.company",
        json!([["id", "=", company]]),
        &["currency_id"],
    )
    .await?;
    let currency = read
        .first()
        .map(|r| int(r, "currency_id"))
        .transpose()?
        .filter(|c| *c > 0)
        .ok_or_else(|| bad(format!("Company {company} has no currency.")))?;
    let read = rows(
        env,
        tx,
        "res.currency",
        json!([["id", "=", currency]]),
        &["decimal_places"],
    )
    .await?;
    let places = read
        .first()
        .map(|r| int(r, "decimal_places"))
        .transpose()?
        .and_then(|p| u32::try_from(p).ok())
        .ok_or_else(|| bad(format!("Currency {currency} has no decimal places.")))?;
    Ok((places, Some(currency)))
}

/// The cells of a line with `values` per column group.
fn cells(def: &ReportDef, id: &LineId, values: &[&Values], money: (u32, Option<i64>)) -> Vec<Cell> {
    let mut out = Vec::with_capacity(values.len() * def.columns.len());
    for group in values {
        for column in &def.columns {
            let shown = column.rollup_field.as_ref().is_none_or(|f| id.has_group(f));
            let value = if shown {
                group.get(&column.label).copied().flatten()
            } else {
                None
            };
            let cell = match column.kind {
                CellKind::Monetary => Cell::monetary(value, money.0, money.1),
                kind => Cell::number(kind, value, column.scale),
            };
            out.push(if column.blank_if_zero {
                cell.blank_if_zero()
            } else {
                cell
            });
        }
    }
    out
}

/// The values of every definition line in every column group, and the optional lines the caller
/// may not read.
async fn top_values(
    cx: &mut ReportCx<'_>,
    def: &ReportDef,
    options: &Options,
) -> Result<(Vec<BTreeMap<usize, Values>>, Vec<usize>)> {
    let mut per_group = vec![BTreeMap::new(); options.column_groups.len()];
    let mut hidden = Vec::new();
    let (env, tx) = cx.parts();
    'line: for (l, line) in def.lines.iter().enumerate() {
        for (g, values) in per_group.iter_mut().enumerate() {
            let mut row = Values::new();
            for (label, agg) in aggs(line) {
                let args = ReadGroup::new(scope(env, agg, options, g)?).aggregates([spec(agg)]);
                match env.records(&agg.model)?._read_group(tx, args).await {
                    Ok(groups) => {
                        row.insert(label.to_owned(), Some(numeric(groups.column(0).first())?));
                    }
                    // an optional line the caller may not read is left out and named (design §8)
                    Err(e) if e.kind() == ErrorKind::Access && line.optional => {
                        hidden.push(l);
                        continue 'line;
                    }
                    Err(e) => return Err(e),
                }
            }
            values.insert(l, row);
        }
    }
    for values in &mut per_group {
        for l in &hidden {
            values.remove(l);
        }
        formulas(def, values, None)?;
    }
    Ok((per_group, hidden))
}

/// The definition lines under `parent` (top level for `None`) with their always-unfolded
/// descendants.
fn definition_lines(
    def: &ReportDef,
    parent_id: &LineId,
    parent: Option<usize>,
    values: &[BTreeMap<usize, Values>],
    money: (u32, Option<i64>),
    out: &mut Vec<Line>,
) {
    let empty = Values::new();
    let line_cells = |l: usize, id: &LineId| {
        let per: Vec<&Values> = values.iter().map(|g| g.get(&l).unwrap_or(&empty)).collect();
        cells(def, id, &per, money)
    };
    fn zero(def: &ReportDef, l: usize, cells_of: &dyn Fn(usize) -> bool) -> bool {
        cells_of(l)
            && def.lines[l]
                .children
                .iter()
                .all(|c| zero(def, *c, cells_of))
    }
    for (l, line) in def
        .lines
        .iter()
        .enumerate()
        .filter(|(_, x)| x.parent == parent)
    {
        // a hidden optional line has no values in any group
        if values.first().is_some_and(|g| !g.contains_key(&l)) {
            continue;
        }
        let id = parent_id.child(Segment::line(&line.code));
        let all_zero = |x: usize| line_cells(x, &id).iter().all(Cell::is_zero);
        if line.hide_if_zero && zero(def, l, &all_zero) {
            continue;
        }
        let mut shown = Line::new(
            id.clone(),
            line.name.clone(),
            LineKind::Section,
            line_cells(l, &id),
        );
        let has_children = !line.children.is_empty();
        match line.foldable {
            Foldable::Always if has_children => {
                shown.unfolded = true;
                out.push(shown);
                definition_lines(def, &id, Some(l), values, money, out);
            }
            Foldable::Never => out.push(shown),
            Foldable::Always | Foldable::OnDemand => {
                shown.unfoldable = has_children || !line.group.is_empty();
                out.push(shown);
            }
        }
    }
}

async fn top_lines(cx: &mut ReportCx<'_>, def: &ReportDef, options: &Options) -> Result<Expansion> {
    let (values, hidden) = top_values(cx, def, options).await?;
    let money = money(cx, def, options).await?;
    let mut lines = Vec::new();
    definition_lines(def, &LineId::root(def.id), None, &values, money, &mut lines);
    let mut expansion = Expansion::new(lines);
    expansion.hidden = hidden.iter().map(|l| def.lines[*l].name.clone()).collect();
    Ok(expansion)
}

/// One row per key of `field`: label -> value, in `groups` (key column first).
fn rows_by_key(
    groups: &Groups,
    labels: &[&str],
    group: &GroupField,
) -> Result<BTreeMap<String, Values>> {
    let mut out = BTreeMap::new();
    for row in groups.rows() {
        let first = row
            .first()
            .ok_or_else(|| Error::internal("reporting: a group row without its key"))?;
        let key = Key::of(first, group.comodel.is_some(), group.integer)?;
        let mut values = Values::new();
        for (i, label) in labels.iter().enumerate() {
            values.insert((*label).to_owned(), Some(numeric(row.get(i + 1))?));
        }
        out.insert(format!("{key:?}"), values);
    }
    Ok(out)
}

async fn expand(
    cx: &mut ReportCx<'_>,
    def: &ReportDef,
    options: &Options,
    id: &LineId,
    page: ExpandPage,
) -> Result<Expansion> {
    let bounds = *cx.bounds();
    let located = locate(def, id, &bounds)?;
    let line = &def.lines[located.line];
    let depth = located.keys.len();
    if depth == 0 && line.group.is_empty() {
        if line.children.is_empty() || line.foldable == Foldable::Never {
            return Err(bad(format!("Line {id} cannot be unfolded.")));
        }
        let (values, _) = top_values(cx, def, options).await?;
        let money = money(cx, def, options).await?;
        let mut lines = Vec::new();
        definition_lines(def, id, Some(located.line), &values, money, &mut lines);
        return Ok(Expansion::new(lines));
    }
    let group = line
        .group
        .get(depth)
        .ok_or_else(|| bad(format!("Line {id} cannot be unfolded further.")))?;
    let end = page
        .offset
        .checked_add(page.limit)
        .filter(|end| usize::try_from(*end).is_ok_and(|e| e <= bounds.max_screen_lines))
        .ok_or_else(|| {
            bad(format!(
                "Loading past {} children of one line is refused (setting {}).",
                bounds.max_screen_lines,
                settings::MAX_SCREEN_LINES
            ))
        })?;
    let found: Vec<(&str, &Agg)> = aggs(line).collect();
    let (_, first) = *found
        .first()
        .ok_or_else(|| bad(format!("Line {id} groups no records.")))?;
    let labels: Vec<&str> = found.iter().map(|(l, _)| *l).collect();
    let specs: Vec<String> = found.iter().map(|(_, a)| spec(a)).collect();
    let money = money(cx, def, options).await?;
    let (env, tx) = cx.parts();
    let parent_keys = keys_domain(&located.keys)?;
    let domains = (0..options.column_groups.len())
        .map(|g| Ok(scope(env, first, options, g)? & parent_keys.clone()))
        .collect::<Result<Vec<Domain>>>()?;
    let union = match domains.as_slice() {
        [one] => one.clone(),
        many => Domain::or(many.iter().cloned()),
    };
    let records = env.records(&first.model)?;
    let mut paging = ReadGroup::new(union)
        .groupby([group.name.clone()])
        .aggregates([specs[0].clone()])
        .limit(end)
        .count_groups(true);
    // shortcut: with comparison periods the order is by the column summed over all periods;
    // per-period order needs the one-statement conditional sums of the milestone-end phase.
    if let Some(order) = &options.order
        && let Some(i) = labels.iter().position(|l| *l == order.column)
    {
        let direction = if order.descending { "desc" } else { "asc" };
        paging = paging
            .aggregates(specs.clone())
            .order(format!("{} {direction}, {}", specs[i], group.name));
    }
    let paged = records._read_group(tx, paging).await?;
    let total = paged
        .group_count()
        .ok_or_else(|| Error::internal("reporting: the paging read has no group count"))?;
    let loaded: Vec<Key> = paged
        .column(0)
        .iter()
        .map(|v| Key::of(v, group.comodel.is_some(), group.integer))
        .collect::<Result<_>>()?;
    let offset = usize::try_from(page.offset).map_err(|_| bad("The page offset is too large."))?;
    let keys: &[Key] = loaded.get(offset..).unwrap_or(&[]);

    let mut per_group: Vec<BTreeMap<String, Values>> = Vec::with_capacity(domains.len());
    for domain in &domains {
        let args = ReadGroup::new(domain.clone() & in_domain(&group.name, keys)?)
            .groupby([group.name.clone()])
            .aggregates(specs.clone());
        per_group.push(rows_by_key(
            &records._read_group(tx, args).await?,
            &labels,
            group,
        )?);
    }
    let mut names = BTreeMap::new();
    if let Some(comodel) = &group.comodel {
        let ids: Vec<i64> = keys
            .iter()
            .filter_map(|k| match k {
                Key::Id(Some(n)) => Some(*n),
                _ => None,
            })
            .collect();
        let shown = env
            .records(comodel)?
            .browse(ids.iter().copied())?
            .values(tx, "display_name")
            .await?;
        for (n, value) in ids.iter().zip(shown) {
            let Value::Str(name) = value else {
                return Err(Error::internal(format!(
                    "reporting: {comodel} {n} has no display name"
                )));
            };
            names.insert(*n, name.to_string());
        }
    }
    let last = depth + 1 >= line.group.len();
    let mut lines = Vec::with_capacity(keys.len() + 1);
    for key in keys {
        let mut values: Vec<Values> = per_group
            .iter()
            .map(|g| {
                g.get(&format!("{key:?}"))
                    .cloned()
                    .unwrap_or_else(|| zeros(&labels))
            })
            .collect();
        for v in &mut values {
            same_line(def, located.line, v)?;
        }
        let child = id.child(Segment::group(
            &group.name,
            group.comodel.as_deref().unwrap_or(""),
            &key.segment(),
        ));
        let name = match key {
            Key::Id(Some(n)) => names
                .get(n)
                .cloned()
                .ok_or_else(|| Error::internal(format!("reporting: no display name for {n}")))?,
            Key::Text(Some(s)) => s.clone(),
            Key::Int(Some(n)) => n.to_string(),
            Key::Id(None) | Key::Text(None) | Key::Int(None) => "None".to_owned(),
        };
        let refs: Vec<&Values> = values.iter().collect();
        let mut shown = Line::new(
            child.clone(),
            name,
            if last {
                LineKind::Detail
            } else {
                LineKind::Group
            },
            cells(def, &child, &refs, money),
        );
        shown.unfoldable = !last;
        if matches!(key, Key::Id(Some(_))) {
            shown.caret = vec!["open"];
        }
        lines.push(shown);
    }
    if total > end && !keys.is_empty() {
        let mut values = Vec::with_capacity(domains.len());
        for domain in &domains {
            let whole = records
                ._read_group(tx, ReadGroup::new(domain.clone()).aggregates(specs.clone()))
                .await?;
            let seen = ReadGroup::new(domain.clone() & in_domain(&group.name, &loaded)?)
                .aggregates(specs.clone());
            let seen = records._read_group(tx, seen).await?;
            let mut rest = Values::new();
            for (i, label) in labels.iter().enumerate() {
                let left = numeric(whole.column(i).first())?
                    .checked_sub(numeric(seen.column(i).first())?)
                    .ok_or_else(|| bad("A remaining sum is too large."))?;
                rest.insert((*label).to_owned(), Some(left));
            }
            same_line(def, located.line, &mut rest)?;
            values.push(rest);
        }
        let more_id = id.child(Segment::more());
        let refs: Vec<&Values> = values.iter().collect();
        let mut more = Line::new(
            more_id.clone(),
            "",
            LineKind::More,
            cells(def, &more_id, &refs, money),
        );
        more.remaining = Some(total - end);
        more.next_offset = Some(end);
        lines.push(more);
    }
    Ok(Expansion::new(lines))
}

fn zeros(labels: &[&str]) -> Values {
    labels
        .iter()
        .map(|l| ((*l).to_owned(), Some(Decimal::ZERO)))
        .collect()
}

/// The same-line formulas of `line` over one row of `values`.
fn same_line(def: &ReportDef, line: usize, values: &mut Values) -> Result<()> {
    let mut one = BTreeMap::from([(line, std::mem::take(values))]);
    formulas(def, &mut one, Some(line))?;
    *values = one
        .remove(&line)
        .ok_or_else(|| Error::internal("reporting: a formula row disappeared"))?;
    Ok(())
}

async fn drill(
    cx: &mut ReportCx<'_>,
    def: &ReportDef,
    options: &Options,
    id: &LineId,
    target: &DrillTarget,
) -> Result<Json> {
    let bounds = *cx.bounds();
    let located = locate(def, id, &bounds)?;
    let line = &def.lines[located.line];
    let (env, tx) = cx.parts();
    match target {
        DrillTarget::Item(item) if item == "open" => {
            let comodel = located
                .keys
                .len()
                .checked_sub(1)
                .and_then(|i| line.group[i].comodel.clone())
                .ok_or_else(|| bad(format!("Line {id} is not a record to open.")))?;
            let Some((_, Key::Id(Some(record)))) = located.keys.last() else {
                return Err(bad(format!("Line {id} is not a record to open.")));
            };
            let name = env
                .records(&comodel)?
                .browse([*record])?
                .values(tx, "display_name")
                .await?;
            let Some(Value::Str(name)) = name.first() else {
                return Err(bad(format!("Record {record} of {comodel} does not exist.")));
            };
            Ok(
                json!({"type": "ir.actions.act_window", "name": name.to_string(), "res_model": comodel,
                      "res_id": record, "target": "new", "views": [[false, "form"]]}),
            )
        }
        DrillTarget::Item(item) => Err(bad(format!("Line {id} has no caret item '{item}'."))),
        DrillTarget::Cell(index) => {
            let width = def.columns.len();
            let (group, column) = (
                index.checked_div(width).unwrap_or(usize::MAX),
                index.checked_rem(width).unwrap_or(0),
            );
            if group >= options.column_groups.len() {
                return Err(bad(format!("The report has no column {index}.")));
            }
            let label = &def.columns[column].label;
            let expr = line
                .exprs
                .iter()
                .find(|e| &e.label == label)
                .ok_or_else(|| {
                    bad(format!(
                        "Line {} has no value in column {label}.",
                        line.code
                    ))
                })?;
            let Engine::Aggregate(agg) = &expr.engine else {
                return Err(bad(format!(
                    "The {label} cell of line {} is a formula; no records are behind it.",
                    line.code
                )));
            };
            let domain = scope(env, agg, options, group)? & keys_domain(&located.keys)?;
            Ok(
                json!({"type": "ir.actions.act_window", "name": line.name, "res_model": agg.model,
                      "target": "current", "views": [[false, "list"], [false, "form"]],
                      "domain": domain.to_json()?, "context": {}}),
            )
        }
    }
}

impl ReportProvider for DefinitionProvider {
    fn spec(&self) -> &ProviderSpec {
        &SPEC
    }
    fn init_options<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        options: &'a mut Options,
        _strict: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match options.provider.keys().next() {
                Some(key) => Err(bad(format!(
                    "options.provider.{key} is not an option of provider {DEFINITION}."
                ))),
                None => Ok(()),
            }
        })
    }
    fn lines<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(top_lines(cx, report, options))
    }
    fn expand<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(expand(cx, report, options, line, page))
    }
    fn drill<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(drill(cx, report, options, line, target))
    }
}

#[cfg(test)]
#[path = "tests/definition.rs"]
mod tests;
