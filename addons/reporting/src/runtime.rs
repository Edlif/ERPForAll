// SPDX-License-Identifier: MIT OR Apache-2.0
//! The calls behind the RPC methods of `reporting.report` (design §9), each in the caller's read
//! transaction: resolve the options, ask the provider for lines, unfold what the options ask for
//! (one page per line), add the totals below unfolded lines, and bound the answer.
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};

use crate::definition::{self, ReportDef};
use crate::line::{DrillTarget, ExpandPage, Line, LineId, LineKind, Segment};
use crate::options::{self, Options};
use crate::provider::{self, ReportCx, ReportProvider};
use crate::settings::{self, Bounds};

pub(crate) struct Prepared {
    pub(crate) def: ReportDef,
    pub(crate) options: Options,
    pub(crate) bounds: Bounds,
    provider: &'static dyn ReportProvider,
}

pub(crate) async fn prepare(
    env: &Env,
    tx: &mut Transaction,
    report: i64,
    input: &Json,
    strict: bool,
) -> Result<Prepared> {
    let bounds = settings::read(env, tx).await?;
    let def = definition::load(env, tx, report, true, &bounds).await?;
    let provider = provider::find(env, &def.provider)?;
    let mut options = options::resolve(env, tx, &def, input, strict, &bounds).await?;
    let mut cx = ReportCx::new(env.clone(), tx, bounds);
    provider
        .init_options(&mut cx, &def, &mut options, strict)
        .await?;
    Ok(Prepared {
        def,
        options,
        bounds,
        provider,
    })
}

fn report_of(input: &Json) -> Result<i64> {
    input
        .get("report")
        .and_then(Json::as_i64)
        .ok_or_else(|| Error::validation("options.report is required."))
}

/// `erp_report_options`: the options for `report`, carrying `previous` over, and the filter bar.
///
/// # Errors
/// As [`options::resolve`] (not strict) and [`definition::load`].
pub async fn report_options(
    env: &Env,
    tx: &mut Transaction,
    report: i64,
    previous: &Json,
) -> Result<Json> {
    let p = prepare(env, tx, report, previous, false).await?;
    Ok(
        json!({"name": p.def.name, "options": p.options.to_json(&p.def), "filters": p.def.filters()}),
    )
}

/// Unfold, in place, every line of `lines` the options ask for, then add the totals.
async fn unfold(
    cx: &mut ReportCx<'_>,
    p: &Prepared,
    mut lines: Vec<Line>,
    (limit, setting): (usize, &str),
) -> Result<Vec<Line>> {
    let too_many = || {
        Error::validation(format!(
            "The report has more than {limit} lines; fold lines or narrow the options (setting {setting})."
        ))
    };
    let mut i = 0;
    while i < lines.len() {
        if lines.len() > limit {
            return Err(too_many());
        }
        let line = &lines[i];
        if line.unfoldable
            && !line.unfolded
            && (p.options.unfold_all || p.options.unfolded.contains(&line.id))
        {
            let page = ExpandPage {
                offset: 0,
                limit: p.def.load_more_limit,
            };
            let children = p
                .provider
                .expand(cx, &p.def, &p.options, &line.id, page)
                .await?
                .lines;
            lines[i].unfolded = true;
            lines.splice(i + 1..i + 1, children);
        }
        i += 1;
    }
    if p.def.totals_below() {
        let mut j = lines.len();
        while j > 0 {
            j -= 1;
            if !lines[j].unfolded {
                continue;
            }
            let level = lines[j].id.level();
            let end = (j + 1..lines.len())
                .find(|k| lines[*k].id.level() <= level)
                .unwrap_or(lines.len());
            let total = Line::new(
                lines[j].id.child(Segment::total()),
                lines[j].name.clone(),
                LineKind::Total,
                lines[j].cells.clone(),
            );
            lines.insert(end, total);
        }
    }
    if lines.len() > limit {
        return Err(too_many());
    }
    Ok(lines)
}

fn screen(p: &Prepared) -> (usize, &'static str) {
    (p.bounds.max_screen_lines, settings::MAX_SCREEN_LINES)
}

/// The lines of an export: those of [`report_lines`], bounded by the export's own row setting.
pub(crate) async fn export_lines(
    env: &Env,
    tx: &mut Transaction,
    input: &Json,
) -> Result<(Prepared, Vec<Line>, Vec<String>)> {
    let p = prepare(env, tx, report_of(input)?, input, true).await?;
    let mut cx = ReportCx::new(env.clone(), tx, p.bounds);
    let top = p.provider.lines(&mut cx, &p.def, &p.options).await?;
    let bound = (p.bounds.export_max_rows, settings::EXPORT_MAX_ROWS);
    let hidden = top.hidden;
    let lines = unfold(&mut cx, &p, top.lines, bound).await?;
    Ok((p, lines, hidden))
}

fn answer(lines: &[Line]) -> Vec<Json> {
    lines.iter().map(Line::to_json).collect()
}

/// `erp_report_lines`: the lines for `input` (strict options).
///
/// # Errors
/// As [`options::resolve`] (strict); the provider's errors; `Validation` naming
/// `reporting.max_screen_lines` when the answer would hold more lines (never a cut tree).
pub async fn report_lines(env: &Env, tx: &mut Transaction, input: &Json) -> Result<Json> {
    let p = prepare(env, tx, report_of(input)?, input, true).await?;
    let mut cx = ReportCx::new(env.clone(), tx, p.bounds);
    let top = p.provider.lines(&mut cx, &p.def, &p.options).await?;
    let lines = unfold(&mut cx, &p, top.lines, screen(&p)).await?;
    Ok(
        json!({"options": p.options.to_json(&p.def), "lines": answer(&lines),
              "computed_at": DateTime::now().to_string(), "hidden_lines": top.hidden}),
    )
}

/// `erp_report_expand`: the children of `line` from `offset`, one page of the report's
/// `load_more_limit`, with the unfolded lines below them.
///
/// # Errors
/// `Validation` for a malformed or foreign line id or a negative offset; as [`report_lines`].
pub async fn report_expand(
    env: &Env,
    tx: &mut Transaction,
    input: &Json,
    line: &str,
    offset: i64,
) -> Result<Json> {
    let line = LineId::parse(line)?;
    let offset =
        u64::try_from(offset).map_err(|_| Error::validation("The page offset is negative."))?;
    let p = prepare(env, tx, report_of(input)?, input, true).await?;
    let mut cx = ReportCx::new(env.clone(), tx, p.bounds);
    let page = ExpandPage {
        offset,
        limit: p.def.load_more_limit,
    };
    let children = p
        .provider
        .expand(&mut cx, &p.def, &p.options, &line, page)
        .await?
        .lines;
    let lines = unfold(&mut cx, &p, children, screen(&p)).await?;
    Ok(json!({"lines": answer(&lines), "computed_at": DateTime::now().to_string()}))
}

/// `erp_report_drill`: the action behind a caret item (`{"item": "open"}`) or a cell
/// (`{"cell": <index in options.columns>}`) of `line`.
///
/// # Errors
/// As [`report_expand`]; the provider's refusal of the target.
pub async fn report_drill(
    env: &Env,
    tx: &mut Transaction,
    input: &Json,
    line: &str,
    target: &Json,
) -> Result<Json> {
    let line = LineId::parse(line)?;
    let target = DrillTarget::parse(target)?;
    let p = prepare(env, tx, report_of(input)?, input, true).await?;
    let mut cx = ReportCx::new(env.clone(), tx, p.bounds);
    p.provider
        .drill(&mut cx, &p.def, &p.options, &line, &target)
        .await
}

/// Check the definition of `reports` as saved (constraint of the four definition models).
pub(crate) async fn check_saved(env: &Env, tx: &mut Transaction, reports: &[i64]) -> Result<()> {
    let bounds = settings::read(env, tx).await?;
    for report in reports {
        definition::load(env, tx, *report, false, &bounds).await?;
    }
    Ok(())
}
