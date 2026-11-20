// SPDX-License-Identifier: MIT OR Apache-2.0
//! The state changes of journal entries: post, reset to draft and cancel (3;
//! reference `_post`, `button_draft`, `button_cancel` in,
//! 6920-6940, 7070-7082`).
//!
//! Posting a batch of plain entries is set-based: one read of the headers, one aggregate over their
//! lines, one lock check (two when entries are re-dated), one header lock, one statement that flips
//! the headers and their lines together, then the numbering step last (3 steps 2, 3, 5
//! and 7); for them the statement count does not grow with the batch. The draft invoices of a batch
//! are first rebuilt from their product lines by the invoice line sync, which writes each changed
//! line and each changed header on its own, and an undated customer invoice takes today's date
//! through the entry write: that part grows with the invoices that need it. The validations are
//! collected and refused together, as the reference does.
//!
//! The company posting policy is `reversal_only` for every company until the opt-in policy column
//! exists, so a posted entry never returns to draft or to cancelled: it is reversed.

use std::collections::{BTreeMap, BTreeSet};

use erp_sdk::Operation;
use erp_sdk::prelude::*;
use erp_sdk::sequence::{FiscalYear, SequencePattern, SequenceReset};

use super::{AccountMove, TEXT_LINE_TYPES};
use crate::fiscal::calendar::{add_months, next_day, previous_day};
use crate::fiscal::settings::FiscalCalendar;
use crate::journal::{AccountJournal, JournalSequence};
use crate::locking::{
    JournalClass, LockCheck, LockRefusal, LockVerdict, TaxImpact, check_lock_dates,
};
use crate::numbering::{LockWait, NumberRequest, assign_numbers};

/// The group whose members may post, reset and cancel entries (the reference asks for its
/// invoicing group, which this addon does not declare yet; the accounting users hold the rights
/// on entries, `security/ir.access.csv`).
pub const POSTER_GROUP: &str = "account.group_account_user";

/// What a caller may still change on a posted or cancelled entry: its reference. Every other
/// field is refused by name, the invoice fields included (due date, totals, payment state, invoice
/// lines). Observed: the reference refuses its posted readonly fields (lines, invoice lines,
/// dates, partner, currency, payment terms,
/// refused at `:4061-4071`) and leaves the rest to its views. Modified: a default-deny list in
/// the ORM (R1b; the payment fields of its list are written by the payment slice,
/// never by a caller, and `narration` does not exist yet).
pub(crate) const POSTED_MOVE_WRITABLE: [&str; 1] = ["ref"];

/// What a caller may still change on the items of a posted or cancelled entry: the label and the
/// due date (R1b). Observed: the reference protects the tax, fiscal and hash fields of
/// a posted item (
/// `:3829-3838`) and the invoice amounts through its views. Modified: everything else (quantity,
/// price, discount, product, subtotals, taxes, amounts, accounts) is refused by the ORM.
pub(crate) const POSTED_LINE_WRITABLE: [&str; 2] = ["name", "date_maturity"];

#[methods(AccountMove, addon = "account")]
impl AccountMove {
    /// Posts the draft entries: every check is collected and refused together, an entry dated in
    /// a soft-locked period is re-dated, then the entries and their lines become posted in one
    /// statement and unnamed entries take the next numbers of their journals (
    /// -044, -048, -054).
    #[method]
    #[rpc(effect = write)]
    pub async fn action_post(this: &RecordSet<AccountMove>, tx: &mut Transaction) -> Result<()> {
        post_moves(tx, this.as_records()).await
    }

    /// Returns cancelled entries to draft. A posted entry is refused: the company posting policy
    /// is reversal-only.
    #[method]
    #[rpc(effect = write)]
    pub async fn button_draft(this: &RecordSet<AccountMove>, tx: &mut Transaction) -> Result<()> {
        reset_moves(tx, this.as_records()).await
    }

    /// Cancels draft entries. A posted entry is refused: the company posting policy is
    /// reversal-only.
    #[method]
    #[rpc(effect = write)]
    pub async fn button_cancel(this: &RecordSet<AccountMove>, tx: &mut Transaction) -> Result<()> {
        cancel_moves(tx, this.as_records()).await
    }
}

/// What the posting reads of one entry before it decides.
struct Header {
    id: i64,
    state: String,
    name: Option<String>,
    date: Date,
    company: i64,
    root_company: i64,
    journal: i64,
    journal_company: i64,
    journal_code: String,
    journal_type: String,
    journal_active: bool,
    journal_reset: String,
    journal_numbered_before: bool,
    currency_active: bool,
    currency_name: String,
    reference: Option<String>,
    move_type: String,
    payment: bool,
}

impl Header {
    fn label(&self) -> String {
        match &self.name {
            Some(name) => format!("entry {name}"),
            None => format!("draft entry {}", self.id),
        }
    }

    fn journal_class(&self) -> JournalClass {
        match self.journal_type.as_str() {
            "sale" => JournalClass::Sale,
            "purchase" => JournalClass::Purchase,
            _ => JournalClass::Other,
        }
    }

    fn is_sale_document(&self) -> bool {
        matches!(
            self.move_type.as_str(),
            "out_invoice" | "out_refund" | "out_receipt"
        )
    }

    /// The numbering kind of the entry: a credit note asks for the refund chain and a payment's
    /// entry the payment chain, which the journal gives only when it keeps one (`R` or `P` before
    /// the code; else the document chain); everything else is a document (reference
    /// ).
    fn sequence_kind(&self) -> &'static str {
        if self.payment {
            "payment"
        } else if crate::invoice::is_refund(&self.move_type) {
            "refund"
        } else {
            "document"
        }
    }
}

/// What the posting reads of the lines of one entry.
#[derive(Default)]
struct LineFacts {
    accountable: i64,
    unbalanced: Option<String>,
    mixed_off_balance: bool,
    archived_accounts: Vec<i64>,
    foreign_accounts: Vec<i64>,
    /// A line carries taxes or is a tax line, so the entry reaches the tax report.
    taxed: bool,
}

/// Post the draft entries of `moves` (3). Atomic: a refusal leaves every entry as it
/// was.
///
/// # Errors
///
/// `Access` without [`POSTER_GROUP`] or write access to an entry; `UserError` with every collected
/// validation message; the lock and period refusals of [`check_lock_dates`]; numbering and
/// database errors.
pub async fn post_moves(tx: &mut Transaction, moves: &Records) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    authorize(tx, moves, "post").await?;
    let ids = sorted_ids(moves);
    tx.savepoint(async |tx| {
        tx.flush().await?;
        // The draft invoices of the batch are rebuilt from their product lines first: a price,
        // quantity, discount or tax written on an invoice line directly did not run the sync, and
        // the checks below read the stored totals. Then the invoice checks and the invoice date
        // default: the date they write is part of what the header read below sees. Entries that
        // are not invoices pass all three.
        crate::invoice::sync_lines(tx, moves.env(), &ids).await?;
        super::refresh_entry_totals(tx, &ids).await?;
        tx.flush().await?;
        crate::invoice::post::check_postable(tx, moves.env(), &ids).await?;
        crate::invoice::post::before_post(tx, moves.env(), &ids).await?;
        tx.flush().await?;
        let headers = read_headers(tx, &ids).await?;
        let facts = line_facts(tx, &ids).await?;
        let mut messages = Vec::new();
        let mut account_ids = BTreeSet::new();
        for header in &headers {
            if let Some(facts) = facts.get(&header.id) {
                account_ids.extend(facts.archived_accounts.iter().copied());
                account_ids.extend(facts.foreign_accounts.iter().copied());
            }
        }
        let account_names = account_names(tx, moves.env(), &account_ids).await?;
        for header in &headers {
            let facts = facts.get(&header.id).ok_or_else(|| {
                Error::invariant(format!("entry {} has no line aggregate", header.id))
            })?;
            for message in validate(header, facts, &account_names) {
                let message = if headers.len() > 1 {
                    format!("{}: {message}", header.label())
                } else {
                    message
                };
                if !messages.contains(&message) {
                    messages.push(message);
                }
            }
        }
        if !messages.is_empty() {
            return Err(Error::user(messages.join("\n")));
        }
        let taxed: BTreeSet<i64> = facts
            .iter()
            .filter(|(_, f)| f.taxed)
            .map(|(id, _)| *id)
            .collect();
        let dates = accounting_dates(tx, moves.env(), &headers, &taxed).await?;
        let locked = lock_headers(tx, &ids).await?;
        let unchanged = headers.len() == locked.len()
            && headers
                .iter()
                .zip(&locked)
                .all(|(h, (id, state, date))| h.id == *id && h.state == *state && h.date == *date);
        if !unchanged {
            return Err(Error::conflict(
                "A journal entry changed while it was being posted; post it again.",
            ));
        }
        let pairs: Vec<(i64, Date)> = headers
            .iter()
            .map(|h| h.id)
            .zip(dates.iter().copied())
            .collect();
        set_state(tx, &pairs, "posted").await?;
        // the history of the entries (a no-op without a tracking addon such as the mail bridge)
        tx.track_external(
            moves,
            "state",
            headers
                .iter()
                .map(|h| (h.id, text_cell(&h.state), text_cell("posted"))),
        )?;
        tx.track_external(
            moves,
            "date",
            pairs
                .iter()
                .zip(&headers)
                .map(|((id, date), h)| (*id, Cell::Date(h.date), Cell::Date(*date))),
        )?;
        number(tx, moves, &headers, &dates).await?;
        invalidate(tx, moves).await
    })
    .await
}

/// Return the cancelled entries of `moves` to draft; a posted entry is refused (
/// reversal-only), so no lock date or number is touched (the reference checks the lock dates
/// only when an entry leaves `posted`).
///
/// # Errors
///
/// `Access` as [`post_moves`]; `UserError` for a draft entry or a posted one.
pub async fn reset_moves(tx: &mut Transaction, moves: &Records) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    authorize(tx, moves, "reset to draft").await?;
    let ids = sorted_ids(moves);
    tx.savepoint(async |tx| {
        tx.flush().await?;
        let states = lock_headers(tx, &ids).await?;
        if states.iter().any(|(_, state, _)| state == "posted") {
            return Err(reversal_only());
        }
        if states.iter().any(|(_, state, _)| state != "cancel") {
            return Err(Error::user(
                "Only a posted or cancelled entry can go back to draft; the selection holds a draft.",
            ));
        }
        let pairs: Vec<(i64, Date)> = states.iter().map(|(id, _, date)| (*id, *date)).collect();
        set_state(tx, &pairs, "draft").await?;
        tx.track_external(
            moves,
            "state",
            states
                .iter()
                .map(|(id, state, _)| (*id, text_cell(state), text_cell("draft"))),
        )?;
        invalidate(tx, moves).await
    })
    .await
}

/// Cancel the draft entries of `moves`; a posted entry is refused (reversal-only).
/// Reconciliation, payments and scheduled posting do not exist yet, so the reference's clean-up
/// of those has nothing to act on.
///
/// # Errors
///
/// `Access` as [`post_moves`]; `UserError` for an entry that is not a draft.
pub async fn cancel_moves(tx: &mut Transaction, moves: &Records) -> Result<()> {
    if moves.is_empty() {
        return Ok(());
    }
    authorize(tx, moves, "cancel").await?;
    let ids = sorted_ids(moves);
    tx.savepoint(async |tx| {
        tx.flush().await?;
        let states = lock_headers(tx, &ids).await?;
        if states.iter().any(|(_, state, _)| state == "posted") {
            return Err(reversal_only());
        }
        if states.iter().any(|(_, state, _)| state != "draft") {
            return Err(Error::user(
                "Cancelling applies to draft entries only; the selection holds an entry that is not a draft.",
            ));
        }
        let pairs: Vec<(i64, Date)> = states.iter().map(|(id, _, date)| (*id, *date)).collect();
        set_state(tx, &pairs, "cancel").await?;
        tx.track_external(
            moves,
            "state",
            states
                .iter()
                .map(|(id, state, _)| (*id, text_cell(state), text_cell("cancel"))),
        )?;
        invalidate(tx, moves).await
    })
    .await
}

/// Asks the caller's `operation` right on the entries of `move_ids`. It runs before any statement
/// reads or locks an entry by a caller-supplied id: without it the refusal text would give the
/// state and number of an entry the caller may not read (another company's), and a caller without
/// rights could hold its header until its own transaction ends.
async fn authorize_entries(
    tx: &mut Transaction,
    env: &Env,
    move_ids: &[i64],
    operation: Operation,
) -> Result<()> {
    env.records("account.move")?
        .browse(move_ids.iter().copied())?
        .check_access(tx, operation)
        .await
}

/// Refuses `what` on the entries of `move_ids` that are posted or cancelled (
/// -066, -069, -070), after the caller's `operation` right on those entries was checked.
///
/// # Errors
///
/// `Access` when the caller lacks `operation` on one of the entries; `UserError` naming the first
/// posted or cancelled entry; database errors.
pub(crate) async fn refuse_closed_entries(
    tx: &mut Transaction,
    env: &Env,
    move_ids: &[i64],
    operation: Operation,
    what: &str,
) -> Result<()> {
    if move_ids.is_empty() {
        return Ok(());
    }
    authorize_entries(tx, env, move_ids, operation).await?;
    closed_refusal(tx, move_ids, what).await
}

/// The refusal of [`refuse_closed_entries`], for entries the caller was authorized on.
async fn closed_refusal(tx: &mut Transaction, move_ids: &[i64], what: &str) -> Result<()> {
    let rows = tx
        .query(
            "SELECT id::int8, state::text, name::text FROM account_move \
             WHERE id = ANY($1::int8[]) AND state <> 'draft' ORDER BY id LIMIT 1",
            &[&move_ids],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(());
    };
    let id: i64 = row.try_get(0).map_err(db)?;
    let state: String = row.try_get(1).map_err(db)?;
    let name: Option<String> = row.try_get(2).map_err(db)?;
    let label = name.unwrap_or_else(|| format!("{id}"));
    Err(Error::user(format!(
        "You cannot {what} the {state} entry {label}: a posted entry is corrected by a reversal \
         (which is not available yet), a cancelled one by resetting it to draft first."
    )))
}

/// The fields of `vals` (values and commands) that `writable` does not list, sorted.
pub(crate) fn closed_fields<'a>(vals: &'a RawVals, writable: &[&str]) -> Vec<&'a str> {
    let mut fields: Vec<&str> = vals
        .iter()
        .map(|(field, _)| field)
        .chain(vals.iter_commands().map(|(field, _)| field))
        .filter(|field| !writable.contains(field))
        .collect();
    fields.sort_unstable();
    fields.dedup();
    fields
}

/// Takes the headers of `move_ids` in id order, the order posting locks them in, with a version
/// update, then refuses when one of them is posted or cancelled. A lock alone would not do: a
/// poster whose snapshot is older than a committed item change then sees an unchanged header
/// and posts without the item. With the version update, whichever of the two commits second
/// fails with a serialization error at its header lock, and its retry sees the other's result.
/// The caller's `operation` right on the entries is checked first, before the lock is taken.
///
/// # Errors
///
/// As [`refuse_closed_entries`]; a serialization failure when a concurrent transaction changed one
/// of the headers since this one began.
pub(crate) async fn hold_draft_entries(
    tx: &mut Transaction,
    env: &Env,
    move_ids: &[i64],
    operation: Operation,
    what: &str,
) -> Result<()> {
    if move_ids.is_empty() {
        return Ok(());
    }
    authorize_entries(tx, env, move_ids, operation).await?;
    tx.execute(
        "WITH held AS (SELECT id FROM account_move WHERE id = ANY($1::int8[]) \
                       ORDER BY id FOR NO KEY UPDATE) \
         UPDATE account_move m SET write_date = m.write_date FROM held WHERE m.id = held.id",
        &[&move_ids],
    )
    .await?;
    closed_refusal(tx, move_ids, what).await
}

/// The database ids of `moves`, sorted and without repeats: every statement below reads and
/// writes in this order.
fn sorted_ids(moves: &Records) -> Vec<i64> {
    let mut ids = moves.ids();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn reversal_only() -> Error {
    Error::user(
        "A posted journal entry cannot be reset to draft or cancelled: this company's posting \
         policy only allows a reversal, which is not available yet. A policy that allows \
         resetting a posted entry is not available yet either.",
    )
}

async fn authorize(tx: &mut Transaction, moves: &Records, what: &str) -> Result<()> {
    let env = moves.env().clone();
    if !env.su() && !tx.has_group(&env, POSTER_GROUP).await? {
        return Err(Error::access(format!(
            "Only accounting users may {what} journal entries ({POSTER_GROUP})."
        )));
    }
    moves.check_access(tx, Operation::Write).await
}

fn db(error: impl std::fmt::Display) -> Error {
    Error::db(format!(
        "A journal entry could not be read for posting: {error}"
    ))
}

async fn read_headers(tx: &mut Transaction, ids: &[i64]) -> Result<Vec<Header>> {
    // `journal_numbered_before` stands for the reference's "the journal has a highest name"
    // : a posted entry exists in the journal.
    let rows = tx
        .query(
            "SELECT m.id::int8, m.state::text, m.name::text, m.date, m.company_id::int8, \
             NULLIF(split_part(co.parent_path, '/', 1), '')::int8, \
             m.journal_id::int8, j.company_id::int8, j.code::text, j.type::text, j.active, \
             j.sequence_reset::text, \
             EXISTS (SELECT 1 FROM account_move p WHERE p.journal_id = m.journal_id \
                     AND p.state = 'posted'), \
             cur.active, cur.name::text, m.ref::text, m.move_type::text, \
             m.origin_payment_id IS NOT NULL \
             FROM account_move m \
             JOIN res_company co ON co.id = m.company_id \
             JOIN account_journal j ON j.id = m.journal_id \
             JOIN res_currency cur ON cur.id = m.currency_id \
             WHERE m.id = ANY($1::int8[]) ORDER BY m.id",
            &[&ids],
        )
        .await?;
    if rows.len() != ids.len() {
        return Err(Error::missing(
            "Some of the journal entries to post no longer exist.",
        ));
    }
    let mut headers = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row.try_get(0).map_err(db)?;
        let root: Option<i64> = row.try_get(5).map_err(db)?;
        headers.push(Header {
            id,
            state: row.try_get(1).map_err(db)?,
            name: row.try_get(2).map_err(db)?,
            date: row.try_get(3).map_err(db)?,
            company: row.try_get(4).map_err(db)?,
            root_company: root.ok_or_else(|| {
                Error::internal(format!("The company of entry {id} has no company path."))
            })?,
            journal: row.try_get(6).map_err(db)?,
            journal_company: row.try_get(7).map_err(db)?,
            journal_code: row.try_get(8).map_err(db)?,
            journal_type: row.try_get(9).map_err(db)?,
            journal_active: row.try_get(10).map_err(db)?,
            journal_reset: row.try_get(11).map_err(db)?,
            journal_numbered_before: row.try_get(12).map_err(db)?,
            currency_active: row.try_get(13).map_err(db)?,
            currency_name: row.try_get(14).map_err(db)?,
            reference: row.try_get(15).map_err(db)?,
            move_type: row.try_get(16).map_err(db)?,
            payment: row.try_get(17).map_err(db)?,
        });
    }
    Ok(headers)
}

/// One aggregate over the lines of all the entries: accountable lines, the balance, archived
/// accounts, accounts none of whose companies is the entry's company or one of its parents
/// (reference), and whether a line has taxes or is a tax line (the
/// reference's tax-report impact; tax tags do not exist yet).
async fn line_facts(tx: &mut Transaction, ids: &[i64]) -> Result<BTreeMap<i64, LineFacts>> {
    let text_types: Vec<&str> = TEXT_LINE_TYPES.to_vec();
    let rows = tx
        .query(
            "SELECT m.id::int8, \
             count(l.id) FILTER (WHERE l.display_type IS NULL \
                                 OR l.display_type <> ALL($2::text[]))::int8, \
             CASE WHEN COALESCE(sum(l.balance), 0) <> 0 THEN sum(l.balance)::text END, \
             COALESCE(array_agg(DISTINCT l.account_id::int8) \
                      FILTER (WHERE a.active IS FALSE), '{}'), \
             COALESCE(array_agg(DISTINCT l.account_id::int8) FILTER (WHERE l.account_id IS NOT NULL \
               AND NOT EXISTS (SELECT 1 FROM account_account_res_company_rel r \
                 WHERE r.account_account_id = l.account_id \
                   AND r.res_company_id = ANY (string_to_array(rtrim(co.parent_path, '/'), '/')::int8[]))), \
               '{}'), \
             COALESCE(bool_or(l.tax_line_id IS NOT NULL OR l.tax_repartition_line_id IS NOT NULL \
               OR EXISTS (SELECT 1 FROM account_move_line_account_tax_rel r \
                          WHERE r.account_move_line_id = l.id)), false), \
             COALESCE(bool_or(a.account_type = 'off_balance') \
                      AND bool_or(a.account_type <> 'off_balance'), false) \
             FROM account_move m \
             JOIN res_company co ON co.id = m.company_id \
             LEFT JOIN account_move_line l ON l.move_id = m.id \
             LEFT JOIN account_account a ON a.id = l.account_id \
             WHERE m.id = ANY($1::int8[]) GROUP BY m.id ORDER BY m.id",
            &[&ids, &text_types],
        )
        .await?;
    let mut facts = BTreeMap::new();
    for row in &rows {
        let id: i64 = row.try_get(0).map_err(db)?;
        facts.insert(
            id,
            LineFacts {
                accountable: row.try_get(1).map_err(db)?,
                unbalanced: row.try_get(2).map_err(db)?,
                archived_accounts: row.try_get(3).map_err(db)?,
                foreign_accounts: row.try_get(4).map_err(db)?,
                taxed: row.try_get(5).map_err(db)?,
                mixed_off_balance: row.try_get(6).map_err(db)?,
            },
        );
    }
    Ok(facts)
}

/// The names of the accounts a refusal lists, in one read.
async fn account_names(
    tx: &mut Transaction,
    env: &Env,
    ids: &BTreeSet<i64>,
) -> Result<BTreeMap<i64, String>> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let accounts = env
        .records("account.account")?
        .sudo(true)
        .with_context([("active_test", Value::Bool(false))])
        .browse(ids.iter().copied())?;
    let names = accounts.cells(tx, "name").await?;
    Ok(accounts
        .ids()
        .into_iter()
        .zip(names)
        .map(|(id, name)| {
            let name = match name {
                Cell::Str(name) => name.to_string(),
                _ => format!("account {id}"),
            };
            (id, name)
        })
        .collect())
}

/// The collected refusals of one entry, in the reference's order.
fn validate(
    header: &Header,
    facts: &LineFacts,
    account_names: &BTreeMap<i64, String>,
) -> Vec<String> {
    let mut messages = Vec::new();
    if header.state != "draft" {
        messages.push(format!(
            "The {} (id {}) is not a draft, so it cannot be posted again.",
            header.label(),
            header.id
        ));
    }
    if header.state == "draft"
        && let Some(name) = &header.name
    {
        // A posted entry never returns to draft (reversal-only), so a draft with a name was named
        // by hand. Such a name is not posted: it could repeat a posted number or take one the
        // journal's chain gives out later.
        messages.push(format!(
            "The draft carries the number {name:?}, written by hand: clear it, an entry takes \
             its number from its journal when it is posted."
        ));
    }
    if facts.accountable == 0 {
        messages
            .push("There is nothing to post: the entry has no line with an account.".to_owned());
    }
    if !header.journal_active {
        messages.push(format!(
            "The journal {} is archived; entries are no longer posted in it.",
            header.journal_code
        ));
    }
    if !header.currency_active {
        messages.push(format!(
            "The currency {} is archived; activate it before posting.",
            header.currency_name
        ));
    }
    if !facts.archived_accounts.is_empty() {
        messages.push(format!(
            "A line of this entry uses an archived account ({}); you cannot post it.",
            names(&facts.archived_accounts, account_names)
        ));
    }
    if !facts.foreign_accounts.is_empty() {
        messages.push(format!(
            "These accounts belong to no company of the entry's company tree: {}.",
            names(&facts.foreign_accounts, account_names)
        ));
    }
    if facts.mixed_off_balance {
        messages.push(
            "Off-balance accounts cannot be mixed with ordinary accounts in one journal entry."
                .to_owned(),
        );
    }
    if let Some(difference) = &facts.unbalanced {
        messages.push(format!(
            "Debits and credits differ by {difference}; balance the entry before posting."
        ));
    }
    messages
}

fn names(ids: &[i64], account_names: &BTreeMap<i64, String>) -> String {
    ids.iter()
        .map(|id| {
            account_names
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("account {id}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The re-dated entries are checked again and a
/// refusal of the new date (a period that is not open) is final. The entries of `taxed` reach
/// the tax report, so the tax lock date binds them too.
///
/// A re-dated entry keeps its company-currency balances: they are not converted again at the new
/// date. Observed: the reference's balance of a plain entry is the amount entered
/// and an invoice converts at its
/// invoice date; neither follows the
/// accounting date. A draft invoice whose date is changed by hand is converted again by the
/// invoice line sync.
async fn accounting_dates(
    tx: &mut Transaction,
    env: &Env,
    headers: &[Header],
    taxed: &BTreeSet<i64>,
) -> Result<Vec<Date>> {
    let companies: Vec<_> = headers
        .iter()
        .map(|h| h.company)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let days = erp_addon_base::company_calendar::business_days(tx, env, &companies).await?;
    let check = |header: &Header, date: Date| {
        let tax = if taxed.contains(&header.id) {
            TaxImpact::Report
        } else {
            TaxImpact::None
        };
        LockCheck::new(header.company, date, header.journal_class(), tax)
    };
    let checks: Vec<LockCheck> = headers
        .iter()
        .map(|h| check(h, h.date))
        .collect::<Result<_>>()?;
    ensure_periods_for(tx, env, &checks).await?;
    let verdicts = check_lock_dates(tx, &checks).await?;
    let mut dates = Vec::with_capacity(headers.len());
    let mut moved = Vec::new();
    for (index, (header, verdict)) in headers.iter().zip(verdicts).enumerate() {
        match verdict {
            LockVerdict::Allowed => dates.push(header.date),
            LockVerdict::Refused(LockRefusal::Dates { violations, .. }) => {
                let latest = violations
                    .iter()
                    .map(|v| v.through())
                    .max()
                    .ok_or_else(|| Error::invariant("a lock refusal names no lock date"))?;
                let reset = SequenceReset::parse(&header.journal_reset)?;
                dates.push(shifted_date(
                    next_day(latest),
                    days.get(&header.company)
                        .ok_or_else(|| {
                            Error::missing(format!("Company {} is unavailable.", header.company))
                        })?
                        .date,
                    reset,
                    header.journal_numbered_before,
                    header.is_sale_document(),
                )?);
                moved.push(index);
            }
            refused => {
                refused.into_result()?;
                return Err(Error::invariant(
                    "a refused lock verdict converted to success",
                ));
            }
        }
    }
    if moved.is_empty() {
        return Ok(dates);
    }
    let rechecks: Vec<LockCheck> = headers
        .iter()
        .zip(&dates)
        .enumerate()
        .filter(|(index, _)| moved.contains(index))
        .map(|(_, (header, date))| check(header, *date))
        .collect::<Result<_>>()?;
    ensure_periods_for(tx, env, &rechecks).await?;
    for verdict in check_lock_dates(tx, &rechecks).await? {
        verdict.into_result()?;
    }
    Ok(dates)
}

/// Posting creates the fiscal periods it needs (a system step, R2): the first entry of
/// a month must not be refused because nobody generated its period. Locks and a date beyond the
/// horizon setting still refuse.
async fn ensure_periods_for(tx: &mut Transaction, env: &Env, checks: &[LockCheck]) -> Result<()> {
    let rows: Vec<(i64, Date)> = checks.iter().map(|c| (c.company, c.date)).collect();
    crate::fiscal::ensure::ensure_for_rows(tx, env, &rows).await?;
    Ok(())
}

pub(crate) fn shifted_date(
    start: Date,
    today: Date,
    reset: SequenceReset,
    numbered_before: bool,
    sale: bool,
) -> Result<Date> {
    let (year, month, _) = start.ymd();
    let (today_year, today_month, _) = today.ymd();
    let month_end = previous_day(add_months(first_of_month(start)?, 1)?);
    let year_end = Date::from_ymd(year, 12, 31)
        .ok_or_else(|| Error::validation(format!("{year} has no 31 December")))?;
    let monthly = !numbered_before || matches!(reset, SequenceReset::Month);
    if sale {
        return Ok(if monthly {
            today.min(month_end)
        } else if reset == SequenceReset::Year {
            today.min(year_end)
        } else {
            start
        });
    }
    let monthly = monthly || reset == SequenceReset::YearRangeMonth;
    Ok(if monthly {
        if (today_year, today_month) > (year, month) {
            month_end
        } else {
            start.max(today)
        }
    } else if reset == SequenceReset::Year {
        if today_year > year {
            year_end
        } else {
            start.max(today)
        }
    } else {
        start
    })
}

fn first_of_month(date: Date) -> Result<Date> {
    let (year, month, _) = date.ymd();
    Date::from_ymd(year, month, 1)
        .ok_or_else(|| Error::validation(format!("{year}-{month:02} has no first day")))
}

/// Lock the headers in id order (1 step 2) and return their state and date. At
/// `REPEATABLE READ` a header another transaction changed and committed since this one began is
/// a serialization failure, retried by the unit of work.
async fn lock_headers(tx: &mut Transaction, ids: &[i64]) -> Result<Vec<(i64, String, Date)>> {
    let rows = tx
        .query(
            "SELECT id::int8, state::text, date FROM account_move \
             WHERE id = ANY($1::int8[]) ORDER BY id FOR NO KEY UPDATE",
            &[&ids],
        )
        .await?;
    if rows.len() != ids.len() {
        return Err(Error::missing(
            "Some of the journal entries no longer exist.",
        ));
    }
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get(0).map_err(db)?,
                row.try_get(1).map_err(db)?,
                row.try_get(2).map_err(db)?,
            ))
        })
        .collect()
}

fn text_cell(text: &str) -> Cell {
    Cell::Str(text.into())
}

/// One statement flips the headers of `pairs` (entry, accounting date) and their lines to `state`
/// and writes the accounting date on both (3 step 5).
async fn set_state(tx: &mut Transaction, pairs: &[(i64, Date)], state: &str) -> Result<()> {
    let (ids, dates): (Vec<i64>, Vec<Date>) = pairs.iter().copied().unzip();
    let uid = tx.env().uid();
    let rows = tx
        .query(
            "WITH v AS (SELECT * FROM unnest($1::int8[], $2::date[]) AS v(id, date)), \
             m AS (UPDATE account_move SET state = $3, date = v.date, write_uid = $4, \
                   write_date = (now() AT TIME ZONE 'UTC') \
                   FROM v WHERE account_move.id = v.id RETURNING account_move.id), \
             l AS (UPDATE account_move_line SET parent_state = $3, date = v.date, write_uid = $4, \
                   write_date = (now() AT TIME ZONE 'UTC') \
                   FROM v WHERE account_move_line.move_id = v.id RETURNING 1) \
             SELECT (SELECT count(*) FROM m)::int8",
            &[&ids, &dates, &state, &uid],
        )
        .await?;
    let changed: i64 = rows
        .first()
        .ok_or_else(|| Error::internal("The state change returned no row."))?
        .try_get(0)
        .map_err(db)?;
    if usize::try_from(changed).ok() != Some(ids.len()) {
        return Err(Error::internal(format!(
            "The state change wrote {changed} of {} journal entries.",
            ids.len()
        )));
    }
    Ok(())
}

/// Number the entries, last in the posting (3 step 7): one declaration read per
/// journal, one calendar read per root company, one allocation for the batch and one statement
/// writing the names. Every entry is unnamed here: a named draft was refused with the
/// validations. Credit notes are numbered in the refund chain of a journal that keeps one.
/// `dates` is in `headers` order.
async fn number(
    tx: &mut Transaction,
    moves: &Records,
    headers: &[Header],
    dates: &[Date],
) -> Result<()> {
    let env = moves.env();
    if let Some(named) = headers.iter().find(|h| h.name.is_some()) {
        return Err(Error::invariant(format!(
            "entry {} reached numbering with a name",
            named.id
        )));
    }
    let unnamed: Vec<(&Header, Date)> = headers.iter().zip(dates.iter().copied()).collect();
    if unnamed.is_empty() {
        return Ok(());
    }
    let journal_ids: BTreeSet<i64> = unnamed.iter().map(|(h, _)| h.journal).collect();
    let journals = env
        .records("account.journal")?
        .sudo(true)
        .with_context([("active_test", Value::Bool(false))])
        .browse(journal_ids.iter().copied())?;
    let journals = RecordSet::<AccountJournal>::try_from(journals)?;
    let mut patterns: BTreeMap<(i64, &'static str), (&'static str, SequencePattern)> =
        BTreeMap::new();
    for journal in journals.iter() {
        let id = journal
            .as_records()
            .ids()
            .first()
            .copied()
            .ok_or_else(|| Error::invariant("a journal record has no id"))?;
        let declaration = JournalSequence::of(tx, &journal).await?;
        let kinds: BTreeSet<&'static str> = unnamed
            .iter()
            .filter(|(h, _)| h.journal == id)
            .map(|(h, _)| h.sequence_kind())
            .collect();
        for kind in kinds {
            patterns.insert((id, kind), declaration.pattern(kind)?);
        }
    }
    let mut calendars: BTreeMap<i64, FiscalCalendar> = BTreeMap::new();
    for (header, _) in &unnamed {
        if let std::collections::btree_map::Entry::Vacant(slot) =
            calendars.entry(header.root_company)
        {
            slot.insert(FiscalCalendar::read(tx, header.root_company).await?);
        }
    }
    let mut requests = Vec::with_capacity(unnamed.len());
    for (header, date) in &unnamed {
        let (kind, pattern) = patterns
            .get(&(header.journal, header.sequence_kind()))
            .ok_or_else(|| Error::invariant("a journal pattern was not read"))?;
        let calendar = calendars
            .get(&header.root_company)
            .ok_or_else(|| Error::invariant("a fiscal calendar was not read"))?;
        let fiscal_year: FiscalYear = calendar.year_end().fiscal_year_of(*date)?;
        requests.push(NumberRequest {
            document_id: header.id,
            company_id: header.journal_company,
            journal_id: header.journal,
            kind,
            pattern,
            date: *date,
            fiscal_year: Some(fiscal_year),
            reference: header.reference.as_deref(),
        });
    }
    let wait = LockWait::from_settings(tx).await?;
    let assigned = assign_numbers(tx, wait, &requests).await?;
    let ids: Vec<i64> = assigned.iter().map(|a| a.document_id).collect();
    let names: Vec<&str> = assigned.iter().map(|a| a.name.as_str()).collect();
    let written = tx
        .execute(
            "UPDATE account_move SET name = v.name \
             FROM unnest($1::int8[], $2::text[]) AS v(id, name) WHERE account_move.id = v.id",
            &[&ids, &names],
        )
        .await?;
    if usize::try_from(written).ok() != Some(ids.len()) {
        return Err(Error::internal(format!(
            "Numbering wrote {written} of {} entry names.",
            ids.len()
        )));
    }
    // the number in the entry's history; every entry was unnamed before (checked above)
    tx.track_external(
        moves,
        "name",
        assigned
            .iter()
            .map(|a| (a.document_id, Cell::Null, text_cell(&a.name))),
    )
}

async fn invalidate(tx: &mut Transaction, moves: &Records) -> Result<()> {
    // the display name is computed from the state and the number, which change here by SQL
    tx.invalidate_records(moves, Some(&["state", "date", "name", "display_name"]))
        .await?;
    tx.invalidate_model("account.move.line", Some(&["parent_state", "date"]))
        .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> Date {
        Date::from_ymd(y, m, day).unwrap()
    }

    #[test]
    fn redating_follows_the_journal_reset() {
        use SequenceReset::{Month, Never, Year};
        // a later month: the end of the start month
        assert_eq!(
            shifted_date(d(2024, 2, 1), d(2026, 11, 16), Month, true, false).unwrap(),
            d(2024, 2, 29)
        );
        // the same month: today, never before the start
        assert_eq!(
            shifted_date(d(2026, 11, 1), d(2026, 11, 16), Month, true, false).unwrap(),
            d(2026, 11, 16)
        );
        assert_eq!(
            shifted_date(d(2026, 11, 20), d(2026, 11, 16), Month, true, false).unwrap(),
            d(2026, 11, 20)
        );
        // a yearly journal: the year end when today is in a later year
        assert_eq!(
            shifted_date(d(2024, 2, 1), d(2026, 11, 16), Year, true, false).unwrap(),
            d(2024, 12, 31)
        );
        // a journal that never numbered an entry follows the monthly rule
        assert_eq!(
            shifted_date(d(2024, 2, 1), d(2026, 11, 16), Year, false, false).unwrap(),
            d(2024, 2, 29)
        );
        // no reset: the start date
        assert_eq!(
            shifted_date(d(2024, 2, 1), d(2026, 11, 16), Never, true, false).unwrap(),
            d(2024, 2, 1)
        );
        // sales documents never go past today
        assert_eq!(
            shifted_date(d(2026, 11, 1), d(2026, 11, 16), Month, true, true).unwrap(),
            d(2026, 11, 16)
        );
        assert_eq!(
            shifted_date(d(2024, 2, 1), d(2026, 11, 16), Month, true, true).unwrap(),
            d(2024, 2, 29)
        );
    }
}
