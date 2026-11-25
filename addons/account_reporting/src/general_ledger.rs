// SPDX-License-Identifier: MIT OR Apache-2.0
//! General Ledger dynamic report provider (`account_reporting.general_ledger`):
//! accounts (code + name, ordered by code) -> chronological journal items with running balance,
//! optional "Initial balance" when non-zero, engine `totals_below` subtotals, grand total row,
//! fiscal-year opening mode, and committed-generation stale fences.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_account::fiscal::settings::FiscalCalendar;
use erp_addon_account::ledger::code::codes_by_account;
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
use erp_sdk::{BoxFuture, ErrorKind, Load, SearchOptions};
use sha2::{Digest, Sha256};

pub(crate) struct GeneralLedger;
const SPEC: ProviderSpec = ProviderSpec::new("account_reporting.general_ledger", 2);
const REQUIRED_LABELS: [&str; 5] = ["date", "entry", "debit", "credit", "balance"];
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

fn opt_integer(row: &Row, field: &str) -> Result<Option<i64>> {
    match get(row, field)? {
        Value::Int(n) if n > 0 => Ok(Some(n)),
        Value::False | Value::Int(_) => Ok(None),
        other => Err(Error::internal(format!(
            "General Ledger {field} is not an optional id: {other:?}"
        ))),
    }
}

fn date_of(row: &Row) -> Result<Date> {
    match get(row, "date")? {
        Value::Date(d) => Ok(d),
        other => Err(Error::internal(format!(
            "General Ledger item has no date: {other:?}"
        ))),
    }
}

fn text_of(row: &Row, field: &str) -> Result<String> {
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

fn fiscal_start(options: &Options) -> Result<Option<Date>> {
    match options.provider.get("fiscal_year_start") {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .and_then(Date::parse_odoo)
            .map(Some)
            .ok_or_else(|| invalid("General Ledger provider.fiscal_year_start must be a date.")),
    }
}

async fn resolve_fiscal_start(cx: &mut ReportCx<'_>, options: &Options) -> Result<Option<Date>> {
    match options.provider.get("fiscal_opening") {
        None | Some(Json::Bool(false)) => return Ok(None),
        Some(Json::Bool(true)) => (),
        Some(_) => {
            return Err(invalid(
                "General Ledger provider.fiscal_opening must be a boolean.",
            ));
        }
    }
    let from = options
        .period
        .from
        .ok_or_else(|| invalid("General Ledger requires a date range."))?;
    let company = cx
        .env()
        .records("res.company")?
        .browse([options.companies[0]])?;
    let Cell::Id(root) = company.cell(cx.tx(), "root_id").await? else {
        return Err(invalid(
            "The selected General Ledger company has no fiscal calendar root.",
        ));
    };
    let calendar = FiscalCalendar::read(cx.tx(), root).await.map_err(|error| {
        if root != options.companies[0] && error.kind() == ErrorKind::Access {
            Error::access("General Ledger fiscal opening requires access to the branch's root company calendar.")
        } else {
            error
        }
    })?;
    let year = calendar.year_end().fiscal_year_of(from)?;
    if options.period.to > year.date_to() {
        return Err(invalid(
            "General Ledger fiscal opening requires a range within one company fiscal year.",
        ));
    }
    Ok(Some(year.date_from()))
}

fn history(options: &Options) -> Result<Domain> {
    let all = base(options)?;
    match fiscal_start(options)? {
        Some(start) => Ok(all
            & condition(json!([
                "|",
                ["account_id.include_initial_balance", "=", true],
                ["date", ">=", day(start)]
            ]))?),
        None => Ok(all),
    }
}

fn unallocated(options: &Options) -> Result<Option<Domain>> {
    fiscal_start(options)?
        .map(|start| {
            Ok(base(options)?
                & condition(json!([
                    ["account_id.include_initial_balance", "=", false],
                    ["date", "<", day(start)]
                ]))?)
        })
        .transpose()
}

fn account_dom(domain: Domain, id: i64) -> Result<Domain> {
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
    let mut values = [0_u64; 4];
    for (key, generation) in generations.iter() {
        if let Some(index) = EPOCH_KEYS
            .iter()
            .position(|name| *name == key.to_key_string())
        {
            values[index] = generation;
        }
    }
    let mut source = format!(
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
    if let Some(start) = fiscal_start(options)? {
        source.push('|');
        source.push_str(&day(start));
    }
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
    Unallocated,
    Account(i64),
    Opening(i64),
    Item(i64, i64),
}

fn locate(report: &ReportDef, id: &LineId) -> Result<Located> {
    if id.report() != report.id {
        return Err(invalid("This line is not part of this General Ledger."));
    }
    let segments = id.segments();
    let bad = || invalid("This is not a General Ledger line.");
    let Some(first) = segments.get(1) else {
        return Err(bad());
    };
    if segments.len() == 2
        && matches!(first.kind(), SegmentKind::Line(code) if code == "UNALLOCATED")
        && first.model().is_empty()
        && first.value().is_empty()
    {
        return Ok(Located::Unallocated);
    }
    if !matches!(first.kind(), SegmentKind::Group(field) if field == "account_id")
        || first.model() != "account.account"
    {
        return Err(bad());
    }
    let account_id = id_of(first.value())?;
    if segments.len() == 2 {
        return Ok(Located::Account(account_id));
    }
    if segments.len() != 3 {
        return Err(bad());
    }
    let last = &segments[2];
    match last.kind() {
        SegmentKind::Line(code)
            if last.model().is_empty() && last.value().is_empty() && code == "OPENING" =>
        {
            Ok(Located::Opening(account_id))
        }
        SegmentKind::Group(field) if field == "id" && last.model() == "account.move.line" => {
            Ok(Located::Item(account_id, id_of(last.value())?))
        }
        _ => Err(bad()),
    }
}

struct Layout {
    scale: u32,
    currency: i64,
    root_company: i64,
}

impl Layout {
    async fn load(cx: &mut ReportCx<'_>, report: &ReportDef, options: &Options) -> Result<Self> {
        if report.columns.iter().any(|c| {
            !matches!(
                (c.label.as_str(), c.kind),
                ("date", CellKind::Date)
                    | ("entry" | "partner", CellKind::Text)
                    | ("debit" | "credit" | "balance", CellKind::Monetary)
            )
        }) || REQUIRED_LABELS
            .iter()
            .any(|label| !report.columns.iter().any(|c| c.label == *label))
        {
            return Err(invalid(
                "General Ledger columns must include date, entry, debit, credit and balance; partner is optional.",
            ));
        }
        let env = cx.env().clone();
        let company = env
            .records("res.company")?
            .browse([options.companies[0]])?
            .read_rows(cx.tx(), &["currency_id", "root_id"], Load::Raw)
            .await?;
        let row = company
            .iter()
            .next()
            .map(Value::Row)
            .ok_or_else(|| invalid("The selected General Ledger company is not visible."))?;
        let currency = integer(&row, "currency_id")?;
        let root_company = opt_integer(&row, "root_id")?.unwrap_or(options.companies[0]);
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
        Ok(Self {
            scale,
            currency,
            root_company,
        })
    }

    fn cells(
        &self,
        report: &ReportDef,
        date: Option<Date>,
        entry: &str,
        partner: &str,
        amounts: [Option<Decimal>; 3],
        drillable: bool,
    ) -> Result<Vec<ReportCell>> {
        report
            .columns
            .iter()
            .map(|c| {
                let cell = match c.label.as_str() {
                    "date" => date.map_or_else(ReportCell::empty, ReportCell::date),
                    "entry" => {
                        if entry.is_empty() {
                            ReportCell::empty()
                        } else {
                            ReportCell::text(entry)
                        }
                    }
                    "partner" => {
                        if partner.is_empty() {
                            ReportCell::empty()
                        } else {
                            ReportCell::text(partner)
                        }
                    }
                    "debit" => ReportCell::monetary(amounts[0], self.scale, Some(self.currency))
                        .with_drillable(drillable),
                    "credit" => ReportCell::monetary(amounts[1], self.scale, Some(self.currency))
                        .with_drillable(drillable),
                    "balance" => ReportCell::monetary(amounts[2], self.scale, Some(self.currency))
                        .with_drillable(drillable),
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
        .search_count(cx.tx(), &account_dom(history(options)?, id)?, Some(1))
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
    Ok(account_dom(history(options)?, account_id)?
        & condition(json!([
            "|",
            ["date", "<", day(date)],
            "&",
            ["date", "=", day(date)],
            ["id", comparison, id]
        ]))?)
}

/// Top level: directly lists accounts ordered by code + name (no wrapper line),
/// optional unallocated prior-year earnings under fiscal opening, and a grand Total row.
async fn top(cx: &mut ReportCx<'_>, report: &ReportDef, options: &Options) -> Result<Expansion> {
    fresh(cx, options).await?;
    let layout = Layout::load(cx, report, options).await?;
    let env = cx.env().clone();
    let records = env.records("account.move.line")?;
    let root = LineId::root(report.id);

    let mut lines = Vec::new();
    let mut unallocated_bal = Decimal::ZERO;
    if let Some(domain) = unallocated(options)? {
        let balance = sums(cx, domain).await?[2];
        if balance != Decimal::ZERO {
            unallocated_bal = balance;
            lines.push(Line::new(
                root.child(Segment::line("UNALLOCATED")),
                "Unallocated prior-year earnings",
                LineKind::Detail,
                layout.cells(report, None, "", "", [None, None, Some(balance)], true)?,
            ));
        }
    }

    let groups = records
        ._read_group(
            cx.tx(),
            ReadGroup::new(history(options)?)
                .groupby(["account_id"])
                .aggregates(["balance:sum"]),
        )
        .await?;

    if groups.is_empty() && lines.is_empty() {
        fresh(cx, options).await?;
        return Ok(Expansion::new(Vec::new()));
    }

    let mut account_closing: Vec<(i64, Decimal)> = Vec::with_capacity(groups.len());
    for row in groups.rows() {
        let key = row
            .first()
            .and_then(GroupValue::record_id)
            .ok_or_else(|| Error::internal("General Ledger account group has no id."))?
            .get();
        account_closing.push((key, num(row.get(1))?));
    }

    let ids: Vec<i64> = account_closing.iter().map(|(k, _)| *k).collect();
    let account_codes = codes_by_account(cx.tx(), &env, &ids, layout.root_company).await?;
    let mut account_names: BTreeMap<i64, String> = BTreeMap::new();
    if !ids.is_empty() {
        let arows = env
            .records("account.account")?
            .browse(ids.iter().copied())?
            .read_rows(cx.tx(), &["id", "name"], Load::Raw)
            .await?;
        for r in arows.iter().map(Value::Row) {
            account_names.insert(integer(&r, "id")?, text_of(&r, "name")?);
        }
    }

    let period_groups = if ids.is_empty() {
        Vec::new()
    } else {
        records
            ._read_group(
                cx.tx(),
                ReadGroup::new(period(options)? & condition(json!([["account_id", "in", ids]]))?)
                    .groupby(["account_id"])
                    .aggregates(["debit:sum", "credit:sum"]),
            )
            .await?
            .rows()
            .into_iter()
            .map(|row| {
                let key = row
                    .first()
                    .and_then(GroupValue::record_id)
                    .ok_or_else(|| Error::internal("General Ledger period group has no id."))?
                    .get();
                Ok((key, [num(row.get(1))?, num(row.get(2))?]))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let period_map: BTreeMap<i64, [Decimal; 2]> = period_groups.into_iter().collect();

    let mut ordered: Vec<(String, String, i64, [Decimal; 3])> =
        Vec::with_capacity(account_closing.len());
    let mut grand_debit = Decimal::ZERO;
    let mut grand_credit = Decimal::ZERO;
    let mut grand_balance = unallocated_bal;

    for (aid, closing) in account_closing {
        let [debit, credit] = period_map.get(&aid).copied().unwrap_or([Decimal::ZERO; 2]);
        let code = account_codes.get(&aid).cloned().unwrap_or_default();
        let raw_name = account_names
            .get(&aid)
            .cloned()
            .unwrap_or_else(|| format!("Account #{aid}"));
        let label = if code.is_empty() {
            raw_name.clone()
        } else {
            format!("{code} {raw_name}")
        };
        grand_debit += debit;
        grand_credit += credit;
        grand_balance += closing;
        ordered.push((code, label, aid, [debit, credit, closing]));
    }
    ordered.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    for (_, label, aid, amounts) in ordered {
        let mut line = Line::new(
            root.child(Segment::group(
                "account_id",
                "account.account",
                &aid.to_string(),
            )),
            label,
            LineKind::Group,
            layout.cells(report, None, "", "", money(amounts), true)?,
        );
        line.unfoldable = true;
        line.caret = vec!["journal_items", "open"];
        lines.push(line);
    }

    lines.push(Line::new(
        root.child(Segment::total()),
        "Total General Ledger",
        LineKind::Total,
        layout.cells(
            report,
            None,
            "",
            "",
            money([grand_debit, grand_credit, grand_balance]),
            false,
        )?,
    ));

    fresh(cx, options).await?;
    Ok(Expansion::new(lines))
}

async fn expand(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
    id: &LineId,
    page: ExpandPage,
) -> Result<Expansion> {
    let Located::Account(account_id) = locate(report, id)? else {
        return Err(invalid("This General Ledger line has no children."));
    };
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

    ensure_account(cx, options, account_id).await?;
    let from = options
        .period
        .from
        .ok_or_else(|| invalid("General Ledger needs a start date."))?;
    let opening = sums(
        cx,
        account_dom(history(options)?, account_id)? & condition(json!([["date", "<", day(from)]]))?,
    )
    .await?[2];

    let mut lines = Vec::new();
    if page.offset == 0 && !opening.is_zero() {
        lines.push(Line::new(
            id.child(Segment::line("OPENING")),
            "Initial balance",
            LineKind::Detail,
            layout.cells(report, None, "", "", [None, None, Some(opening)], true)?,
        ));
    }

    let domain = account_dom(period(options)?, account_id)?;
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
                "partner_id",
                "debit",
                "credit",
                "balance",
            ],
            Load::Raw,
        )
        .await?;

    let mut partner_ids = BTreeSet::new();
    for row in rows.iter().map(Value::Row) {
        if let Some(pid) = opt_integer(&row, "partner_id")? {
            partner_ids.insert(pid);
        }
    }
    let mut partner_names: BTreeMap<i64, String> = BTreeMap::new();
    if !partner_ids.is_empty() {
        let prows = env
            .records("res.partner")?
            .browse(partner_ids.iter().copied())?
            .read_rows(cx.tx(), &["id", "name"], Load::Raw)
            .await?;
        for r in prows.iter().map(Value::Row) {
            partner_names.insert(integer(&r, "id")?, text_of(&r, "name")?);
        }
    }

    let mut running = match rows.iter().next().map(Value::Row) {
        Some(row) => {
            sums(
                cx,
                before_key(
                    options,
                    account_id,
                    date_of(&row)?,
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
        let entry = text_of(&row, "move_name")?;
        let description = text_of(&row, "name")?;
        let name = if description.is_empty() || description == "/" {
            entry.clone()
        } else {
            description
        };
        let partner = opt_integer(&row, "partner_id")?
            .and_then(|pid| partner_names.get(&pid))
            .map(String::as_str)
            .unwrap_or("");
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
                Some(date_of(&row)?),
                &entry,
                partner,
                money([number(&row, "debit")?, number(&row, "credit")?, running]),
                true,
            )?,
        );
        line.caret = vec!["entry", "open"];
        lines.push(line);
    }

    if count > end {
        let mut more = Line::new(
            id.child(Segment::more()),
            "",
            LineKind::More,
            layout.cells(report, None, "", "", [None; 3], false)?,
        );
        more.remaining = Some(count - end);
        more.next_offset = Some(end);
        lines.push(more);
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
    let mut model = "account.move.line";
    let mut form = None;
    let domain = match (location, target) {
        (Located::Item(account_id, item_id), target) => {
            ensure_account(cx, options, account_id).await?;
            let items = env
                .records(model)?
                .search(
                    cx.tx(),
                    &(account_dom(period(options)?, account_id)?
                        & condition(json!([["id", "=", item_id]]))?),
                )
                .await?;
            let rows = items
                .read_rows(cx.tx(), &["id", "date", "move_id"], Load::Raw)
                .await?;
            let row = rows.iter().next().map(Value::Row).ok_or_else(|| {
                invalid("This journal item is not visible in the General Ledger scope.")
            })?;
            match target {
                DrillTarget::Item(item) if item == "entry" => {
                    model = "account.move";
                    let mid = integer(&row, "move_id")?;
                    form = Some(mid);
                    condition(json!([["id", "=", mid]]))?
                }
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
                        "balance" => {
                            before_key(options, account_id, date_of(&row)?, item_id, true)?
                        }
                        "date" | "debit" | "credit" => {
                            account_dom(period(options)?, account_id)?
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
        (Located::Unallocated, DrillTarget::Cell(index))
        | (Located::Account(_), DrillTarget::Cell(index))
        | (Located::Opening(_), DrillTarget::Cell(index)) => {
            let label = report
                .columns
                .get(*index)
                .ok_or_else(|| invalid("General Ledger has no such column."))?
                .label
                .as_str();
            let d = match (location, label) {
                (Located::Opening(_), "balance") => {
                    history(options)?
                        & condition(json!([[
                            "date",
                            "<",
                            day(options
                                .period
                                .from
                                .ok_or_else(|| invalid("General Ledger needs a start date."))?)
                        ]]))?
                }
                (Located::Account(_), "balance") => history(options)?,
                (Located::Unallocated, "balance") => {
                    let missing = || {
                        invalid("General Ledger has no unallocated earnings line in this scope.")
                    };
                    let domain = unallocated(options)?.ok_or_else(missing)?;
                    if sums(cx, domain.clone()).await?[2] == Decimal::ZERO {
                        return Err(missing());
                    }
                    domain
                }
                (Located::Account(_), "debit" | "credit") => period(options)?,
                _ => return Err(invalid("This General Ledger cell has no amount to drill.")),
            };
            match location {
                Located::Account(a) | Located::Opening(a) => {
                    ensure_account(cx, options, a).await?;
                    account_dom(d, a)?
                }
                _ => d,
            }
        }
        (Located::Account(a), DrillTarget::Item(item)) if item == "journal_items" => {
            ensure_account(cx, options, a).await?;
            account_dom(history(options)?, a)?
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
            let target_mode = if model == "account.move" {
                "current"
            } else {
                "new"
            };
            json!({
                "type": "ir.actions.act_window",
                "name": report.name,
                "res_model": model,
                "res_id": record,
                "target": target_mode,
                "views": [[false, "form"]]
            })
        }
        None => {
            json!({
                "type": "ir.actions.act_window",
                "name": report.name,
                "res_model": model,
                "target": "current",
                "views": [[false, "list"], [false, "form"]],
                "domain": domain.to_json()?,
                "context": {}
            })
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
                    "General Ledger rows use posting date and immutable id order; custom sorting is not supported.",
                ));
            }
            if options.provider.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "epoch" | "fiscal_opening" | "fiscal_year_start"
                )
            }) {
                return Err(invalid(
                    "General Ledger provider options only support fiscal_opening, fiscal_year_start and epoch.",
                ));
            }
            for line in &options.unfolded {
                if !matches!(locate(report, line)?, Located::Account(_)) {
                    return Err(invalid(
                        "Only General Ledger account lines can be unfolded.",
                    ));
                }
            }
            for line in options.loaded_pages.keys() {
                if !matches!(locate(report, &LineId::parse(line)?)?, Located::Account(_)) {
                    return Err(invalid(
                        "Only General Ledger account lines can have loaded pages.",
                    ));
                }
            }
            let records = cx.env().records("account.move.line")?;
            records
                ._read_group(cx.tx(), ReadGroup::new(source).aggregates(["__count"]))
                .await?;
            let supplied_start = fiscal_start(options)?;
            let resolved_start = resolve_fiscal_start(cx, options).await?;
            options
                .provider
                .entry("fiscal_opening".to_owned())
                .or_insert(Json::Bool(false));
            if strict && supplied_start != resolved_start {
                return Err(invalid(STALE));
            }
            if let Some(start) = resolved_start {
                options
                    .provider
                    .insert("fiscal_year_start".into(), json!(day(start)));
            } else {
                options.provider.remove("fiscal_year_start");
            }
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
