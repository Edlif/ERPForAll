// SPDX-License-Identifier: MIT OR Apache-2.0
//! `dashboard_card` is JSON text of the card contract v1: buttons, stats and a chart, computed
//! for all requested journals in a fixed number of grouped reads under the reader's own access
//! rights (record rules and company scope apply inside each statement). As in the reference a
//! stat appears only when it counts something. Amounts are company currency: journal-item
//! balances on receivable and payable accounts for sales and purchases, the default account's
//! posted balance for bank, cash and credit card journals.
//!
//! The months of the sales and purchase chart and the days of the balance chart are declared
//! settings ([`GRAPH_MONTHS`], [`BALANCE_DAYS`]). User-visible labels are kept in [`labels`]: the
//! addon has no server-side code-text translation yet, so they arrive in the source language.

use std::collections::BTreeMap;

use erp_addon_base::ir_config_setting::read_values;
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{self, Value as Json, json};
use erp_sdk::settings::{ScopeKind, SettingDecl};
use erp_sdk::views::wire::SettingsScope;

use super::AccountJournal;
use crate::fiscal::calendar::add_months;

/// Declared setting: months (the current one included) the sales and purchase chart shows.
pub const GRAPH_MONTHS: &str = "account.dashboard_graph_months";
/// Default of [`GRAPH_MONTHS`]: the current month and the twelve before it, as the reference.
pub const GRAPH_MONTHS_DEFAULT: i64 = 13;
/// Floor of [`GRAPH_MONTHS`].
pub const GRAPH_MONTHS_FLOOR: i64 = 1;
/// Hard bound of [`GRAPH_MONTHS`]: one bar per month, at most three years on a card.
pub const GRAPH_MONTHS_HARD: i64 = 36;

/// Declared setting: days the balance chart of bank, cash and credit card journals covers.
pub const BALANCE_DAYS: &str = "account.dashboard_balance_days";
/// Default of [`BALANCE_DAYS`]: about a month, as the reference.
pub const BALANCE_DAYS_DEFAULT: i64 = 30;
/// Floor of [`BALANCE_DAYS`].
pub const BALANCE_DAYS_FLOOR: i64 = 7;
/// Hard bound of [`BALANCE_DAYS`]: one point per day, and the card parser admits at most 64 chart
/// points (`MAX_ITEMS` in `addons/web/static/src/client/dashboard-card.ts`).
pub const BALANCE_DAYS_HARD: i64 = 64;

/// The account types whose lines are what a customer or a vendor owes.
const OPEN_ACCOUNT_TYPES: [&str; 2] = ["asset_receivable", "liability_payable"];

/// Every user-visible text of the card, in one place until server-side code texts are translated.
pub mod labels {
    /// Button of a sales journal.
    pub const NEW_INVOICE: &str = "New Invoice";
    /// Button of a purchase journal.
    pub const NEW_BILL: &str = "New Bill";
    /// Button of a miscellaneous journal.
    pub const NEW_ENTRY: &str = "New Entry";
    /// Button of a bank, cash or credit card journal: its entries.
    pub const TRANSACTIONS: &str = "Transactions";
    /// Draft documents or entries.
    pub const DRAFTS: &str = "Drafts";
    /// Posted customer documents still open.
    pub const UNPAID: &str = "Unpaid";
    /// Posted vendor documents still open.
    pub const TO_PAY: &str = "To pay";
    /// Posted open documents due before today.
    pub const LATE: &str = "Late";
    /// Posted balance of a liquidity journal's default account.
    pub const BALANCE: &str = "Balance";
    /// Title of the sales chart.
    pub const INVOICED: &str = "Invoiced";
    /// Title of the purchase chart.
    pub const BILLED: &str = "Billed";
}

fn settings() -> Result<Vec<SettingDecl>> {
    let admin = |decl: SettingDecl| {
        decl.block("Dashboard")?.access(
            &["account.group_account_readonly"],
            &["account.group_account_manager"],
        )
    };
    Ok(vec![
        admin(SettingDecl::integer(
            GRAPH_MONTHS,
            "Dashboard: months in the chart of sales and purchase journals",
            ScopeKind::Global,
            GRAPH_MONTHS_DEFAULT,
            (GRAPH_MONTHS_FLOOR, GRAPH_MONTHS_HARD),
        )?)?,
        admin(SettingDecl::integer(
            BALANCE_DAYS,
            "Dashboard: days in the balance chart of bank and cash journals",
            ScopeKind::Global,
            BALANCE_DAYS_DEFAULT,
            (BALANCE_DAYS_FLOOR, BALANCE_DAYS_HARD),
        )?)?,
    ])
}
erp_sdk::setting_declarations!(settings);

/// The dashboard settings as read for one computation.
#[derive(Debug, Clone, Copy)]
struct DashboardSettings {
    graph_months: i32,
    balance_days: i32,
}

impl DashboardSettings {
    /// Both settings in one read, with the framework's rights (reading a card needs no settings
    /// group). A stored value outside its bounds is refused by name.
    async fn read(tx: &mut Transaction) -> Result<Self> {
        let env = tx.env().sudo(true);
        let keys = vec![GRAPH_MONTHS.to_owned(), BALANCE_DAYS.to_owned()];
        let read = read_values(&env, tx, SettingsScope::Global {}, keys).await?;
        let read = serde_json::to_value(&read)
            .map_err(|error| Error::internal(format!("dashboard settings: {error}")))?;
        let value = |key: &str, floor: i64, hard: i64| -> Result<i32> {
            read["values"]
                .as_array()
                .and_then(|values| values.iter().find(|v| v["key"] == key))
                .and_then(|entry| entry["value"].as_i64())
                .filter(|n| (floor..=hard).contains(n))
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| {
                    Error::validation(format!("{key} must be between {floor} and {hard}."))
                })
        };
        Ok(Self {
            graph_months: value(GRAPH_MONTHS, GRAPH_MONTHS_FLOOR, GRAPH_MONTHS_HARD)?,
            balance_days: value(BALANCE_DAYS, BALANCE_DAYS_FLOOR, BALANCE_DAYS_HARD)?,
        })
    }
}

/// The journal type, which decides the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sale,
    Purchase,
    Bank,
    Cash,
    Credit,
    General,
}

impl Kind {
    fn is_document(self) -> bool {
        matches!(self, Self::Sale | Self::Purchase)
    }

    fn is_liquidity(self) -> bool {
        matches!(self, Self::Bank | Self::Cash | Self::Credit)
    }
}

#[derive(Debug, Clone)]
struct Journal {
    id: i64,
    kind: Kind,
    company: i64,
    currency: Option<i64>,
    default_account: Option<i64>,
}

impl Journal {
    /// The list a stat of this journal opens.
    fn action(&self) -> &'static str {
        match self.kind {
            Kind::Sale => "account.action_move_out_invoice_type",
            Kind::Purchase => "account.action_move_in_invoice_type",
            _ => "account.action_move_journal_line",
        }
    }
}

fn id_cell(cell: &Cell, what: &str) -> Result<Option<i64>> {
    match cell {
        Cell::Id(id) => Ok(Some(*id)),
        Cell::Null => Ok(None),
        other => Err(Error::internal(format!("{what} is {other:?}"))),
    }
}

/// The journals of `records`, in their order, read with the reader's rights.
async fn journals(tx: &mut Transaction, records: &Records) -> Result<Vec<Journal>> {
    let types = records.cells(tx, "type").await?;
    let companies = records.cells(tx, "company_id").await?;
    let accounts = records.cells(tx, "default_account_id").await?;
    let mut out = Vec::with_capacity(types.len());
    let rows = records
        .ids()
        .into_iter()
        .zip(types)
        .zip(companies)
        .zip(accounts);
    for (((id, journal_type), company), account) in rows {
        let kind = match &journal_type {
            Cell::Str(t) => match &**t {
                "sale" => Kind::Sale,
                "purchase" => Kind::Purchase,
                "bank" => Kind::Bank,
                "cash" => Kind::Cash,
                "credit" => Kind::Credit,
                "general" => Kind::General,
                other => {
                    return Err(Error::internal(format!(
                        "journal {id} has the unknown type {other:?}"
                    )));
                }
            },
            other => {
                return Err(Error::internal(format!(
                    "journal {id} has the type {other:?}"
                )));
            }
        };
        let company = id_cell(&company, "company_id")?
            .ok_or_else(|| Error::internal(format!("journal {id} has no company")))?;
        out.push(Journal {
            id,
            kind,
            company,
            currency: None,
            default_account: id_cell(&account, "default_account_id")?,
        });
    }
    // The company currency of each journal company, in one read. Elevated: a branch reader may
    // see a parent company's journal without reading that company; the currency id is not data
    // of the journal's entries.
    let company_ids: Vec<i64> = {
        let mut ids: Vec<i64> = out.iter().map(|j| j.company).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let companies = records
        .env()
        .records("res.company")?
        .sudo(true)
        .browse(company_ids.clone())?;
    let currencies: BTreeMap<i64, Option<i64>> = company_ids
        .into_iter()
        .zip(companies.cells(tx, "currency_id").await?)
        .map(|(id, cell)| Ok((id, id_cell(&cell, "currency_id")?)))
        .collect::<Result<_>>()?;
    for journal in &mut out {
        journal.currency = currencies.get(&journal.company).copied().flatten();
    }
    Ok(out)
}

fn group_id(value: &GroupValue, what: &str) -> Result<i64> {
    value
        .record_id()
        .map(|id| id.get())
        .ok_or_else(|| Error::internal(format!("a dashboard group has no {what}")))
}

fn group_int(value: &GroupValue) -> Result<i64> {
    match value {
        GroupValue::Int(n) => Ok(*n),
        GroupValue::False => Ok(0),
        other => Err(Error::internal(format!("a dashboard count is {other:?}"))),
    }
}

fn group_decimal(value: &GroupValue) -> Result<Decimal> {
    match value {
        GroupValue::Decimal(d) => Ok(*d),
        GroupValue::Int(n) => Ok(Decimal::from(*n)),
        GroupValue::False | GroupValue::Null => Ok(Decimal::ZERO),
        other => Err(Error::internal(format!("a dashboard amount is {other:?}"))),
    }
}

fn group_str(value: &GroupValue) -> Result<&str> {
    match value {
        GroupValue::Str(s) => Ok(&**s),
        other => Err(Error::internal(format!("a dashboard state is {other:?}"))),
    }
}

fn group_date(value: &GroupValue) -> Result<Option<Date>> {
    match value {
        GroupValue::Date(d) => Ok(Some(*d)),
        GroupValue::False | GroupValue::Null => Ok(None),
        other => Err(Error::internal(format!("a dashboard day is {other:?}"))),
    }
}

async fn grouped(
    tx: &mut Transaction,
    env: &Env,
    model: &str,
    domain: Domain,
    groupby: &[&str],
    aggregates: &[&str],
) -> Result<Groups> {
    env.records(model)?
        ._read_group(
            tx,
            ReadGroup::new(domain)
                .groupby(groupby.iter().copied())
                .aggregates(aggregates.iter().copied()),
        )
        .await
}

fn day(date: Date, offset: i32) -> Date {
    Date::from_days(date.days() + offset)
}

/// The first day of the month of `date`.
fn month_start(date: Date) -> Result<Date> {
    let (year, month, _) = date.ymd();
    Date::from_ymd(year, month, 1)
        .ok_or_else(|| Error::internal(format!("no first day of the month of {date}")))
}

/// The first days of the chart months, oldest first, the current month last.
fn chart_months(today: Date, months: i32) -> Result<Vec<Date>> {
    let current = month_start(today)?;
    (0..months)
        .rev()
        .map(|back| add_months(current, -back))
        .collect()
}

/// Count and company-currency amount of a selection of documents.
#[derive(Debug, Clone, Copy, Default)]
struct Sum {
    count: i64,
    amount: Decimal,
}

/// Every number the cards show, read in a fixed number of statements.
#[derive(Debug, Default)]
struct Figures {
    /// Entries by journal and state.
    entries: BTreeMap<(i64, String), i64>,
    /// Sales and purchases: lines on receivable and payable accounts by journal and entry state.
    open: BTreeMap<(i64, String), Sum>,
    /// Sales and purchases: posted open lines due before today.
    late: BTreeMap<i64, Sum>,
    /// Sales and purchases: posted invoiced (billed) amount by journal and month start.
    monthly: BTreeMap<(i64, Date), Decimal>,
    /// Liquidity: posted lines and balance by (account, company).
    balance: BTreeMap<(i64, i64), Sum>,
    /// Liquidity: posted balance before the chart window by (account, company).
    opening: BTreeMap<(i64, i64), Decimal>,
    /// Liquidity: posted movement by (account, company) and day inside the chart window.
    daily: BTreeMap<(i64, i64, Date), Decimal>,
}

async fn figures(
    tx: &mut Transaction,
    env: &Env,
    journals: &[Journal],
    today: Date,
    settings: DashboardSettings,
) -> Result<Figures> {
    let mut f = Figures::default();
    let ids_where = |keep: fn(Kind) -> bool| -> Vec<i64> {
        journals
            .iter()
            .filter(|j| keep(j.kind))
            .map(|j| j.id)
            .collect()
    };

    // 1. Entries by journal and state (drafts, and whether a journal has entries at all).
    let groups = grouped(
        tx,
        env,
        "account.move",
        Domain::field("journal_id")?.in_(ids_where(|_| true)),
        &["journal_id", "state"],
        &["__count"],
    )
    .await?;
    for r in 0..groups.len() {
        let journal = group_id(&groups.column(0)[r], "journal")?;
        let state = group_str(&groups.column(1)[r])?.to_owned();
        f.entries
            .insert((journal, state), group_int(&groups.column(2)[r])?);
    }

    // 2-4. Sales and purchases: lines on receivable and payable accounts.
    let documents = ids_where(Kind::is_document);
    if !documents.is_empty() {
        let open_lines = || -> Result<Domain> {
            Ok(Domain::and([
                Domain::field("journal_id")?.in_(documents.clone()),
                Domain::field("account_id.account_type")?.in_(OPEN_ACCOUNT_TYPES.to_vec()),
            ]))
        };
        let posted = || -> Result<Domain> {
            Ok(Domain::and([
                open_lines()?,
                Domain::field("parent_state")?.eq("posted"),
            ]))
        };
        // shortcut: "open" is every posted line until reconciliation exists; then the residual
        // replaces the balance here.
        let groups = grouped(
            tx,
            env,
            "account.move.line",
            Domain::and([
                open_lines()?,
                Domain::field("parent_state")?.in_(vec!["draft", "posted"]),
            ]),
            &["journal_id", "parent_state"],
            &["balance:sum", "move_id:count_distinct"],
        )
        .await?;
        for r in 0..groups.len() {
            let journal = group_id(&groups.column(0)[r], "journal")?;
            let state = group_str(&groups.column(1)[r])?.to_owned();
            f.open.insert(
                (journal, state),
                Sum {
                    amount: group_decimal(&groups.column(2)[r])?,
                    count: group_int(&groups.column(3)[r])?,
                },
            );
        }
        let groups = grouped(
            tx,
            env,
            "account.move.line",
            Domain::and([
                posted()?,
                Domain::field("date_maturity")?.lt(today.to_string()),
            ]),
            &["journal_id"],
            &["balance:sum", "move_id:count_distinct"],
        )
        .await?;
        for r in 0..groups.len() {
            f.late.insert(
                group_id(&groups.column(0)[r], "journal")?,
                Sum {
                    amount: group_decimal(&groups.column(1)[r])?,
                    count: group_int(&groups.column(2)[r])?,
                },
            );
        }
        // The invoiced side only, as the reference charts invoices and bills without refunds:
        // debits on a sales journal's receivables, credits on a purchase journal's payables.
        let months = chart_months(today, settings.graph_months)?;
        let first = months
            .first()
            .copied()
            .ok_or_else(|| Error::internal(format!("{GRAPH_MONTHS} gave no month")))?;
        let end = add_months(month_start(today)?, 1)?;
        let mut sides = Vec::new();
        for (kind, side) in [
            (Kind::Sale, Domain::field("balance")?.gt(0)),
            (Kind::Purchase, Domain::field("balance")?.lt(0)),
        ] {
            let ids: Vec<i64> = journals
                .iter()
                .filter(|j| j.kind == kind)
                .map(|j| j.id)
                .collect();
            if !ids.is_empty() {
                sides.push(Domain::and([Domain::field("journal_id")?.in_(ids), side]));
            }
        }
        let groups = grouped(
            tx,
            env,
            "account.move.line",
            Domain::and([
                posted()?,
                Domain::or(sides),
                Domain::field("date")?.ge(first.to_string()),
                Domain::field("date")?.lt(end.to_string()),
            ]),
            &["journal_id", "date:month"],
            &["balance:sum"],
        )
        .await?;
        for r in 0..groups.len() {
            let journal = group_id(&groups.column(0)[r], "journal")?;
            if let Some(month) = group_date(&groups.column(1)[r])? {
                f.monthly
                    .insert((journal, month), group_decimal(&groups.column(2)[r])?);
            }
        }
    }

    // 5-7. Liquidity: posted lines of each default account in its journal's company.
    let pairs: Vec<(i64, i64)> = journals
        .iter()
        .filter(|j| j.kind.is_liquidity())
        .filter_map(|j| j.default_account.map(|a| (a, j.company)))
        .collect();
    if !pairs.is_empty() {
        let mut accounts: Vec<i64> = pairs.iter().map(|(a, _)| *a).collect();
        accounts.sort_unstable();
        accounts.dedup();
        let mut companies: Vec<i64> = pairs.iter().map(|(_, c)| *c).collect();
        companies.sort_unstable();
        companies.dedup();
        let base = || -> Result<Domain> {
            Ok(Domain::and([
                Domain::field("account_id")?.in_(accounts.clone()),
                Domain::field("company_id")?.in_(companies.clone()),
                Domain::field("parent_state")?.eq("posted"),
            ]))
        };
        let pair_of = |groups: &Groups, r: usize| -> Result<(i64, i64)> {
            Ok((
                group_id(&groups.column(0)[r], "account")?,
                group_id(&groups.column(1)[r], "company")?,
            ))
        };
        let start = day(today, 1 - settings.balance_days);
        let groups = grouped(
            tx,
            env,
            "account.move.line",
            base()?,
            &["account_id", "company_id"],
            &["balance:sum", "__count"],
        )
        .await?;
        for r in 0..groups.len() {
            f.balance.insert(
                pair_of(&groups, r)?,
                Sum {
                    amount: group_decimal(&groups.column(2)[r])?,
                    count: group_int(&groups.column(3)[r])?,
                },
            );
        }
        let groups = grouped(
            tx,
            env,
            "account.move.line",
            Domain::and([base()?, Domain::field("date")?.lt(start.to_string())]),
            &["account_id", "company_id"],
            &["balance:sum"],
        )
        .await?;
        for r in 0..groups.len() {
            f.opening
                .insert(pair_of(&groups, r)?, group_decimal(&groups.column(2)[r])?);
        }
        let groups = grouped(
            tx,
            env,
            "account.move.line",
            Domain::and([
                base()?,
                Domain::field("date")?.ge(start.to_string()),
                Domain::field("date")?.le(today.to_string()),
            ]),
            &["account_id", "company_id", "date:day"],
            &["balance:sum"],
        )
        .await?;
        for r in 0..groups.len() {
            let (account, company) = pair_of(&groups, r)?;
            if let Some(date) = group_date(&groups.column(2)[r])? {
                f.daily.insert(
                    (account, company, date),
                    group_decimal(&groups.column(3)[r])?,
                );
            }
        }
    }
    Ok(f)
}

fn amount(value: Decimal) -> Json {
    Json::String(value.to_string())
}

/// A stat.
fn stat(
    journal: &Journal,
    label: &str,
    sum: Sum,
    money: bool,
    action: &str,
    domain: Json,
    tone: &str,
) -> Json {
    let tone = if sum.count == 0 { "neutral" } else { tone };
    let (amount_value, currency) = if money {
        (amount(sum.amount), json!(journal.currency))
    } else {
        (Json::Null, Json::Null)
    };
    json!({
        "label": label,
        "count": sum.count,
        "amount": amount_value,
        "currency_id": currency,
        "action": action,
        "domain": domain,
        "context": {},
        "tone": tone,
    })
}

fn button(journal: &Journal) -> Json {
    let id = journal.id;
    let (label, action, view_mode, context) = match journal.kind {
        Kind::Sale => (
            labels::NEW_INVOICE,
            journal.action(),
            json!("form"),
            json!({"default_move_type": "out_invoice", "default_journal_id": id}),
        ),
        Kind::Purchase => (
            labels::NEW_BILL,
            journal.action(),
            json!("form"),
            json!({"default_move_type": "in_invoice", "default_journal_id": id}),
        ),
        // The reference opens the journal's statements; until statements exist, its entries.
        Kind::Bank | Kind::Cash | Kind::Credit => (
            labels::TRANSACTIONS,
            journal.action(),
            Json::Null,
            json!({"default_journal_id": id, "search_default_journal_id": id}),
        ),
        Kind::General => (
            labels::NEW_ENTRY,
            journal.action(),
            json!("form"),
            json!({"default_move_type": "entry", "default_journal_id": id}),
        ),
    };
    json!({
        "label": label,
        "action": action,
        "view_mode": view_mode,
        "context": context,
        "primary": true,
    })
}

/// The card of one journal from the figures.
fn card(journal: &Journal, f: &Figures, today: Date, settings: DashboardSettings) -> Result<Json> {
    let id = journal.id;
    let entries = |state: &str| f.entries.get(&(id, state.to_owned())).copied().unwrap_or(0);
    let drafts_domain = json!([["journal_id", "=", id], ["state", "=", "draft"]]);
    let (stats, chart) = match journal.kind {
        Kind::Sale | Kind::Purchase => {
            // purchases show what is owed as a positive amount
            let (sign, unpaid_label, chart_label) = if journal.kind == Kind::Purchase {
                (Decimal::NEGATIVE_ONE, labels::TO_PAY, labels::BILLED)
            } else {
                (Decimal::ONE, labels::UNPAID, labels::INVOICED)
            };
            let open = |state: &str| {
                f.open
                    .get(&(id, state.to_owned()))
                    .copied()
                    .unwrap_or_default()
            };
            let signed = |sum: Sum| Sum {
                count: sum.count,
                amount: sum.amount * sign,
            };
            let late = f.late.get(&id).copied().unwrap_or_default();
            let unpaid_domain = |late: bool| {
                let mut domain = vec![
                    json!(["journal_id", "=", id]),
                    json!(["state", "=", "posted"]),
                    json!(["payment_state", "in", ["not_paid", "partial"]]),
                ];
                if late {
                    domain.push(json!(["invoice_date_due", "<", today.to_string()]));
                }
                Json::Array(domain)
            };
            let drafts = Sum {
                count: entries("draft"),
                amount: open("draft").amount * sign,
            };
            let action = journal.action();
            let stats = [
                stat(
                    journal,
                    labels::DRAFTS,
                    drafts,
                    true,
                    action,
                    drafts_domain,
                    "neutral",
                ),
                stat(
                    journal,
                    unpaid_label,
                    signed(open("posted")),
                    true,
                    action,
                    unpaid_domain(false),
                    "warning",
                ),
                stat(
                    journal,
                    labels::LATE,
                    signed(late),
                    true,
                    action,
                    unpaid_domain(true),
                    "danger",
                ),
            ];
            // always drawn, with every month at zero on a journal without entries (the reference
            // draws sample bars there; ours shows the real, empty months)
            let chart = {
                let points: Vec<Json> = chart_months(today, settings.graph_months)?
                    .into_iter()
                    .map(|month| {
                        let (year, number, _) = month.ymd();
                        let value = f.monthly.get(&(id, month)).copied().unwrap_or_default();
                        json!({
                            "label": format!("{year:04}-{number:02}"),
                            "value": amount(value * sign),
                            "tone": "neutral",
                        })
                    })
                    .collect();
                json!({"type": "bar", "label": chart_label, "points": points})
            };
            (stats.to_vec(), chart)
        }
        Kind::Bank | Kind::Cash | Kind::Credit => {
            let pair = journal.default_account.map(|a| (a, journal.company));
            let balance = pair
                .and_then(|p| f.balance.get(&p).copied())
                .unwrap_or_default();
            // The reference shows the balance without a link; the general ledger link of its
            // reports addon is the journal items of the default account here.
            let balance_domain = match pair {
                Some((account, company)) => json!([
                    ["account_id", "=", account],
                    ["company_id", "=", company],
                    ["parent_state", "=", "posted"],
                ]),
                None => json!([]),
            };
            let stats = stat(
                journal,
                labels::BALANCE,
                balance,
                true,
                "account.action_account_moves_all",
                balance_domain,
                "neutral",
            );
            // always drawn: a journal without entries shows its flat balance line
            let chart = {
                let mut running = pair
                    .and_then(|p| f.opening.get(&p).copied())
                    .unwrap_or_default();
                let mut points = Vec::new();
                for offset in (0..settings.balance_days).rev() {
                    let date = day(today, -offset);
                    if let Some((account, company)) = pair {
                        running += f
                            .daily
                            .get(&(account, company, date))
                            .copied()
                            .unwrap_or_default();
                    }
                    points.push(json!({
                        "label": date.to_string(), "value": amount(running), "tone": "neutral"
                    }));
                }
                json!({"type": "line", "label": labels::BALANCE, "points": points})
            };
            (vec![stats], chart)
        }
        Kind::General => {
            let drafts = Sum {
                count: entries("draft"),
                amount: Decimal::ZERO,
            };
            let stats = stat(
                journal,
                labels::DRAFTS,
                drafts,
                false,
                journal.action(),
                drafts_domain,
                "warning",
            );
            (vec![stats], Json::Null)
        }
    };
    Ok(json!({"v": 1, "buttons": [button(journal)], "stats": stats, "chart": chart}))
}

#[methods(AccountJournal, addon = "account")]
impl AccountJournal {
    /// The dashboard card of each journal (contract v1), from one batch of grouped reads.
    #[compute(
        depends = ["type", "company_id", "default_account_id"],
        depends_context = ["uid", "allowed_company_ids", "tz"]
    )]
    async fn _compute_dashboard_card(
        this: &RecordSet<AccountJournal>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountJournal>,
    ) -> Result<()> {
        if this.is_empty() {
            return Ok(());
        }
        let env = this.env().clone();
        let today = env.context_today()?;
        let settings = DashboardSettings::read(tx).await?;
        let journals = journals(tx, this.as_records()).await?;
        let figures = figures(tx, &env, &journals, today, settings).await?;
        for (i, journal) in journals.iter().enumerate() {
            let text = card(journal, &figures, today, settings)?.to_string();
            out.set(i, AccountJournal::DASHBOARD_CARD, Some(text.into()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> Date {
        Date::from_ymd(y, m, d).unwrap()
    }

    #[test]
    fn chart_months_end_with_the_current_month() {
        let months = chart_months(date(2026, 2, 17), 13).unwrap();
        assert_eq!(months.len(), 13);
        assert_eq!(months.first(), Some(&date(2025, 2, 1)));
        assert_eq!(months.last(), Some(&date(2026, 2, 1)));
        assert!(months.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(
            chart_months(date(2026, 1, 31), 1).unwrap(),
            [date(2026, 1, 1)]
        );
    }

    /// The card parser's list bound and the chart settings' hard bounds are one fact: a setting the
    /// administrator may choose never produces a chart the card refuses.
    #[test]
    fn chart_setting_bounds_fit_the_card_parser() {
        let parser = include_str!("../../../web/static/src/client/dashboard-card.ts");
        let limit: i64 = parser
            .split("MAX_ITEMS = ")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|digits| digits.parse().ok())
            .expect("dashboard-card.ts declares MAX_ITEMS");
        assert_eq!(BALANCE_DAYS_HARD, limit, "one balance point per day");
        assert!(GRAPH_MONTHS_HARD <= limit, "one bar per month");
    }

    /// A stat that counts nothing is still shown, at zero and in the neutral tone.
    #[test]
    fn a_stat_counting_nothing_is_shown_at_zero() {
        let journal = Journal {
            id: 7,
            kind: Kind::Sale,
            company: 1,
            currency: Some(3),
            default_account: None,
        };
        let empty = stat(
            &journal,
            labels::LATE,
            Sum::default(),
            true,
            "a.b",
            json!([]),
            "danger",
        );
        assert_eq!(empty["count"], 0);
        assert_eq!(empty["amount"], "0");
        assert_eq!(empty["tone"], "neutral");
        let late = Sum {
            count: 2,
            amount: Decimal::from(5),
        };
        let shown = stat(
            &journal,
            labels::LATE,
            late,
            true,
            "a.b",
            json!([]),
            "danger",
        );
        assert_eq!(shown["count"], 2);
        assert_eq!(shown["amount"], "5");
        assert_eq!(shown["currency_id"], 3);
        let plain = stat(
            &journal,
            labels::DRAFTS,
            late,
            false,
            "a.b",
            json!([]),
            "warning",
        );
        assert!(plain["amount"].is_null() && plain["currency_id"].is_null());
    }
}
