// SPDX-License-Identifier: MIT OR Apache-2.0
//! Partner Ledger dynamic report provider (`account_reporting.partner_ledger`):
//! partner -> chronological journal items with running balance, locale dates, overdue due-date
//! highlighting, caret actions (`journal_items`, `partner`, `entry`), engine `totals_below`,
//! and caller-scoped historical unreconciled cutoff admission.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_account::ledger::code::codes_by_account;
use erp_addon_reporting::{
    definition::ReportDef,
    line::{
        Cell as ReportCell, DrillTarget, ExpandPage, Expansion, Line, LineId, LineKind, Segment,
        SegmentKind,
    },
    options::Options,
    provider::{ProviderSpec, ReportCx, ReportProvider},
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, FieldOp, Load, SearchOptions};

pub(crate) struct PartnerLedger;
const SPEC: ProviderSpec = ProviderSpec::new("account_reporting.partner_ledger", 2);

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
            "Partner Ledger amount is not exact decimal: {other:?}"
        ))),
    }
}

fn get(row: &Value, field: &str) -> Result<Value> {
    row.dict()
        .ok_or_else(|| Error::internal("Partner Ledger row is not a batch handle."))?
        .get(field)
}

fn int_field(row: &Value, field: &str) -> Result<i64> {
    match get(row, field)? {
        Value::Int(n) => Ok(n),
        other => Err(Error::internal(format!(
            "Partner Ledger {field} is not an integer: {other:?}"
        ))),
    }
}

fn opt_int_field(row: &Value, field: &str) -> Result<Option<i64>> {
    match get(row, field)? {
        Value::Int(n) if n > 0 => Ok(Some(n)),
        Value::False | Value::Int(_) => Ok(None),
        other => Err(Error::internal(format!(
            "Partner Ledger {field} is not an optional id: {other:?}"
        ))),
    }
}

fn dec_field(row: &Value, field: &str) -> Result<Decimal> {
    match get(row, field)? {
        Value::Decimal(n) => Ok(n),
        Value::Int(n) => Ok(Decimal::from(n)),
        Value::False => Ok(Decimal::ZERO),
        other => Err(Error::internal(format!(
            "Partner Ledger {field} is not decimal: {other:?}"
        ))),
    }
}

fn text_field(row: &Value, field: &str) -> Result<String> {
    match get(row, field)? {
        Value::Str(s) => Ok(s.to_string()),
        Value::False => Ok(String::new()),
        other => Err(Error::internal(format!(
            "Partner Ledger {field} is not text: {other:?}"
        ))),
    }
}

fn opt_date_field(row: &Value, field: &str) -> Result<Option<Date>> {
    match get(row, field)? {
        Value::Date(d) => Ok(Some(d)),
        Value::False => Ok(None),
        other => Err(Error::internal(format!(
            "Partner Ledger {field} is not a date: {other:?}"
        ))),
    }
}

fn require_scope(options: &Options) -> Result<(i64, Date, Date)> {
    let [company] = options.companies.as_slice() else {
        return Err(invalid("Select one company for Partner Ledger."));
    };
    if options.column_groups.len() != 1 {
        return Err(invalid("Use a single period for Partner Ledger."));
    }
    let from = options
        .period
        .from
        .ok_or_else(|| invalid("Partner Ledger requires a date range."))?;
    Ok((*company, from, options.period.to))
}

/// Resolves the caller-scoped base domain (`account.move.line` up through `period.to`),
/// enforcing historical reconciliation admission when `provider.unreconciled = true`.
async fn eligible_base(cx: &mut ReportCx<'_>, options: &Options) -> Result<Domain> {
    let (company, _from, cutoff) = require_scope(options)?;
    let mut base = cond(json!([
        ["company_id", "=", company],
        ["parent_state", "=", "posted"],
        ["date", "<=", day(cutoff)],
        ["journal_id.journal_group_id", "=", false],
        [
            "account_id.account_type",
            "in",
            ["asset_receivable", "liability_payable"]
        ]
    ]))?;
    if options.provider.get("unreconciled").and_then(Json::as_bool) != Some(true) {
        return Ok(base);
    }
    let marker = cx
        .tx()
        .query(
            "SELECT safe_from FROM account_reconcile_coverage WHERE name='partial_matches'",
            &[],
        )
        .await?;
    let [row] = marker.as_slice() else {
        return Err(invalid(
            "Reconciliation history coverage has not been established.",
        ));
    };
    let floor: Option<Date> = row
        .try_get(0)
        .map_err(|e| Error::db(format!("Reading historical coverage: {e}")))?;
    if let Some(floor) = floor.filter(|start| cutoff < *start) {
        let (y, m, d) = floor.ymd();
        return Err(invalid(format!(
            "Reconciliation history is available from {y:04}-{m:02}-{d:02}."
        )));
    }
    let env = cx.env().clone();
    for field in [
        "amount_residual",
        "balance",
        "company_id",
        "currency_id",
        "account_id",
        "partner_id",
        "date",
    ] {
        if !cx
            .tx()
            .has_field_access(&env, "account.move.line", field, FieldOp::Read)
            .await?
        {
            return Err(Error::access(
                "Reading historical residuals requires access to journal item reconciliation fields.",
            ));
        }
    }
    let limit = cx.bounds().max_filter_source_rows;
    let ids = env
        .records("account.move.line")?
        .search_with(
            cx.tx(),
            &base,
            &SearchOptions::new().limit(limit as u64 + 1),
        )
        .await?
        .ids();
    if ids.len() > limit {
        return Err(invalid(
            "The journal items exceed reporting.max_filter_source_rows. Choose an earlier cutoff or ask your administrator to review the limit.",
        ));
    }
    if ids.is_empty() {
        return Ok(base & cond(json!([["id", "in", []]]))?);
    }
    let rows = cx
        .tx()
        .query(
            include_str!("partner_ledger_residual.sql"),
            &[&ids, &company, &cutoff],
        )
        .await?;
    if rows.len() != ids.len() {
        return Err(invalid(
            "The journal item scope changed. Refresh the Partner Ledger.",
        ));
    }
    let mut eligible = Vec::<i64>::new();
    for row in rows {
        let valid: bool = row
            .try_get(1)
            .map_err(|e| Error::db(format!("Reading reconciliation evidence: {e}")))?;
        if !valid {
            return Err(invalid(
                "Historical residuals cannot be established for this cutoff. Check currency, dated matching evidence, and overlapping re-matches.",
            ));
        }
        let open: bool = row
            .try_get(2)
            .map_err(|e| Error::db(format!("Reading historical residual: {e}")))?;
        if open {
            eligible.push(
                row.try_get(0)
                    .map_err(|e| Error::db(format!("Reading historical journal item: {e}")))?,
            );
        }
    }
    base = base & cond(json!([["id", "in", eligible]]))?;
    Ok(base)
}

fn partner_domain(base: Domain, partner_id: i64) -> Result<Domain> {
    if partner_id > 0 {
        Ok(base & cond(json!([["partner_id", "=", partner_id]]))?)
    } else {
        Ok(base & cond(json!([["partner_id", "=", false]]))?)
    }
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
        .ok_or_else(|| invalid("The selected Partner Ledger company is not visible."))?;
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
        .ok_or_else(|| invalid("The Partner Ledger currency is not visible."))?;
    let scale = u32::try_from(int_field(&cur, "decimal_places")?)
        .map_err(|_| invalid("The Partner Ledger currency precision is invalid."))?;
    Ok(CurrencyCtx {
        currency,
        scale,
        root_company,
    })
}

struct RowData<'a> {
    journal: &'a str,
    account: &'a str,
    date: Option<Date>,
    due_date: Option<Date>,
    due_danger: bool,
    matching: &'a str,
    opening: Option<Decimal>,
    debit: Option<Decimal>,
    credit: Option<Decimal>,
    balance: Option<Decimal>,
    drillable: bool,
}

fn build_cells(report: &ReportDef, ctx: &CurrencyCtx, row: &RowData<'_>) -> Vec<ReportCell> {
    report
        .columns
        .iter()
        .map(|col| {
            let cell = match col.label.as_str() {
                "journal" => {
                    if row.journal.is_empty() {
                        ReportCell::empty()
                    } else {
                        ReportCell::text(row.journal)
                    }
                }
                "account" => {
                    if row.account.is_empty() {
                        ReportCell::empty()
                    } else {
                        ReportCell::text(row.account)
                    }
                }
                "date" => row.date.map_or_else(ReportCell::empty, ReportCell::date),
                "due_date" => row.due_date.map_or_else(ReportCell::empty, |d| {
                    ReportCell::date(d).with_danger(row.due_danger)
                }),
                "matching" => {
                    if row.matching.is_empty() {
                        ReportCell::empty()
                    } else {
                        ReportCell::text(row.matching)
                    }
                }
                "opening" => ReportCell::monetary(row.opening, ctx.scale, Some(ctx.currency))
                    .with_drillable(row.drillable),
                "debit" => ReportCell::monetary(row.debit, ctx.scale, Some(ctx.currency))
                    .with_drillable(row.drillable),
                "credit" => ReportCell::monetary(row.credit, ctx.scale, Some(ctx.currency))
                    .with_drillable(row.drillable),
                "balance" | "closing" => {
                    ReportCell::monetary(row.balance, ctx.scale, Some(ctx.currency))
                        .with_drillable(row.drillable)
                }
                _ => ReportCell::empty(),
            };
            if col.blank_if_zero {
                cell.blank_if_zero()
            } else {
                cell
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Located {
    Partner(i64),
    Initial(i64),
    Item(i64, i64),
}

fn locate(report: &ReportDef, id: &LineId) -> Result<Located> {
    if id.report() != report.id {
        return Err(invalid("This line is not part of this Partner Ledger."));
    }
    let segs = id.segments();
    let bad = || invalid("This is not a Partner Ledger line.");
    let Some(partner_seg) = segs.get(1) else {
        return Err(bad());
    };
    if !matches!(partner_seg.kind(), SegmentKind::Group(f) if f == "partner_id")
        || partner_seg.model() != "res.partner"
    {
        return Err(bad());
    }
    let pid: i64 = partner_seg.value().parse().map_err(|_| bad())?;
    if pid < 0 {
        return Err(bad());
    }
    if segs.len() == 2 {
        return Ok(Located::Partner(pid));
    }
    if segs.len() != 3 {
        return Err(bad());
    }
    let child = &segs[2];
    match child.kind() {
        SegmentKind::Line(code) if code == "INITIAL" => Ok(Located::Initial(pid)),
        SegmentKind::Group(f) if f == "id" && child.model() == "account.move.line" => {
            let item_id: i64 = child.value().parse().map_err(|_| bad())?;
            if item_id <= 0 {
                return Err(bad());
            }
            Ok(Located::Item(pid, item_id))
        }
        _ => Err(bad()),
    }
}

async fn sum_balance(cx: &mut ReportCx<'_>, domain: Domain) -> Result<Decimal> {
    let groups = cx
        .env()
        .records("account.move.line")?
        ._read_group(cx.tx(), ReadGroup::new(domain).aggregates(["balance:sum"]))
        .await?;
    num(groups.column(0).first())
}

async fn top_lines(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
) -> Result<Expansion> {
    let (company, from, _to) = require_scope(options)?;
    let ctx = load_currency_ctx(cx, company).await?;
    let base = eligible_base(cx, options).await?;
    let period = base.clone() & cond(json!([["date", ">=", day(from)]]))?;
    let env = cx.env().clone();
    let records = env.records("account.move.line")?;

    let closing_groups = records
        ._read_group(
            cx.tx(),
            ReadGroup::new(base)
                .groupby(["partner_id"])
                .aggregates(["balance:sum"]),
        )
        .await?;
    if closing_groups.is_empty() {
        return Ok(Expansion::new(Vec::new()));
    }
    let period_groups = records
        ._read_group(
            cx.tx(),
            ReadGroup::new(period)
                .groupby(["partner_id"])
                .aggregates(["debit:sum", "credit:sum"]),
        )
        .await?;

    let mut closing_by_partner: BTreeMap<i64, Decimal> = BTreeMap::new();
    for row in closing_groups.rows() {
        let pid = row
            .first()
            .and_then(GroupValue::record_id)
            .map(|id| id.get())
            .unwrap_or(0);
        closing_by_partner.insert(pid, num(row.get(1))?);
    }
    let mut period_by_partner: BTreeMap<i64, [Decimal; 2]> = BTreeMap::new();
    for row in period_groups.rows() {
        let pid = row
            .first()
            .and_then(GroupValue::record_id)
            .map(|id| id.get())
            .unwrap_or(0);
        period_by_partner.insert(pid, [num(row.get(1))?, num(row.get(2))?]);
    }

    let real_ids: Vec<i64> = closing_by_partner
        .keys()
        .copied()
        .filter(|&id| id > 0)
        .collect();
    let mut names: BTreeMap<i64, String> = BTreeMap::new();
    if !real_ids.is_empty() {
        let found = env
            .records("res.partner")?
            .browse(real_ids.iter().copied())?
            .read_rows(cx.tx(), &["id", "display_name", "name"], Load::Raw)
            .await?;
        for row in found.iter().map(Value::Row) {
            let pid = int_field(&row, "id")?;
            let dn = text_field(&row, "display_name")?;
            let n = if dn.is_empty() {
                text_field(&row, "name")?
            } else {
                dn
            };
            names.insert(pid, n);
        }
    }

    let mut ordered: Vec<(bool, String, i64, Decimal, Decimal, Decimal, Decimal)> = Vec::new();
    let mut grand_opening = Decimal::ZERO;
    let mut grand_debit = Decimal::ZERO;
    let mut grand_credit = Decimal::ZERO;
    let mut grand_closing = Decimal::ZERO;

    for (pid, closing) in closing_by_partner {
        let [debit, credit] = period_by_partner
            .get(&pid)
            .copied()
            .unwrap_or([Decimal::ZERO; 2]);
        let opening = closing - (debit - credit);
        let is_unknown = pid == 0;
        let label = if is_unknown {
            "Unknown Partner".to_owned()
        } else {
            names
                .get(&pid)
                .cloned()
                .unwrap_or_else(|| format!("Partner #{pid}"))
        };
        grand_opening += opening;
        grand_debit += debit;
        grand_credit += credit;
        grand_closing += closing;
        ordered.push((is_unknown, label, pid, opening, debit, credit, closing));
    }
    ordered.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
            .then_with(|| a.2.cmp(&b.2))
    });

    let root = LineId::root(report.id);
    let mut lines = Vec::with_capacity(ordered.len() + 1);
    for (_, label, pid, opening, debit, credit, closing) in ordered {
        let line_id = root.child(Segment::group(
            "partner_id",
            "res.partner",
            &pid.to_string(),
        ));
        let mut line = Line::new(
            line_id,
            label,
            LineKind::Group,
            build_cells(
                report,
                &ctx,
                &RowData {
                    journal: "",
                    account: "",
                    date: None,
                    due_date: None,
                    due_danger: false,
                    matching: "",
                    opening: Some(opening),
                    debit: Some(debit),
                    credit: Some(credit),
                    balance: Some(closing),
                    drillable: true,
                },
            ),
        );
        line.unfoldable = true;
        line.caret = if pid > 0 {
            vec!["journal_items", "partner"]
        } else {
            vec!["journal_items"]
        };
        lines.push(line);
    }

    lines.push(Line::new(
        root.child(Segment::total()),
        "Total Partner Ledger",
        LineKind::Total,
        build_cells(
            report,
            &ctx,
            &RowData {
                journal: "",
                account: "",
                date: None,
                due_date: None,
                due_danger: false,
                matching: "",
                opening: Some(grand_opening),
                debit: Some(grand_debit),
                credit: Some(grand_credit),
                balance: Some(grand_closing),
                drillable: false,
            },
        ),
    ));

    Ok(Expansion::new(lines))
}

async fn expand_partner(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
    id: &LineId,
    page: ExpandPage,
) -> Result<Expansion> {
    let Located::Partner(pid) = locate(report, id)? else {
        return Err(invalid("This Partner Ledger line has no children."));
    };
    let (company, from, to) = require_scope(options)?;
    let ctx = load_currency_ctx(cx, company).await?;
    let base = partner_domain(eligible_base(cx, options).await?, pid)?;
    let opening_domain = base.clone() & cond(json!([["date", "<", day(from)]]))?;
    let period_domain = base.clone() & cond(json!([["date", ">=", day(from)]]))?;

    let opening = sum_balance(cx, opening_domain).await?;
    let mut lines = Vec::new();
    if page.offset == 0 && !opening.is_zero() {
        lines.push(Line::new(
            id.child(Segment::line("INITIAL")),
            "Initial balance",
            LineKind::Detail,
            build_cells(
                report,
                &ctx,
                &RowData {
                    journal: "",
                    account: "",
                    date: None,
                    due_date: None,
                    due_danger: false,
                    matching: "",
                    opening: Some(opening),
                    debit: None,
                    credit: None,
                    balance: Some(opening),
                    drillable: true,
                },
            ),
        ));
    }

    let env = cx.env().clone();
    let records = env.records("account.move.line")?;
    let count = records.search_count(cx.tx(), &period_domain, None).await?;
    let found = records
        .search_with(
            cx.tx(),
            &period_domain,
            &SearchOptions::new()
                .order("date, id")
                .offset(page.offset)
                .limit(page.limit),
        )
        .await?;
    let rows = found
        .read_rows(
            cx.tx(),
            &[
                "id",
                "move_name",
                "name",
                "journal_id",
                "account_id",
                "date",
                "date_maturity",
                "matching_number",
                "full_reconcile_id",
                "debit",
                "credit",
                "balance",
            ],
            Load::Raw,
        )
        .await?;

    let mut journal_ids = BTreeSet::new();
    let mut account_ids = BTreeSet::new();
    for row in rows.iter().map(Value::Row) {
        if let Some(jid) = opt_int_field(&row, "journal_id")? {
            journal_ids.insert(jid);
        }
        if let Some(aid) = opt_int_field(&row, "account_id")? {
            account_ids.insert(aid);
        }
    }

    let mut journal_codes: BTreeMap<i64, String> = BTreeMap::new();
    if !journal_ids.is_empty() {
        let jrows = env
            .records("account.journal")?
            .browse(journal_ids.iter().copied())?
            .read_rows(cx.tx(), &["id", "code"], Load::Raw)
            .await?;
        for r in jrows.iter().map(Value::Row) {
            journal_codes.insert(int_field(&r, "id")?, text_field(&r, "code")?);
        }
    }
    let account_id_vec: Vec<i64> = account_ids.into_iter().collect();
    let account_codes = codes_by_account(cx.tx(), &env, &account_id_vec, ctx.root_company).await?;

    let mut running = match rows.iter().next().map(Value::Row) {
        Some(first) if page.offset > 0 => {
            let first_date = opt_date_field(&first, "date")?
                .ok_or_else(|| Error::internal("Partner Ledger item has no date."))?;
            let first_id = int_field(&first, "id")?;
            let before = base.clone()
                & cond(json!([
                    "|",
                    ["date", "<", day(first_date)],
                    "&",
                    ["date", "=", day(first_date)],
                    ["id", "<", first_id]
                ]))?;
            sum_balance(cx, before).await?
        }
        _ => opening,
    };

    for row in rows.iter().map(Value::Row) {
        let item_id = int_field(&row, "id")?;
        let move_name = text_field(&row, "move_name")?;
        let desc = text_field(&row, "name")?;
        let label = if !move_name.is_empty() && move_name != "/" {
            move_name
        } else if !desc.is_empty() {
            desc
        } else {
            format!("Item #{item_id}")
        };
        let jcode = opt_int_field(&row, "journal_id")?
            .and_then(|jid| journal_codes.get(&jid))
            .map(String::as_str)
            .unwrap_or("");
        let acode = opt_int_field(&row, "account_id")?
            .and_then(|aid| account_codes.get(&aid))
            .map(String::as_str)
            .unwrap_or("");
        let item_date = opt_date_field(&row, "date")?;
        let due_date = opt_date_field(&row, "date_maturity")?.or(item_date);
        let matching = text_field(&row, "matching_number")?;
        let reconciled = opt_int_field(&row, "full_reconcile_id")?.is_some();
        let debit = dec_field(&row, "debit")?;
        let credit = dec_field(&row, "credit")?;
        let bal = dec_field(&row, "balance")?;
        running = running
            .checked_add(bal)
            .ok_or_else(|| invalid("Partner Ledger running balance is too large."))?;
        let due_danger = due_date.is_some_and(|d| d < to) && !reconciled && !bal.is_zero();

        let mut detail = Line::new(
            id.child(Segment::group(
                "id",
                "account.move.line",
                &item_id.to_string(),
            )),
            label,
            LineKind::Detail,
            build_cells(
                report,
                &ctx,
                &RowData {
                    journal: jcode,
                    account: acode,
                    date: item_date,
                    due_date,
                    due_danger,
                    matching: &matching,
                    opening: None,
                    debit: Some(debit),
                    credit: Some(credit),
                    balance: Some(running),
                    drillable: true,
                },
            ),
        );
        detail.caret = vec!["entry"];
        lines.push(detail);
    }

    let end = page.offset.saturating_add(page.limit);
    if count > end {
        let mut more = Line::new(
            id.child(Segment::more()),
            "",
            LineKind::More,
            build_cells(
                report,
                &ctx,
                &RowData {
                    journal: "",
                    account: "",
                    date: None,
                    due_date: None,
                    due_danger: false,
                    matching: "",
                    opening: None,
                    debit: None,
                    credit: None,
                    balance: None,
                    drillable: false,
                },
            ),
        );
        more.remaining = Some(count - end);
        more.next_offset = Some(end);
        lines.push(more);
    }

    Ok(Expansion::new(lines))
}

async fn drill_line(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
    id: &LineId,
    target: &DrillTarget,
) -> Result<Json> {
    let loc = locate(report, id)?;
    let (_company, from, _to) = require_scope(options)?;
    let base = eligible_base(cx, options).await?;
    let env = cx.env().clone();

    match (loc, target) {
        (Located::Partner(pid), DrillTarget::Item(item)) if item == "partner" && pid > 0 => {
            Ok(json!({
                "type": "ir.actions.act_window",
                "name": "Partner",
                "res_model": "res.partner",
                "res_id": pid,
                "target": "current",
                "views": [[false, "form"]],
            }))
        }
        (Located::Item(pid, item_id), DrillTarget::Item(item)) if item == "entry" => {
            let pdom = partner_domain(base, pid)? & cond(json!([["id", "=", item_id]]))?;
            let found = env
                .records("account.move.line")?
                .search(cx.tx(), &pdom)
                .await?;
            let rows = found.read_rows(cx.tx(), &["move_id"], Load::Raw).await?;
            let row = rows.iter().next().map(Value::Row).ok_or_else(|| {
                invalid("This journal item is not visible in the Partner Ledger scope.")
            })?;
            let move_id = int_field(&row, "move_id")?;
            Ok(json!({
                "type": "ir.actions.act_window",
                "name": "Journal Entry",
                "res_model": "account.move",
                "res_id": move_id,
                "target": "current",
                "views": [[false, "form"]],
            }))
        }
        (Located::Partner(pid), DrillTarget::Item(item)) if item == "journal_items" => {
            let domain = partner_domain(base, pid)?;
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
        (Located::Partner(pid), DrillTarget::Cell(col_idx)) => {
            let label = report
                .columns
                .get(*col_idx)
                .ok_or_else(|| invalid("Partner Ledger has no such column."))?
                .label
                .as_str();
            let pdom = partner_domain(base, pid)?;
            let domain = match label {
                "opening" => pdom & cond(json!([["date", "<", day(from)]]))?,
                "debit" | "credit" => pdom & cond(json!([["date", ">=", day(from)]]))?,
                "balance" | "closing" => pdom,
                _ => return Err(invalid("This Partner Ledger cell has no amount to drill.")),
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
        (Located::Initial(pid), DrillTarget::Cell(_)) => {
            let domain = partner_domain(base, pid)? & cond(json!([["date", "<", day(from)]]))?;
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
        (Located::Item(pid, item_id), DrillTarget::Cell(col_idx)) => {
            let label = report
                .columns
                .get(*col_idx)
                .ok_or_else(|| invalid("Partner Ledger has no such column."))?
                .label
                .as_str();
            let pdom = partner_domain(base, pid)?;
            let item_dom = pdom.clone() & cond(json!([["id", "=", item_id]]))?;
            let found = env
                .records("account.move.line")?
                .search(cx.tx(), &item_dom)
                .await?;
            let rows = found.read_rows(cx.tx(), &["id", "date"], Load::Raw).await?;
            let row = rows.iter().next().map(Value::Row).ok_or_else(|| {
                invalid("This journal item is not visible in the Partner Ledger scope.")
            })?;
            let item_date = opt_date_field(&row, "date")?
                .ok_or_else(|| Error::internal("Partner Ledger item has no date."))?;
            let domain = match label {
                "balance" | "closing" => {
                    pdom & cond(json!([
                        "|",
                        ["date", "<", day(item_date)],
                        "&",
                        ["date", "=", day(item_date)],
                        ["id", "<=", item_id]
                    ]))?
                }
                _ => item_dom,
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
        _ => Err(invalid("This Partner Ledger action is not supported.")),
    }
}

impl ReportProvider for PartnerLedger {
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
            for (key, value) in &options.provider {
                if key != "unreconciled" || !value.is_boolean() {
                    return Err(invalid(format!(
                        "Invalid Partner Ledger option {key}. Use unreconciled with a boolean value."
                    )));
                }
            }
            options
                .provider
                .entry("unreconciled".to_owned())
                .or_insert(Json::Bool(false));
            Ok(())
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
        Box::pin(expand_partner(cx, report, options, line, page))
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

erp_addon_reporting::provider_declarations!(PartnerLedger);
