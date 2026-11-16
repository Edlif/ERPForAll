// SPDX-License-Identifier: MIT OR Apache-2.0
//! An invoice is a draft entry whose `move_type` is one of [`INVOICE_TYPES`]. The user edits its
//! product lines (`invoice_line_ids`); after every create and every relevant write of a draft
//! invoice [`sync_lines`] rebuilds the rest of `line_ids` from them: one tax line per tax
//! repartition line and account, one receivable or payable line that balances the entry, and the
//! header totals. Posting checks live in [`post`] and are called by the posting service.
//!
//! Not in this cut (each refused or left visible, never guessed): receipts, payment terms
//! (the due date is the invoice date unless set by hand), product account mappings
//! (product lines take the journal's default account), fiscal positions, cash
//! rounding, payments (the amount due is the total, the payment state `not_paid`).

mod onchange;
pub mod post;
mod sync;

pub use sync::sync_lines;

use erp_sdk::Command;
use erp_sdk::prelude::*;

use crate::moves::id_cell;

/// The move types this module treats as invoices.
pub const INVOICE_TYPES: [&str; 4] = ["out_invoice", "out_refund", "in_invoice", "in_refund"];

/// The task that adds product income and expense accounts.
pub const ACCOUNT_MAPPING_TASK: &str = "a later release";

/// The task that adds groups of taxes with their children.
pub const TAX_CHILDREN_TASK: &str = "a later release";

/// Fields whose change on a draft invoice rebuilds its tax, receivable/payable and total values.
const SYNC_FIELDS: [&str; 8] = [
    "partner_id",
    "currency_id",
    "move_type",
    "journal_id",
    "invoice_date",
    "invoice_date_due",
    "date",
    "company_id",
];

/// True for the move types of [`INVOICE_TYPES`].
pub fn is_invoice(move_type: &str) -> bool {
    INVOICE_TYPES.contains(&move_type)
}

/// True for customer documents (invoice, credit note).
pub fn is_sale(move_type: &str) -> bool {
    matches!(move_type, "out_invoice" | "out_refund")
}

/// True for credit notes.
pub fn is_refund(move_type: &str) -> bool {
    matches!(move_type, "out_refund" | "in_refund")
}

/// `direction_sign`: -1 for a customer invoice and a vendor credit note, +1 for the other types.
/// A product line's amount in currency is its subtotal times this sign, so a
/// customer invoice credits its income and debits its receivable.
pub fn direction_sign(move_type: &str) -> Decimal {
    if matches!(move_type, "out_invoice" | "in_refund") {
        -Decimal::ONE
    } else {
        Decimal::ONE
    }
}

/// The journal type an invoice of `move_type` belongs to.
pub fn journal_type(move_type: &str) -> &'static str {
    if is_sale(move_type) {
        "sale"
    } else {
        "purchase"
    }
}

fn text_of(cell: Option<&Cell>) -> Option<Arc<str>> {
    match cell {
        Some(Cell::Str(text)) => Some(text.clone()),
        _ => None,
    }
}

/// The move type a new entry will get: the given one, else the `default_move_type` context key.
fn new_move_type(env: &Env, vals: &RawVals) -> Result<Option<Arc<str>>> {
    if let Some(given) = vals.get("move_type") {
        return Ok(text_of(Some(given)));
    }
    match env.context().get("default_move_type") {
        None | Some(Value::False | Value::Bool(false)) => Ok(None),
        Some(Value::Str(kind)) => Ok(Some(kind.clone())),
        Some(other) => Err(Error::programming(format!(
            "The context key default_move_type takes a move type, not {other:?}."
        ))),
    }
}

/// Fills what a new draft invoice needs before the entry defaults run: its move type, a sale or
/// purchase journal, the accounting date from the invoice date and the account
/// of new product lines. Does nothing for other entries.
pub(crate) async fn prepare_create(
    tx: &mut Transaction,
    env: &Env,
    vals: &mut RawVals,
) -> Result<()> {
    let Some(move_type) = new_move_type(env, vals)?.filter(|t| is_invoice(t)) else {
        return Ok(());
    };
    vals.insert("move_type".into(), Cell::Str(move_type.clone()));
    let journal = match id_cell(vals.get("journal_id"), "journal_id")? {
        Some(journal) => journal,
        None => {
            let company = match id_cell(vals.get("company_id"), "company_id")? {
                Some(company) => company,
                None => env.company_id()?.ok_or_else(|| {
                    Error::user("No company is active: give the invoice a company.")
                })?,
            };
            let journal = invoice_journal(tx, env, company, &move_type).await?;
            vals.insert("journal_id".into(), Cell::Id(journal));
            journal
        }
    };
    date_from_invoice_date(vals);
    fill_line_accounts(tx, env, vals, journal).await
}

/// Prepares a write of draft entries: fills the account of new product lines and the accounting
/// date from the invoice date. True when the write touches invoices in a way that needs
/// [`sync_lines`] afterwards.
pub(crate) async fn prepare_write(
    tx: &mut Transaction,
    records: &Records,
    vals: &mut RawVals,
) -> Result<bool> {
    let lines = ["line_ids", "invoice_line_ids"]
        .iter()
        .any(|f| vals.commands(f).is_some());
    if !lines && !SYNC_FIELDS.iter().any(|f| vals.contains(f)) {
        return Ok(false);
    }
    let invoices = match text_of(vals.get("move_type")) {
        Some(kind) => is_invoice(&kind),
        None => records
            .cells(tx, "move_type")
            .await?
            .iter()
            .any(|c| text_of(Some(c)).is_some_and(|k| is_invoice(&k))),
    };
    if !invoices {
        return Ok(false);
    }
    date_from_invoice_date(vals);
    if has_new_lines(vals) {
        let journal = match id_cell(vals.get("journal_id"), "journal_id")? {
            Some(journal) => journal,
            None => {
                let journals = records.cells(tx, "journal_id").await?;
                match journals.as_slice() {
                    [one] => id_cell(Some(one), "journal_id")?
                        .ok_or_else(|| Error::invariant("an invoice has no journal"))?,
                    _ => {
                        return Err(Error::user("Add invoice lines to one invoice at a time."));
                    }
                }
            }
        };
        fill_line_accounts(tx, records.env(), vals, journal).await?;
    }
    Ok(true)
}

/// The accounting date follows the invoice date when only the invoice date is given
/// (partial: the vendor bill date rules come later).
fn date_from_invoice_date(vals: &mut RawVals) {
    if let Some(Cell::Date(date)) = vals.get("invoice_date").cloned()
        && !vals.contains("date")
    {
        vals.insert("date".into(), Cell::Date(date));
    }
}

fn has_new_lines(vals: &RawVals) -> bool {
    ["line_ids", "invoice_line_ids"].iter().any(|f| {
        vals.commands(f)
            .is_some_and(|cmds| cmds.iter().any(|c| matches!(c, Command::Create(_))))
    })
}

/// The first sale or purchase journal of `company` or one of its parents, in journal order.
async fn invoice_journal(
    tx: &mut Transaction,
    env: &Env,
    company: i64,
    move_type: &str,
) -> Result<i64> {
    let kind = journal_type(move_type);
    let found = env
        .records("account.journal")?
        .search(
            tx,
            &Domain::and([
                Domain::condition("type", Operator::Eq, kind)?,
                Domain::condition("company_id", Operator::ParentOf, company)?,
            ]),
        )
        .await?;
    found.ids().first().copied().ok_or_else(|| {
        Error::user(format!(
            "No {kind} journal is available for company {company}: create one in Accounting, \
             Configuration, Journals."
        ))
    })
}

/// New product lines without an account take the journal's default account (
/// partial: product accounts come with [`ACCOUNT_MAPPING_TASK`]); without one the save is refused
/// with what to configure.
async fn fill_line_accounts(
    tx: &mut Transaction,
    env: &Env,
    vals: &mut RawVals,
    journal: i64,
) -> Result<()> {
    let mut default: Option<Option<i64>> = None;
    for field in ["line_ids", "invoice_line_ids"] {
        let Some(mut commands) = vals.remove_commands(field) else {
            continue;
        };
        for command in &mut commands {
            let Command::Create(line) = command else {
                continue;
            };
            let product = match line.get("display_type") {
                None | Some(Cell::Null) => true,
                Some(Cell::Str(kind)) => kind.as_ref() == "product",
                Some(_) => false,
            };
            if !product || id_cell(line.get("account_id"), "account_id")?.is_some() {
                continue;
            }
            let account = match default {
                Some(account) => account,
                None => {
                    let cell = env
                        .records("account.journal")?
                        .browse([journal])?
                        .cell(tx, "default_account_id")
                        .await?;
                    let account = id_cell(Some(&cell), "default_account_id")?;
                    default = Some(account);
                    account
                }
            };
            let account = account.ok_or_else(|| {
                Error::user(format!(
                    "Choose an account on each invoice line, or give journal {journal} a default \
                     account in Accounting, Configuration, Journals (product accounts come with \
                     task {ACCOUNT_MAPPING_TASK})."
                ))
            })?;
            line.insert("account_id".into(), Cell::Id(account));
        }
        vals.insert_commands(field.into(), commands);
    }
    Ok(())
}
