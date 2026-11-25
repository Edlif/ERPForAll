// SPDX-License-Identifier: MIT OR Apache-2.0
//! Aged Receivable and Aged Payable dynamic report providers (`account_reporting.aged_receivable`,
//! `account_reporting.aged_payable`): one implementation parameterised by account type. Partner
//! lines hold the open items of the selected company aged into buckets by due date (the item's
//! due date, else its accounting date); each partner unfolds into its journal items, one page at
//! a time. Every read goes through the ORM as the caller, so record rules and field access apply.
//! The bucket width is the provider option `interval_days`.

use std::collections::BTreeMap;

use erp_addon_reporting::{
    definition::ReportDef,
    line::{
        Cell as ReportCell, CellKind, DrillTarget, ExpandPage, Expansion, Line, LineId, LineKind,
        Segment, SegmentKind,
    },
    options::Options,
    provider::{ProviderSpec, ReportCx, ReportProvider},
    settings::MAX_FILTER_SOURCE_ROWS,
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, Load, SearchOptions};

/// The bucket column labels, oldest last, then the total.
const BUCKETS: [&str; 7] = [
    "not_due", "d1_30", "d31_60", "d61_90", "d91_120", "older", "total",
];
const TOTAL: usize = 6;
const INTERVAL: &str = "interval_days";
const DEFAULT_INTERVAL: i64 = 30;
const MAX_INTERVAL: i64 = 365;
const ITEM_FIELDS: [&str; 7] = [
    "id",
    "partner_id",
    "move_name",
    "name",
    "date",
    "date_maturity",
    "amount_residual",
];

/// One aged report: its declaration, the account type it ages, and its sign.
pub(crate) struct Aged {
    spec: ProviderSpec,
    account_type: &'static str,
    /// Payables are credits: shown with the opposite sign so open amounts read positive.
    negate: bool,
}

pub(crate) static AGED_RECEIVABLE: Aged = Aged {
    spec: ProviderSpec::new("account_reporting.aged_receivable", 2),
    account_type: "asset_receivable",
    negate: false,
};
pub(crate) static AGED_PAYABLE: Aged = Aged {
    spec: ProviderSpec::new("account_reporting.aged_payable", 2),
    account_type: "liability_payable",
    negate: true,
};

fn invalid(message: impl Into<String>) -> Error {
    Error::validation(message.into())
}

fn day(date: Date) -> String {
    let (y, m, d) = date.ymd();
    format!("{y:04}-{m:02}-{d:02}")
}

fn cond(value: Json) -> Result<Domain> {
    Ok(Domain::from_json(&value)?)
}

fn get(row: &Value, field: &str, what: &str) -> Result<Value> {
    row.dict()
        .ok_or_else(|| Error::internal(format!("Reading {what}: the row is not a batch handle.")))?
        .get(field)
}

fn int_of(row: &Value, field: &str, what: &str) -> Result<i64> {
    match get(row, field, what)? {
        Value::Int(n) => Ok(n),
        other => Err(Error::internal(format!(
            "Reading {what}: {field} is not an integer ({other:?})."
        ))),
    }
}

fn opt_id_of(row: &Value, field: &str, what: &str) -> Result<Option<i64>> {
    match get(row, field, what)? {
        Value::Int(n) if n > 0 => Ok(Some(n)),
        Value::False => Ok(None),
        other => Err(Error::internal(format!(
            "Reading {what}: {field} is not an optional id ({other:?})."
        ))),
    }
}

fn text_of(row: &Value, field: &str, what: &str) -> Result<String> {
    match get(row, field, what)? {
        Value::Str(s) => Ok(s.to_string()),
        Value::False => Ok(String::new()),
        other => Err(Error::internal(format!(
            "Reading {what}: {field} is not text ({other:?})."
        ))),
    }
}

fn opt_date_of(row: &Value, field: &str, what: &str) -> Result<Option<Date>> {
    match get(row, field, what)? {
        Value::Date(d) => Ok(Some(d)),
        Value::False => Ok(None),
        other => Err(Error::internal(format!(
            "Reading {what}: {field} is not a date ({other:?})."
        ))),
    }
}

fn dec_of(row: &Value, field: &str, what: &str) -> Result<Decimal> {
    match get(row, field, what)? {
        Value::Decimal(n) => Ok(n),
        Value::Int(n) => Ok(Decimal::from(n)),
        Value::False => Ok(Decimal::ZERO),
        other => Err(Error::internal(format!(
            "Reading {what}: {field} is not decimal ({other:?})."
        ))),
    }
}

/// The validated `interval_days` of `options` (`report` names the report in the refusal).
fn interval(report: &ReportDef, value: Option<&Json>) -> Result<i32> {
    match value {
        None => Ok(DEFAULT_INTERVAL as i32),
        Some(v) => v
            .as_i64()
            .filter(|n| (1..=MAX_INTERVAL).contains(n))
            .map(|n| n as i32)
            .ok_or_else(|| {
                invalid(format!(
                    "{} option {INTERVAL} must be a whole number of days from 1 to {MAX_INTERVAL}.",
                    report.name
                ))
            }),
    }
}

/// The bucket of an item due on `due` at `as_of` with buckets `width` days wide:
/// 0 not due, 1..=4 the late intervals, 5 older.
fn bucket(due: Date, as_of: Date, width: i32) -> usize {
    let late = as_of.days() - due.days();
    if late <= 0 {
        0
    } else {
        // late >= 1 and width >= 1, so the quotient is non-negative
        (((late - 1) / width) as usize + 1).min(5)
    }
}

/// The inclusive due-date range of bucket `index` (`None`: open on that side).
fn bucket_range(index: usize, as_of: Date, width: i32) -> (Option<Date>, Option<Date>) {
    let shift = |n: i32| Date::from_days(as_of.days() - n);
    match index {
        0 => (Some(as_of), None),
        1..=4 => {
            let k = index as i32;
            (Some(shift(k * width)), Some(shift((k - 1) * width + 1)))
        }
        5 => (None, Some(shift(4 * width + 1))),
        _ => (None, None),
    }
}

/// Items whose due date (the item's, else its accounting date) lies in `lo..=hi`.
fn due_domain(lo: Option<Date>, hi: Option<Date>) -> Result<Domain> {
    if lo.is_none() && hi.is_none() {
        return cond(json!([]));
    }
    let mut on_maturity = vec![json!(["date_maturity", "!=", false])];
    let mut on_date = vec![json!(["date_maturity", "=", false])];
    if let Some(lo) = lo {
        on_maturity.push(json!(["date_maturity", ">=", day(lo)]));
        on_date.push(json!(["date", ">=", day(lo)]));
    }
    if let Some(hi) = hi {
        on_maturity.push(json!(["date_maturity", "<=", day(hi)]));
        on_date.push(json!(["date", "<=", day(hi)]));
    }
    Ok(cond(Json::Array(on_maturity))? | cond(Json::Array(on_date))?)
}

fn partner_domain(partner: i64) -> Result<Domain> {
    if partner > 0 {
        cond(json!([["partner_id", "=", partner]]))
    } else {
        cond(json!([["partner_id", "=", false]]))
    }
}

/// What every call of one report shares: the scope, the layout and the currency.
struct Scope<'r> {
    report: &'r ReportDef,
    as_of: Date,
    width: i32,
    base: Domain,
    currency: i64,
    scale: u32,
    negate: bool,
}

impl Aged {
    async fn scope<'r>(
        &self,
        cx: &mut ReportCx<'_>,
        report: &'r ReportDef,
        options: &Options,
    ) -> Result<Scope<'r>> {
        let name = &report.name;
        for column in &report.columns {
            let ok = match column.label.as_str() {
                "due_date" => column.kind == CellKind::Date,
                label if BUCKETS.contains(&label) => column.kind == CellKind::Monetary,
                _ => false,
            };
            if !ok {
                return Err(invalid(format!(
                    "{name} column {} is not supported: use due_date (date) and {} (monetary).",
                    column.label,
                    BUCKETS.join(", ")
                )));
            }
        }
        if let Some(missing) = BUCKETS
            .iter()
            .find(|label| !report.columns.iter().any(|c| c.label == **label))
        {
            return Err(invalid(format!("{name} needs the column {missing}.")));
        }
        let [company] = options.companies.as_slice() else {
            return Err(invalid(format!("Select one company for {name}.")));
        };
        let as_of = options.period.to;
        let today = cx.env().context_today()?;
        if as_of < today {
            // shortcut: refuses past dates; wire the dated residual evidence of the Partner
            // Ledger here to age as of a past cutoff
            return Err(invalid(format!(
                "{name} shows today's open amounts, so it cannot be dated before today: the \
                 residuals as of a past date are not computed yet. Choose today or a later date."
            )));
        }
        let width = interval(report, options.provider.get(INTERVAL))?;
        let base = cond(json!([
            ["company_id", "=", company],
            ["parent_state", "=", "posted"],
            ["date", "<=", day(as_of)],
            ["journal_id.journal_group_id", "=", false],
            ["account_id.account_type", "=", self.account_type],
            ["full_reconcile_id", "=", false],
            ["balance", "!=", 0]
        ]))?;
        let env = cx.env().clone();
        let rows = env
            .records("res.company")?
            .browse([*company])?
            .read_rows(cx.tx(), &["currency_id"], Load::Raw)
            .await?;
        let row = rows
            .iter()
            .next()
            .map(Value::Row)
            .ok_or_else(|| invalid(format!("The selected {name} company is not visible.")))?;
        let currency = int_of(&row, "currency_id", "the company currency")?;
        let rows = env
            .records("res.currency")?
            .browse([currency])?
            .read_rows(cx.tx(), &["decimal_places"], Load::Raw)
            .await?;
        let row = rows
            .iter()
            .next()
            .map(Value::Row)
            .ok_or_else(|| invalid(format!("The {name} currency is not visible.")))?;
        let scale = u32::try_from(int_of(&row, "decimal_places", "the currency precision")?)
            .map_err(|_| invalid(format!("The {name} currency precision is invalid.")))?;
        Ok(Scope {
            report,
            as_of,
            width,
            base,
            currency,
            scale,
            negate: self.negate,
        })
    }
}

/// One open item as the report shows it.
struct Item {
    id: i64,
    partner: i64,
    label: String,
    due: Date,
    amount: Decimal,
}

impl Scope<'_> {
    fn cells(&self, due: Option<Date>, amounts: &[Decimal; 7], drillable: bool) -> Vec<ReportCell> {
        self.report
            .columns
            .iter()
            .map(|column| {
                let cell = match BUCKETS.iter().position(|l| *l == column.label) {
                    Some(index) => {
                        ReportCell::monetary(Some(amounts[index]), self.scale, Some(self.currency))
                            .with_drillable(drillable)
                    }
                    // due_date, the only other label `scope` admits
                    None => due.map_or_else(ReportCell::empty, |d| {
                        ReportCell::date(d).with_danger(d < self.as_of)
                    }),
                };
                if column.blank_if_zero {
                    cell.blank_if_zero()
                } else {
                    cell
                }
            })
            .collect()
    }

    fn amounts(&self, item: &Item) -> [Decimal; 7] {
        let mut amounts = [Decimal::ZERO; 7];
        amounts[bucket(item.due, self.as_of, self.width)] = item.amount;
        amounts[TOTAL] = item.amount;
        amounts
    }

    /// The items of `found`, in their order.
    async fn items(&self, cx: &mut ReportCx<'_>, found: &Records) -> Result<Vec<Item>> {
        let what = format!("{} journal items", self.report.name);
        let rows = found.read_rows(cx.tx(), &ITEM_FIELDS, Load::Raw).await?;
        rows.iter()
            .map(Value::Row)
            .map(|row| {
                let id = int_of(&row, "id", &what)?;
                let move_name = text_of(&row, "move_name", &what)?;
                let desc = text_of(&row, "name", &what)?;
                let label = if !move_name.is_empty() && move_name != "/" {
                    move_name
                } else if !desc.is_empty() {
                    desc
                } else {
                    format!("Item #{id}")
                };
                let date = opt_date_of(&row, "date", &what)?
                    .ok_or_else(|| Error::internal(format!("Reading {what}: {id} has no date.")))?;
                let residual = dec_of(&row, "amount_residual", &what)?;
                Ok(Item {
                    id,
                    partner: opt_id_of(&row, "partner_id", &what)?.unwrap_or(0),
                    label,
                    due: opt_date_of(&row, "date_maturity", &what)?.unwrap_or(date),
                    amount: if self.negate { -residual } else { residual },
                })
            })
            .collect()
    }

    fn act_window(&self, domain: Domain) -> Result<Json> {
        Ok(json!({
            "type": "ir.actions.act_window",
            "name": self.report.name,
            "res_model": "account.move.line",
            "target": "current",
            "views": [[false, "list"], [false, "form"]],
            "domain": domain.to_json()?,
            "context": {},
        }))
    }

    /// The domain of the column at `index`, refused unless it holds an amount.
    fn cell_domain(&self, index: usize) -> Result<Domain> {
        let column = self
            .report
            .columns
            .get(index)
            .ok_or_else(|| invalid(format!("{} has no column {index}.", self.report.name)))?;
        let bucket = BUCKETS
            .iter()
            .position(|l| *l == column.label)
            .ok_or_else(|| {
                invalid(format!(
                    "The {} column {} has no amount to open.",
                    self.report.name, column.label
                ))
            })?;
        let (lo, hi) = bucket_range(bucket, self.as_of, self.width);
        due_domain(lo, hi)
    }
}

#[derive(Clone, Copy)]
enum Located {
    Partner(i64),
    Item(i64, i64),
}

fn locate(report: &ReportDef, id: &LineId) -> Result<Located> {
    if id.report() != report.id {
        return Err(invalid(format!(
            "This line is not part of this {}.",
            report.name
        )));
    }
    let bad = || invalid(format!("This line does not belong to {}.", report.name));
    let segments = id.segments();
    let Some(partner) = segments.get(1) else {
        return Err(bad());
    };
    if !matches!(partner.kind(), SegmentKind::Group(f) if f == "partner_id")
        || partner.model() != "res.partner"
    {
        return Err(bad());
    }
    let pid: i64 = partner.value().parse().map_err(|_| bad())?;
    if pid < 0 {
        return Err(bad());
    }
    match segments {
        [_, _] => Ok(Located::Partner(pid)),
        [_, _, item]
            if matches!(item.kind(), SegmentKind::Group(f) if f == "id")
                && item.model() == "account.move.line" =>
        {
            let item_id: i64 = item.value().parse().map_err(|_| bad())?;
            if item_id <= 0 {
                return Err(bad());
            }
            Ok(Located::Item(pid, item_id))
        }
        _ => Err(bad()),
    }
}

async fn top_lines(scope: &Scope<'_>, cx: &mut ReportCx<'_>) -> Result<Expansion> {
    let name = &scope.report.name;
    let limit = cx.bounds().max_filter_source_rows;
    let env = cx.env().clone();
    let found = env
        .records("account.move.line")?
        .search_with(
            cx.tx(),
            &scope.base,
            &SearchOptions::new().order("id").limit(limit as u64 + 1),
        )
        .await?;
    if found.ids().len() > limit {
        return Err(invalid(format!(
            "{name} has more than {limit} open journal items (setting {MAX_FILTER_SOURCE_ROWS}). \
             Ask your administrator to review the limit."
        )));
    }
    let mut by_partner: BTreeMap<i64, [Decimal; 7]> = BTreeMap::new();
    for item in scope.items(cx, &found).await? {
        let amounts = scope.amounts(&item);
        let sums = by_partner.entry(item.partner).or_default();
        for (sum, amount) in sums.iter_mut().zip(amounts) {
            *sum += amount;
        }
    }
    let real: Vec<i64> = by_partner.keys().copied().filter(|p| *p > 0).collect();
    let mut names: BTreeMap<i64, String> = BTreeMap::new();
    if !real.is_empty() {
        let what = format!("{name} partners");
        let rows = env
            .records("res.partner")?
            .browse(real)?
            .read_rows(cx.tx(), &["id", "display_name"], Load::Raw)
            .await?;
        for row in rows.iter().map(Value::Row) {
            names.insert(
                int_of(&row, "id", &what)?,
                text_of(&row, "display_name", &what)?,
            );
        }
    }
    let mut ordered: Vec<(bool, String, i64, [Decimal; 7])> = by_partner
        .into_iter()
        .map(|(pid, sums)| {
            let label = match names.get(&pid) {
                _ if pid == 0 => "Unknown Partner".to_owned(),
                Some(n) if !n.is_empty() => n.clone(),
                _ => format!("Partner #{pid}"),
            };
            (pid == 0, label, pid, sums)
        })
        .collect();
    ordered.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
            .then_with(|| a.2.cmp(&b.2))
    });

    let root = LineId::root(scope.report.id);
    let mut grand = [Decimal::ZERO; 7];
    let mut lines = Vec::with_capacity(ordered.len() + 1);
    for (_, label, pid, sums) in ordered {
        for (total, amount) in grand.iter_mut().zip(sums) {
            *total += amount;
        }
        let mut line = Line::new(
            root.child(Segment::group(
                "partner_id",
                "res.partner",
                &pid.to_string(),
            )),
            label,
            LineKind::Group,
            scope.cells(None, &sums, true),
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
        format!("Total {name}"),
        LineKind::Total,
        scope.cells(None, &grand, false),
    ));
    Ok(Expansion::new(lines))
}

async fn expand_partner(
    scope: &Scope<'_>,
    cx: &mut ReportCx<'_>,
    id: &LineId,
    page: ExpandPage,
) -> Result<Expansion> {
    let Located::Partner(pid) = locate(scope.report, id)? else {
        return Err(invalid(format!(
            "This {} line has no children.",
            scope.report.name
        )));
    };
    let domain = scope.base.clone() & partner_domain(pid)?;
    let env = cx.env().clone();
    let records = env.records("account.move.line")?;
    let count = records.search_count(cx.tx(), &domain, None).await?;
    let found = records
        .search_with(
            cx.tx(),
            &domain,
            &SearchOptions::new()
                .order("date_maturity, date, id")
                .offset(page.offset)
                .limit(page.limit),
        )
        .await?;
    let mut lines = Vec::new();
    for item in scope.items(cx, &found).await? {
        let amounts = scope.amounts(&item);
        let mut line = Line::new(
            id.child(Segment::group(
                "id",
                "account.move.line",
                &item.id.to_string(),
            )),
            item.label,
            LineKind::Detail,
            scope.cells(Some(item.due), &amounts, true),
        );
        line.caret = vec!["entry"];
        lines.push(line);
    }
    let end = page.offset.saturating_add(page.limit);
    if count > end {
        let mut more = Line::new(
            id.child(Segment::more()),
            "",
            LineKind::More,
            scope
                .report
                .columns
                .iter()
                .map(|_| ReportCell::empty())
                .collect(),
        );
        more.remaining = Some(count - end);
        more.next_offset = Some(end);
        lines.push(more);
    }
    Ok(Expansion::new(lines))
}

async fn drill_line(
    scope: &Scope<'_>,
    cx: &mut ReportCx<'_>,
    id: &LineId,
    target: &DrillTarget,
) -> Result<Json> {
    let name = &scope.report.name;
    let location = locate(scope.report, id)?;
    let (pid, item) = match location {
        Located::Partner(pid) => (pid, None),
        Located::Item(pid, item) => (pid, Some(item)),
    };
    let mut domain = scope.base.clone() & partner_domain(pid)?;
    if let Some(item) = item {
        domain = domain & cond(json!([["id", "=", item]]))?;
    }
    match (location, target) {
        (Located::Partner(pid), DrillTarget::Item(i)) if i == "partner" && pid > 0 => Ok(json!({
            "type": "ir.actions.act_window",
            "name": "Partner",
            "res_model": "res.partner",
            "res_id": pid,
            "target": "current",
            "views": [[false, "form"]],
        })),
        (Located::Partner(_), DrillTarget::Item(i)) if i == "journal_items" => {
            scope.act_window(domain)
        }
        (Located::Item(..), DrillTarget::Item(i)) if i == "entry" => {
            let found = cx
                .env()
                .records("account.move.line")?
                .search(cx.tx(), &domain)
                .await?;
            let rows = found.read_rows(cx.tx(), &["move_id"], Load::Raw).await?;
            let row = rows.iter().next().map(Value::Row).ok_or_else(|| {
                invalid(format!(
                    "This journal item is not visible in the {name} scope."
                ))
            })?;
            Ok(json!({
                "type": "ir.actions.act_window",
                "name": "Journal Entry",
                "res_model": "account.move",
                "res_id": int_of(&row, "move_id", "the journal entry")?,
                "target": "current",
                "views": [[false, "form"]],
            }))
        }
        (_, DrillTarget::Cell(index)) => scope.act_window(domain & scope.cell_domain(*index)?),
        (_, DrillTarget::Item(i)) => Err(invalid(format!("This {name} line has no action {i}."))),
        (_, _) => Err(invalid(format!("This {name} action is not supported."))),
    }
}

impl ReportProvider for Aged {
    fn spec(&self) -> &ProviderSpec {
        &self.spec
    }
    fn init_options<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a mut Options,
        _strict: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if let Some(key) = options.provider.keys().find(|k| *k != INTERVAL) {
                return Err(invalid(format!(
                    "Invalid {} option {key}. Use {INTERVAL}.",
                    report.name
                )));
            }
            let width = interval(report, options.provider.get(INTERVAL))?;
            options
                .provider
                .insert(INTERVAL.to_owned(), Json::from(width));
            Ok(())
        })
    }
    fn lines<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move {
            let scope = self.scope(cx, report, options).await?;
            top_lines(&scope, cx).await
        })
    }
    fn expand<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move {
            locate(report, line)?;
            let scope = self.scope(cx, report, options).await?;
            expand_partner(&scope, cx, line, page).await
        })
    }
    fn drill<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(async move {
            locate(report, line)?;
            let scope = self.scope(cx, report, options).await?;
            drill_line(&scope, cx, line, target).await
        })
    }
}

mod ar_decl {
    erp_addon_reporting::provider_declarations!(super::AGED_RECEIVABLE);
}

mod ap_decl {
    erp_addon_reporting::provider_declarations!(super::AGED_PAYABLE);
}

#[cfg(test)]
mod tests {
    use super::{bucket, bucket_range};
    use erp_sdk::Date;

    #[test]
    fn buckets_and_their_ranges_agree_at_every_boundary() {
        let as_of = Date::from_ymd(2026, 11, 25).unwrap();
        for width in [1, 7, 30, 365] {
            for late in -3..=(4 * width + 3) {
                let due = Date::from_days(as_of.days() - late);
                let index = bucket(due, as_of, width);
                let (lo, hi) = bucket_range(index, as_of, width);
                assert!(lo.is_none_or(|lo| lo <= due), "{width} {late}");
                assert!(hi.is_none_or(|hi| due <= hi), "{width} {late}");
            }
        }
        assert_eq!(bucket(as_of, as_of, 30), 0);
        assert_eq!(bucket(Date::from_days(as_of.days() - 30), as_of, 30), 1);
        assert_eq!(bucket(Date::from_days(as_of.days() - 31), as_of, 30), 2);
        assert_eq!(bucket(Date::from_days(as_of.days() - 121), as_of, 30), 5);
    }
}
