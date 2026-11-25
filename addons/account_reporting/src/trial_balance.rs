// SPDX-License-Identifier: MIT OR Apache-2.0
//! Trial Balance dynamic report provider (`account_reporting.trial_balance`):
//! flat per-account ledger ordered by account code (`code name`), supporting period comparisons,
//! exact-decimal Initial Balance / Debit / Credit / End Balance columns, caret actions
//! (`journal_items`, `open`), per-cell drill-down, and grand Total row — with no wrapper line.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_account::ledger::code::codes_by_account;
use erp_addon_reporting::{
    definition::ReportDef,
    line::{
        Cell as ReportCell, DrillTarget, ExpandPage, Expansion, Line, LineId, LineKind, Segment,
        SegmentKind,
    },
    options::{Options, Period},
    provider::{ProviderSpec, ReportCx, ReportProvider},
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, Load};

pub(crate) struct TrialBalance;
const SPEC: ProviderSpec = ProviderSpec::new("account_reporting.trial_balance", 1);

fn invalid(msg: impl Into<String>) -> Error {
    Error::validation(msg.into())
}

fn day(date: Date) -> String {
    let (y, m, d) = date.ymd();
    format!("{y:04}-{m:02}-{d:02}")
}

fn cond(value: Json) -> Result<Domain> {
    Ok(Domain::from_json(&value)?)
}

fn num(value: Option<&GroupValue>) -> Result<Decimal> {
    match value {
        None | Some(GroupValue::Null | GroupValue::False) => Ok(Decimal::ZERO),
        Some(GroupValue::Decimal(n)) => Ok(*n),
        Some(GroupValue::Int(n)) => Ok(Decimal::from(*n)),
        Some(other) => Err(Error::internal(format!(
            "Trial Balance amount is not exact decimal: {other:?}"
        ))),
    }
}

fn get(row: &Value, field: &str) -> Result<Value> {
    row.dict()
        .ok_or_else(|| Error::internal("Trial Balance row is not a batch handle."))?
        .get(field)
}

fn int_field(row: &Value, field: &str) -> Result<i64> {
    match get(row, field)? {
        Value::Int(n) => Ok(n),
        other => Err(Error::internal(format!(
            "Trial Balance {field} is not an integer: {other:?}"
        ))),
    }
}

fn opt_int_field(row: &Value, field: &str) -> Result<Option<i64>> {
    match get(row, field)? {
        Value::Int(n) if n > 0 => Ok(Some(n)),
        Value::False | Value::Int(_) => Ok(None),
        other => Err(Error::internal(format!(
            "Trial Balance {field} is not an optional id: {other:?}"
        ))),
    }
}

fn text_field(row: &Value, field: &str) -> Result<String> {
    match get(row, field)? {
        Value::Str(s) => Ok(s.to_string()),
        Value::False => Ok(String::new()),
        other => Err(Error::internal(format!(
            "Trial Balance {field} is not text: {other:?}"
        ))),
    }
}

fn base_domain(company: i64) -> Result<Domain> {
    cond(json!([
        ["company_id", "=", company],
        ["parent_state", "=", "posted"],
        ["account_id", "!=", false],
        ["journal_id.journal_group_id", "=", false]
    ]))
}

struct CurrencyCtx {
    currency: i64,
    scale: u32,
    root_company: i64,
}

async fn load_currency_ctx(cx: &mut ReportCx<'_>, company: i64) -> Result<CurrencyCtx> {
    let env = cx.env().clone();
    let comp_rows = env
        .records("res.company")?
        .browse([company])?
        .read_rows(cx.tx(), &["currency_id", "root_id"], Load::Raw)
        .await?;
    let comp = comp_rows
        .iter()
        .next()
        .map(Value::Row)
        .ok_or_else(|| invalid("The selected Trial Balance company is not visible."))?;
    let currency = int_field(&comp, "currency_id")?;
    let root_company = opt_int_field(&comp, "root_id")?.unwrap_or(company);
    let cur_rows = env
        .records("res.currency")?
        .browse([currency])?
        .read_rows(cx.tx(), &["decimal_places"], Load::Raw)
        .await?;
    let cur = cur_rows
        .iter()
        .next()
        .map(Value::Row)
        .ok_or_else(|| invalid("The Trial Balance currency is not visible."))?;
    let scale = u32::try_from(int_field(&cur, "decimal_places")?)
        .map_err(|_| invalid("The Trial Balance currency precision is invalid."))?;
    Ok(CurrencyCtx {
        currency,
        scale,
        root_company,
    })
}

#[derive(Default, Clone, Copy)]
struct PeriodAmounts {
    opening: Decimal,
    debit: Decimal,
    credit: Decimal,
    closing: Decimal,
}

fn period_cells(
    report: &ReportDef,
    ctx: &CurrencyCtx,
    groups: &[PeriodAmounts],
    drillable: bool,
) -> Vec<ReportCell> {
    let mut cells = Vec::with_capacity(groups.len() * report.columns.len());
    for amounts in groups {
        for col in &report.columns {
            let value = match col.label.as_str() {
                "opening" => Some(amounts.opening),
                "debit" => Some(amounts.debit),
                "credit" => Some(amounts.credit),
                "closing" | "balance" => Some(amounts.closing),
                _ => None,
            };
            let cell = ReportCell::monetary(value, ctx.scale, Some(ctx.currency))
                .with_drillable(drillable);
            cells.push(if col.blank_if_zero {
                cell.blank_if_zero()
            } else {
                cell
            });
        }
    }
    cells
}

async fn load_period_map(
    cx: &mut ReportCx<'_>,
    company: i64,
    period: &Period,
) -> Result<BTreeMap<i64, PeriodAmounts>> {
    let from = period
        .from
        .ok_or_else(|| invalid("Trial Balance requires a date range with a start date."))?;
    let base = base_domain(company)?;
    let records = cx.env().records("account.move.line")?;

    let opening_groups = records
        ._read_group(
            cx.tx(),
            ReadGroup::new(base.clone() & cond(json!([["date", "<", day(from)]]))?)
                .groupby(["account_id"])
                .aggregates(["balance:sum"]),
        )
        .await?;
    let activity_groups = records
        ._read_group(
            cx.tx(),
            ReadGroup::new(
                base & cond(json!([
                    ["date", ">=", day(from)],
                    ["date", "<=", day(period.to)]
                ]))?,
            )
            .groupby(["account_id"])
            .aggregates(["debit:sum", "credit:sum"]),
        )
        .await?;

    let mut out: BTreeMap<i64, PeriodAmounts> = BTreeMap::new();
    for row in opening_groups.rows() {
        let aid = row
            .first()
            .and_then(GroupValue::record_id)
            .ok_or_else(|| Error::internal("Trial Balance opening group has no account id."))?
            .get();
        let opening = num(row.get(1))?;
        let entry = out.entry(aid).or_default();
        entry.opening = opening;
        entry.closing = opening;
    }
    for row in activity_groups.rows() {
        let aid = row
            .first()
            .and_then(GroupValue::record_id)
            .ok_or_else(|| Error::internal("Trial Balance activity group has no account id."))?
            .get();
        let debit = num(row.get(1))?;
        let credit = num(row.get(2))?;
        let entry = out.entry(aid).or_default();
        entry.debit = debit;
        entry.credit = credit;
        entry.closing = entry.opening + debit - credit;
    }
    Ok(out)
}

async fn render_lines(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
) -> Result<Expansion> {
    let [company] = options.companies.as_slice() else {
        return Err(invalid("Trial Balance requires a single company."));
    };
    let ctx = load_currency_ctx(cx, *company).await?;

    let mut per_group: Vec<BTreeMap<i64, PeriodAmounts>> =
        Vec::with_capacity(options.column_groups.len());
    let mut all_accounts: BTreeSet<i64> = BTreeSet::new();
    for group in &options.column_groups {
        let map = load_period_map(cx, *company, group).await?;
        all_accounts.extend(map.keys().copied());
        per_group.push(map);
    }

    if all_accounts.is_empty() {
        return Ok(Expansion::new(Vec::new()));
    }

    let ids: Vec<i64> = all_accounts.into_iter().collect();
    let env = cx.env().clone();
    let codes = codes_by_account(cx.tx(), &env, &ids, ctx.root_company).await?;
    let mut names: BTreeMap<i64, String> = BTreeMap::new();
    let arows = env
        .records("account.account")?
        .browse(ids.iter().copied())?
        .read_rows(cx.tx(), &["id", "name"], Load::Raw)
        .await?;
    for r in arows.iter().map(Value::Row) {
        names.insert(int_field(&r, "id")?, text_field(&r, "name")?);
    }

    let mut rows: Vec<(String, String, i64, Vec<PeriodAmounts>)> = Vec::with_capacity(ids.len());
    let mut totals = vec![PeriodAmounts::default(); options.column_groups.len()];

    for aid in ids {
        let mut group_vals = Vec::with_capacity(per_group.len());
        let mut all_zero = true;
        for (gi, map) in per_group.iter().enumerate() {
            let amt = map.get(&aid).copied().unwrap_or_default();
            if !amt.opening.is_zero()
                || !amt.debit.is_zero()
                || !amt.credit.is_zero()
                || !amt.closing.is_zero()
            {
                all_zero = false;
            }
            totals[gi].opening += amt.opening;
            totals[gi].debit += amt.debit;
            totals[gi].credit += amt.credit;
            totals[gi].closing += amt.closing;
            group_vals.push(amt);
        }
        if all_zero && options.hide_zero {
            continue;
        }
        let code = codes.get(&aid).cloned().unwrap_or_default();
        let raw_name = names
            .get(&aid)
            .cloned()
            .unwrap_or_else(|| format!("Account #{aid}"));
        let label = if code.is_empty() {
            raw_name
        } else {
            format!("{code} {raw_name}")
        };
        rows.push((code, label, aid, group_vals));
    }

    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let root = LineId::root(report.id);
    let mut lines = Vec::with_capacity(rows.len() + 1);
    for (_, label, aid, group_vals) in rows {
        let mut line = Line::new(
            root.child(Segment::group(
                "account_id",
                "account.account",
                &aid.to_string(),
            )),
            label,
            LineKind::Detail,
            period_cells(report, &ctx, &group_vals, true),
        );
        line.caret = vec!["journal_items", "open"];
        lines.push(line);
    }

    lines.push(Line::new(
        root.child(Segment::total()),
        "Total",
        LineKind::Total,
        period_cells(report, &ctx, &totals, false),
    ));

    Ok(Expansion::new(lines))
}

fn parse_account(report: &ReportDef, id: &LineId) -> Result<i64> {
    if id.report() != report.id {
        return Err(invalid("This line is not part of this Trial Balance."));
    }
    let segs = id.segments();
    if segs.len() != 2 {
        return Err(invalid("This is not a Trial Balance account line."));
    }
    let seg = &segs[1];
    if !matches!(seg.kind(), SegmentKind::Group(f) if f == "account_id")
        || seg.model() != "account.account"
    {
        return Err(invalid("This is not a Trial Balance account line."));
    }
    let aid: i64 = seg
        .value()
        .parse()
        .map_err(|_| invalid("Invalid Trial Balance account id."))?;
    if aid <= 0 {
        return Err(invalid("Invalid Trial Balance account id."));
    }
    Ok(aid)
}

async fn drill_line(
    _cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
    id: &LineId,
    target: &DrillTarget,
) -> Result<Json> {
    let [company] = options.companies.as_slice() else {
        return Err(invalid("Trial Balance requires a single company."));
    };
    let aid = parse_account(report, id)?;
    match target {
        DrillTarget::Item(item) if item == "open" => Ok(json!({
            "type": "ir.actions.act_window",
            "name": "Account",
            "res_model": "account.account",
            "res_id": aid,
            "target": "new",
            "views": [[false, "form"]],
        })),
        DrillTarget::Item(item) if item == "journal_items" => {
            let domain = base_domain(*company)?
                & cond(json!([
                    ["account_id", "=", aid],
                    ["date", "<=", day(options.period.to)]
                ]))?;
            Ok(json!({
                "type": "ir.actions.act_window",
                "name": report.name,
                "res_model": "account.move.line",
                "target": "current",
                "views": [[false, "list"], [false, "form"]],
                "domain": domain.to_json()?,
                "context": {},
            }))
        }
        DrillTarget::Cell(col_index) => {
            let per_group = report.columns.len();
            if per_group == 0 {
                return Err(invalid("Trial Balance has no columns."));
            }
            let group_idx = *col_index / per_group;
            let col_in_group = *col_index % per_group;
            let period = options
                .column_groups
                .get(group_idx)
                .ok_or_else(|| invalid("Trial Balance period index out of range."))?;
            let col = report
                .columns
                .get(col_in_group)
                .ok_or_else(|| invalid("Trial Balance column index out of range."))?;
            let from = period
                .from
                .ok_or_else(|| invalid("Trial Balance requires a date range with a start date."))?;
            let base = base_domain(*company)? & cond(json!([["account_id", "=", aid]]))?;
            let domain = match col.label.as_str() {
                "opening" => base & cond(json!([["date", "<", day(from)]]))?,
                "debit" | "credit" => {
                    base & cond(json!([
                        ["date", ">=", day(from)],
                        ["date", "<=", day(period.to)]
                    ]))?
                }
                "closing" | "balance" => base & cond(json!([["date", "<=", day(period.to)]]))?,
                _ => return Err(invalid("This Trial Balance cell has no amount to drill.")),
            };
            Ok(json!({
                "type": "ir.actions.act_window",
                "name": report.name,
                "res_model": "account.move.line",
                "target": "current",
                "views": [[false, "list"], [false, "form"]],
                "domain": domain.to_json()?,
                "context": {},
            }))
        }
        _ => Err(invalid("This Trial Balance action is not supported.")),
    }
}

impl ReportProvider for TrialBalance {
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
            if options.order.is_some() {
                return Err(invalid("Trial Balance rows are ordered by account code."));
            }
            Ok(())
        })
    }
    fn lines<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(render_lines(cx, report, options))
    }
    fn expand<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        _options: &'a Options,
        _line: &'a LineId,
        _page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async { Err(invalid("Trial Balance lines do not unfold.")) })
    }
    fn drill<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(drill_line(cx, report, options, line, target))
    }
}

erp_addon_reporting::provider_declarations!(TrialBalance);

#[cfg(test)]
mod tests {
    use super::PeriodAmounts;
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn trial_balance_conserves_exact_decimal_equation() {
        let opening = Decimal::from_str("90071992547409.91").unwrap();
        let debit = Decimal::from_str("37.25").unwrap();
        let credit = Decimal::from_str("90071992547409.90").unwrap();
        let row = PeriodAmounts {
            opening,
            debit,
            credit,
            closing: opening + debit - credit,
        };
        assert_eq!(row.closing, Decimal::from_str("37.26").unwrap());
        assert_eq!(row.opening + row.debit - row.credit, row.closing);
    }
}
