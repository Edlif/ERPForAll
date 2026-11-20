// SPDX-License-Identifier: MIT OR Apache-2.0
//! Chronological source items; aggregate-only prefixes and committed-generation stale fences.
use erp_addon_reporting::{
    definition::ReportDef,
    line::{
        Cell as ReportCell, CellKind, DrillTarget, ExpandPage, Expansion, Line, LineId, LineKind,
        Segment, SegmentKind,
    },
    options::Options,
    provider::{ProviderSpec, ReportCx, ReportProvider},
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, Load, SearchOptions};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) struct GeneralLedger;
const SPEC: ProviderSpec = ProviderSpec::new("account_reporting.general_ledger", 1);
const LABELS: [&str; 5] = ["date", "entry", "debit", "credit", "balance"];
const EPOCH_KEYS: [&str; 4] = [
    "registry",
    "policy",
    "membership",
    "cache:account_reporting.ledger",
];
const STALE: &str =
    "The ledger source or access changed. Refresh the General Ledger before continuing.";
type Row = Value;
fn get(row: &Row, field: &str) -> Result<Value> {
    row.dict()
        .ok_or_else(|| Error::internal("General Ledger row is not a batch handle."))?
        .get(field)
}

fn invalid(message: impl Into<String>) -> Error {
    Error::validation(message.into())
}
fn day(date: Date) -> String {
    let (y, m, d) = date.ymd();
    format!("{y:04}-{m:02}-{d:02}")
}
fn condition(value: Json) -> Result<Domain> {
    Ok(Domain::from_json(&value)?)
}
fn id_of(text: &str) -> Result<i64> {
    let id = text
        .parse::<i64>()
        .map_err(|_| invalid("A General Ledger line has an invalid record id."))?;
    if id <= 0 {
        return Err(invalid("A General Ledger line has an invalid record id."));
    }
    Ok(id)
}
fn num(value: Option<&GroupValue>) -> Result<Decimal> {
    match value {
        // An authorized SQL SUM over no contributing records is zero.
        None | Some(GroupValue::Null | GroupValue::False) => Ok(Decimal::ZERO),
        Some(GroupValue::Decimal(n)) => Ok(*n),
        Some(GroupValue::Int(n)) => Ok(Decimal::from(*n)),
        Some(other) => Err(Error::internal(format!(
            "General Ledger amount is not exact decimal: {other:?}"
        ))),
    }
}
fn number(row: &Row, field: &str) -> Result<Decimal> {
    match get(row, field)? {
        Value::Decimal(n) => Ok(n),
        other => Err(Error::internal(format!(
            "General Ledger {field} is not decimal: {other:?}"
        ))),
    }
}
fn integer(row: &Row, field: &str) -> Result<i64> {
    match get(row, field)? {
        Value::Int(n) => Ok(n),
        other => Err(Error::internal(format!(
            "General Ledger {field} is not an id: {other:?}"
        ))),
    }
}
fn date(row: &Row) -> Result<Date> {
    match get(row, "date")? {
        Value::Date(d) => Ok(d),
        other => Err(Error::internal(format!(
            "General Ledger item has no date: {other:?}"
        ))),
    }
}
fn text(row: &Row, field: &str) -> Result<String> {
    match get(row, field)? {
        Value::Str(s) => Ok(s.to_string()),
        Value::False => Ok(String::new()),
        other => Err(Error::internal(format!(
            "General Ledger {field} is not text: {other:?}"
        ))),
    }
}
fn base(options: &Options) -> Result<Domain> {
    if options.companies.len() != 1
        || options.column_groups.len() != 1
        || options.period.from.is_none()
    {
        return Err(invalid(
            "General Ledger requires one company and one date range without comparisons.",
        ));
    }
    condition(json!([
        ["company_id", "=", options.companies[0]],
        ["parent_state", "=", "posted"],
        ["account_id", "!=", false],
        ["journal_id.journal_group_id", "=", false],
        ["date", "<=", day(options.period.to)]
    ]))
}
fn period(options: &Options) -> Result<Domain> {
    let from = options
        .period
        .from
        .ok_or_else(|| invalid("General Ledger requires a date range."))?;
    Ok(base(options)? & condition(json!([["date", ">=", day(from)]]))?)
}
fn account(domain: Domain, id: i64) -> Result<Domain> {
    Ok(domain & condition(json!([["account_id", "=", id]]))?)
}
async fn sums(cx: &mut ReportCx<'_>, domain: Domain) -> Result<[Decimal; 3]> {
    let records = cx.env().records("account.move.line")?;
    let values = records
        ._read_group(
            cx.tx(),
            ReadGroup::new(domain).aggregates(["debit:sum", "credit:sum", "balance:sum"]),
        )
        .await?;
    Ok([
        num(values.column(0).first())?,
        num(values.column(1).first())?,
        num(values.column(2).first())?,
    ])
}
async fn epoch(cx: &mut ReportCx<'_>, options: &Options) -> Result<Json> {
    let generations = cx.tx().signal_generations().await?;
    // Signal generations are zero when a key has never been bumped (the signal API contract).
    let mut values = [0_u64; 4];
    for (key, generation) in generations.iter() {
        if let Some(index) = EPOCH_KEYS
            .iter()
            .position(|name| *name == key.to_key_string())
        {
            values[index] = generation;
        }
    }
    // A freshness token, never an authorization grant. Hide raw global activity counters,
    // and bind it to the principal, context and complete company/date/report scope.
    let source = format!(
        "{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{:?}|{:?}|{:?}",
        values,
        cx.env().principal().actor(),
        cx.env().principal().scope(),
        cx.env().principal().elevation(),
        cx.env().uid(),
        options.report,
        SPEC.version,
        options.companies,
        (options.period, cx.env().context()),
        cx.env().context_today()?
    );
    let digest = Sha256::digest(source.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    use std::fmt::Write;
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| Error::internal("Cannot encode the General Ledger freshness token."))?;
    }
    Ok(json!(encoded))
}
async fn fresh(cx: &mut ReportCx<'_>, options: &Options) -> Result<()> {
    if options.provider.get("epoch") != Some(&epoch(cx, options).await?) {
        return Err(invalid(STALE));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Located {
    Root,
    Account(i64),
    Opening(i64),
    Closing(i64),
    Item(i64, i64),
}
fn locate(report: &ReportDef, id: &LineId) -> Result<Located> {
    if id.report() != report.id {
        return Err(invalid("This line is not part of this General Ledger."));
    }
    let segments = id.segments();
    let bad = || invalid("This is not a General Ledger line.");
    if !segments.get(1).is_some_and(|s| {
        matches!(s.kind(),SegmentKind::Line(code) if code=="GL")
            && s.model().is_empty()
            && s.value().is_empty()
    }) {
        return Err(bad());
    }
    if segments.len() == 2 {
        return Ok(Located::Root);
    }
    let group = &segments[2];
    if !matches!(group.kind(),SegmentKind::Group(field) if field=="account_id")
        || group.model() != "account.account"
    {
        return Err(bad());
    }
    let account = id_of(group.value())?;
    if segments.len() == 3 {
        return Ok(Located::Account(account));
    }
    if segments.len() != 4 {
        return Err(bad());
    }
    let last = &segments[3];
    match last.kind() {
        SegmentKind::Line(code)
            if last.model().is_empty() && last.value().is_empty() && code == "OPENING" =>
        {
            Ok(Located::Opening(account))
        }
        SegmentKind::Line(code)
            if last.model().is_empty() && last.value().is_empty() && code == "CLOSING" =>
        {
            Ok(Located::Closing(account))
        }
        SegmentKind::Group(field) if field == "id" && last.model() == "account.move.line" => {
            Ok(Located::Item(account, id_of(last.value())?))
        }
        _ => Err(bad()),
    }
}
struct Layout {
    scale: u32,
    currency: i64,
    names: BTreeMap<String, String>,
}
impl Layout {
    async fn load(cx: &mut ReportCx<'_>, report: &ReportDef, options: &Options) -> Result<Self> {
        if report.columns.len() != LABELS.len()
            || report.columns.iter().any(|c| {
                !matches!(
                    (c.label.as_str(), c.kind),
                    ("date", CellKind::Date)
                        | ("entry", CellKind::Text)
                        | ("debit" | "credit" | "balance", CellKind::Monetary)
                )
            })
            || LABELS
                .iter()
                .any(|label| !report.columns.iter().any(|c| c.label == *label))
        {
            return Err(invalid(
                "General Ledger columns must be date, entry, debit, credit and balance; their names and order are editable.",
            ));
        }
        let env = cx.env().clone();
        let company = env
            .records("res.company")?
            .browse([options.companies[0]])?
            .read_rows(cx.tx(), &["currency_id"], Load::Raw)
            .await?;
        let row = company
            .iter()
            .next()
            .map(Value::Row)
            .ok_or_else(|| invalid("The selected General Ledger company is not visible."))?;
        let currency = integer(&row, "currency_id")?;
        let rows = env
            .records("res.currency")?
            .browse([currency])?
            .read_rows(cx.tx(), &["decimal_places"], Load::Raw)
            .await?;
        let row = rows
            .iter()
            .next()
            .map(Value::Row)
            .ok_or_else(|| invalid("The General Ledger currency is not visible."))?;
        let scale = u32::try_from(integer(&row, "decimal_places")?)
            .map_err(|_| invalid("The General Ledger currency precision is invalid."))?;
        let records = env
            .records("reporting.report.line")?
            .search_with(
                cx.tx(),
                &condition(json!([["report_id", "=", report.id]]))?,
                &SearchOptions::new().limit(4),
            )
            .await?;
        let mut names = BTreeMap::new();
        for row in records
            .read_rows(cx.tx(), &["code", "name"], Load::Raw)
            .await?
            .iter()
            .map(Value::Row)
        {
            let code = text(&row, "code")?;
            if !["GL", "OPENING", "CLOSING"].contains(&code.as_str())
                || names.insert(code, text(&row, "name")?).is_some()
            {
                return Err(invalid(
                    "General Ledger lines must have the unique codes GL, OPENING and CLOSING; their names are editable.",
                ));
            }
        }
        if names.len() != 3 {
            return Err(invalid(
                "General Ledger needs the GL, OPENING and CLOSING line definitions.",
            ));
        }
        Ok(Self {
            scale,
            currency,
            names,
        })
    }
    fn name(&self, code: &str) -> Result<&str> {
        self.names
            .get(code)
            .map(String::as_str)
            .ok_or_else(|| Error::internal("A checked General Ledger label is absent."))
    }
    fn cells(
        &self,
        report: &ReportDef,
        date: Option<Date>,
        entry: &str,
        amounts: [Option<Decimal>; 3],
    ) -> Result<Vec<ReportCell>> {
        report
            .columns
            .iter()
            .map(|c| {
                let cell = match c.label.as_str() {
                    "date" => date.map_or_else(ReportCell::empty, ReportCell::date),
                    "entry" => ReportCell::text(entry),
                    "debit" => ReportCell::monetary(amounts[0], self.scale, Some(self.currency)),
                    "credit" => ReportCell::monetary(amounts[1], self.scale, Some(self.currency)),
                    "balance" => ReportCell::monetary(amounts[2], self.scale, Some(self.currency)),
                    _ => {
                        return Err(Error::internal(
                            "An unchecked General Ledger column was used.",
                        ));
                    }
                };
                Ok(if c.blank_if_zero {
                    cell.blank_if_zero()
                } else {
                    cell
                })
            })
            .collect()
    }
}
fn money(values: [Decimal; 3]) -> [Option<Decimal>; 3] {
    values.map(Some)
}
async fn ensure_account(cx: &mut ReportCx<'_>, options: &Options, id: i64) -> Result<()> {
    let records = cx.env().records("account.move.line")?;
    if records
        .search_count(cx.tx(), &account(base(options)?, id)?, Some(1))
        .await?
        == 0
    {
        return Err(invalid(
            "This account has no visible posted items in the General Ledger scope.",
        ));
    }
    Ok(())
}
fn before_key(
    options: &Options,
    account_id: i64,
    date: Date,
    id: i64,
    inclusive: bool,
) -> Result<Domain> {
    let comparison = if inclusive { "<=" } else { "<" };
    Ok(account(base(options)?, account_id)?
        & condition(json!([
            "|",
            ["date", "<", day(date)],
            "&",
            ["date", "=", day(date)],
            ["id", comparison, id]
        ]))?)
}

async fn top(cx: &mut ReportCx<'_>, report: &ReportDef, options: &Options) -> Result<Expansion> {
    fresh(cx, options).await?;
    let layout = Layout::load(cx, report, options).await?;
    let mut amounts = sums(cx, period(options)?).await?;
    amounts[2] = sums(cx, base(options)?).await?[2];
    let mut line = Line::new(
        LineId::root(report.id).child(Segment::line("GL")),
        layout.name("GL")?,
        LineKind::Section,
        layout.cells(report, None, "", money(amounts))?,
    );
    line.unfoldable = true;
    fresh(cx, options).await?;
    Ok(Expansion::new(vec![line]))
}

async fn expand(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
    id: &LineId,
    page: ExpandPage,
) -> Result<Expansion> {
    let location = locate(report, id)?;
    let end = page
        .offset
        .checked_add(page.limit)
        .filter(|n| *n <= cx.bounds().max_screen_lines as u64)
        .ok_or_else(|| {
            invalid("General Ledger paging exceeds reporting.max_screen_lines; narrow the scope.")
        })?;
    fresh(cx, options).await?;
    let layout = Layout::load(cx, report, options).await?;
    let env = cx.env().clone();
    let records = env.records("account.move.line")?;
    let mut lines = Vec::new();
    match location {
        Located::Root => {
            let groups = records
                ._read_group(
                    cx.tx(),
                    ReadGroup::new(base(options)?)
                        .groupby(["account_id"])
                        .aggregates(["balance:sum"])
                        .limit(end)
                        .count_groups(true),
                )
                .await?;
            if groups.is_empty() {
                fresh(cx, options).await?;
                return Ok(Expansion::new(lines));
            }
            let total = groups
                .group_count()
                .ok_or_else(|| Error::internal("General Ledger account paging lacks its count."))?;
            let mut loaded = Vec::new();
            for row in groups.rows() {
                let key = row
                    .first()
                    .and_then(GroupValue::record_id)
                    .ok_or_else(|| Error::internal("General Ledger account group has no id."))?
                    .get();
                loaded.push((key, num(row.get(1))?));
            }
            let offset = usize::try_from(page.offset)
                .map_err(|_| invalid("General Ledger page offset is too large."))?;
            let keys = loaded.get(offset..).ok_or_else(|| {
                invalid("General Ledger account page offset is past its results.")
            })?;
            let ids: Vec<i64> = keys.iter().map(|(key, _)| *key).collect();
            let names = env
                .records("account.account")?
                .browse(ids.iter().copied())?
                .values(cx.tx(), "display_name")
                .await?;
            let period_groups = records
                ._read_group(
                    cx.tx(),
                    ReadGroup::new(
                        period(options)? & condition(json!([["account_id", "in", ids]]))?,
                    )
                    .groupby(["account_id"])
                    .aggregates(["debit:sum", "credit:sum"]),
                )
                .await?;
            let mut amounts = BTreeMap::new();
            for row in period_groups.rows() {
                let key = row
                    .first()
                    .and_then(GroupValue::record_id)
                    .ok_or_else(|| Error::internal("General Ledger period group has no id."))?
                    .get();
                amounts.insert(key, [num(row.get(1))?, num(row.get(2))?]);
            }
            for ((key, closing), name) in keys.iter().zip(names) {
                let Value::Str(name) = name else {
                    return Err(Error::internal(
                        "General Ledger account has no display name.",
                    ));
                };
                // An account with only opening items has no period group, and zero movement.
                let [debit, credit] = amounts.get(key).copied().unwrap_or([Decimal::ZERO; 2]);
                let mut line = Line::new(
                    id.child(Segment::group(
                        "account_id",
                        "account.account",
                        &key.to_string(),
                    )),
                    name.to_string(),
                    LineKind::Group,
                    layout.cells(report, None, "", money([debit, credit, *closing]))?,
                );
                line.unfoldable = true;
                lines.push(line);
            }
            if total > end && !keys.is_empty() {
                let seen: Vec<i64> = loaded.iter().map(|(key, _)| *key).collect();
                let omitted = condition(json!([["account_id", "not in", seen]]))?;
                let mut remaining = sums(cx, period(options)? & omitted.clone()).await?;
                remaining[2] = sums(cx, base(options)? & omitted).await?[2];
                let mut more = Line::new(
                    id.child(Segment::more()),
                    "",
                    LineKind::More,
                    layout.cells(report, None, "", money(remaining))?,
                );
                more.remaining = Some(total - end);
                more.next_offset = Some(end);
                lines.push(more);
            }
        }
        Located::Account(account_id) => {
            ensure_account(cx, options, account_id).await?;
            let from = options
                .period
                .from
                .ok_or_else(|| invalid("General Ledger needs a start date."))?;
            let opening = sums(
                cx,
                account(base(options)?, account_id)?
                    & condition(json!([["date", "<", day(from)]]))?,
            )
            .await?[2];
            if page.offset == 0 {
                lines.push(Line::new(
                    id.child(Segment::line("OPENING")),
                    layout.name("OPENING")?,
                    LineKind::Detail,
                    layout.cells(report, None, "", [None, None, Some(opening)])?,
                ));
            }
            let domain = account(period(options)?, account_id)?;
            let count = records.search_count(cx.tx(), &domain, None).await?;
            let found = records
                .search_with(
                    cx.tx(),
                    &domain,
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
                        "date",
                        "move_name",
                        "name",
                        "debit",
                        "credit",
                        "balance",
                    ],
                    Load::Raw,
                )
                .await?;
            let mut running = match rows.iter().next().map(Value::Row) {
                Some(row) => {
                    sums(
                        cx,
                        before_key(
                            options,
                            account_id,
                            date(&row)?,
                            integer(&row, "id")?,
                            false,
                        )?,
                    )
                    .await?[2]
                }
                None => opening,
            };
            for row in rows.iter().map(Value::Row) {
                let item_id = integer(&row, "id")?;
                running = running
                    .checked_add(number(&row, "balance")?)
                    .ok_or_else(|| invalid("General Ledger running balance is too large."))?;
                let entry = text(&row, "move_name")?;
                let description = text(&row, "name")?;
                let name = if description.is_empty() {
                    entry.clone()
                } else {
                    description
                };
                let mut line = Line::new(
                    id.child(Segment::group(
                        "id",
                        "account.move.line",
                        &item_id.to_string(),
                    )),
                    name,
                    LineKind::Detail,
                    layout.cells(
                        report,
                        Some(date(&row)?),
                        &entry,
                        money([number(&row, "debit")?, number(&row, "credit")?, running]),
                    )?,
                );
                line.caret = vec!["open"];
                lines.push(line);
            }
            if count > end {
                let mut more = Line::new(
                    id.child(Segment::more()),
                    "",
                    LineKind::More,
                    layout.cells(report, None, "", [None; 3])?,
                );
                more.remaining = Some(count - end);
                more.next_offset = Some(end);
                lines.push(more);
            } else {
                let mut closing = sums(cx, domain).await?;
                closing[2] = sums(cx, account(base(options)?, account_id)?).await?[2];
                lines.push(Line::new(
                    id.child(Segment::line("CLOSING")),
                    layout.name("CLOSING")?,
                    LineKind::Detail,
                    layout.cells(report, None, "", money(closing))?,
                ));
            }
        }
        _ => return Err(invalid("This General Ledger line has no children.")),
    }
    fresh(cx, options).await?;
    Ok(Expansion::new(lines))
}

async fn drill(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
    id: &LineId,
    target: &DrillTarget,
) -> Result<Json> {
    let location = locate(report, id)?;
    fresh(cx, options).await?;
    let env = cx.env().clone();
    let base = base(options)?;
    let mut model = "account.move.line";
    let mut form = None;
    let domain = match (location, target) {
        (Located::Item(account_id, item_id), target) => {
            ensure_account(cx, options, account_id).await?;
            let items = env
                .records(model)?
                .search(
                    cx.tx(),
                    &(account(period(options)?, account_id)?
                        & condition(json!([["id", "=", item_id]]))?),
                )
                .await?;
            let rows = items.read_rows(cx.tx(), &["id", "date"], Load::Raw).await?;
            let row = rows.iter().next().map(Value::Row).ok_or_else(|| {
                invalid("This journal item is not visible in the General Ledger scope.")
            })?;
            match target {
                DrillTarget::Item(item) if item == "open" => {
                    form = Some(item_id);
                    condition(json!([["id", "=", item_id]]))?
                }
                DrillTarget::Cell(index) => {
                    let label = report
                        .columns
                        .get(*index)
                        .ok_or_else(|| invalid("General Ledger has no such column."))?
                        .label
                        .as_str();
                    match label {
                        "balance" => before_key(options, account_id, date(&row)?, item_id, true)?,
                        "date" | "debit" | "credit" => {
                            account(period(options)?, account_id)?
                                & condition(json!([["id", "=", item_id]]))?
                        }
                        _ => {
                            return Err(invalid(
                                "This General Ledger cell has no amount to drill.",
                            ));
                        }
                    }
                }
                _ => return Err(invalid("This General Ledger action is not supported.")),
            }
        }
        (Located::Root, DrillTarget::Cell(index))
        | (Located::Account(_), DrillTarget::Cell(index))
        | (Located::Opening(_), DrillTarget::Cell(index))
        | (Located::Closing(_), DrillTarget::Cell(index)) => {
            let label = report
                .columns
                .get(*index)
                .ok_or_else(|| invalid("General Ledger has no such column."))?
                .label
                .as_str();
            let d = match (location, label) {
                (Located::Opening(_), "balance") => {
                    base & condition(json!([[
                        "date",
                        "<",
                        day(options
                            .period
                            .from
                            .ok_or_else(|| invalid("General Ledger needs a start date."))?)
                    ]]))?
                }
                (Located::Root | Located::Account(_) | Located::Closing(_), "balance") => base,
                (Located::Root | Located::Account(_) | Located::Closing(_), "debit" | "credit") => {
                    period(options)?
                }
                _ => return Err(invalid("This General Ledger cell has no amount to drill.")),
            };
            match location {
                Located::Account(a) | Located::Opening(a) | Located::Closing(a) => {
                    ensure_account(cx, options, a).await?;
                    account(d, a)?
                }
                _ => d,
            }
        }
        (Located::Account(a), DrillTarget::Item(item)) if item == "open" => {
            ensure_account(cx, options, a).await?;
            model = "account.account";
            form = Some(a);
            condition(json!([["id", "=", a]]))?
        }
        _ => return Err(invalid("This General Ledger action is not supported.")),
    };
    fresh(cx, options).await?;
    Ok(match form {
        Some(record) => {
            json!({"type":"ir.actions.act_window","name":report.name,"res_model":model,"res_id":record,"target":"new","views":[[false,"form"]]})
        }
        None => {
            json!({"type":"ir.actions.act_window","name":report.name,"res_model":model,"target":"current","views":[[false,"list"],[false,"form"]],"domain":domain.to_json()?,"context":{}})
        }
    })
}

impl ReportProvider for GeneralLedger {
    fn spec(&self) -> &ProviderSpec {
        &SPEC
    }
    fn init_options<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a mut Options,
        strict: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let source = base(options)?;
            if options.order.is_some() {
                return Err(invalid(
                    "General Ledger rows use posting date and immutable id order; custom sorting is not supported until a later release.",
                ));
            }
            if !options.search.is_empty() {
                return Err(invalid(
                    "General Ledger transaction search is not supported until a later release.",
                ));
            }
            if options.provider.keys().any(|key| key != "epoch") {
                return Err(invalid(
                    "General Ledger provider options only support epoch.",
                ));
            }
            for line in &options.unfolded {
                if !matches!(locate(report, line)?, Located::Root | Located::Account(_)) {
                    return Err(invalid(
                        "Only General Ledger account parents can be unfolded.",
                    ));
                }
            }
            for line in options.loaded_pages.keys() {
                if !matches!(
                    locate(report, &LineId::parse(line)?)?,
                    Located::Root | Located::Account(_)
                ) {
                    return Err(invalid(
                        "Only General Ledger account parents can have loaded pages.",
                    ));
                }
            }
            // Authorization is checked even for an empty ledger, before returning any epoch.
            let records = cx.env().records("account.move.line")?;
            records
                ._read_group(cx.tx(), ReadGroup::new(source).aggregates(["__count"]))
                .await?;
            Layout::load(cx, report, options).await?;
            let current = epoch(cx, options).await?;
            if strict {
                let given = options.provider.get("epoch").ok_or_else(|| {
                    invalid("General Ledger requires a ledger epoch; refresh the report options.")
                })?;
                if !given.as_str().is_some_and(|value| {
                    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
                }) {
                    return Err(invalid(
                        "General Ledger epoch must be a SHA-256 freshness token.",
                    ));
                }
                if given != &current {
                    return Err(invalid(STALE));
                }
            } else {
                options.provider.insert("epoch".into(), current);
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
        Box::pin(top(cx, report, options))
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
