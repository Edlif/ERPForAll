// SPDX-License-Identifier: MIT OR Apache-2.0
//! An entry ([`AccountMove`]) is the header: journal, company, currency, date and state. Its items
//! ([`AccountMoveLine`]) carry one stored amount, `balance` in the company currency; `debit` and
//! `credit` are virtual generated columns derived from it, and a written debit or
//! credit is turned into a balance by the line's generated-input adapter before the
//! ordinary write runs. On a line in another currency than the company's, the amount that is not
//! given is converted through the rate at the entry date (see the guide's
//! "Lines in another currency").
//!
//! This part delivers drafts only (lead ruling V1-V5 of 2026-11-13): an entry can be created,
//! edited, checked for balance and deleted. Nothing posts: [`AccountMove`]'s `action_post` and
//! every write of a state other than draft are refused naming [`POST_TASK`], and two named CHECK
//! constraints keep the tables draft-only against raw SQL until replaces them with its
//! posted-row guards. Rules the database will enforce with (company of accounts and
//! journals, the line's entry never changing, amount in currency equal to the balance in the company
//! currency) are checked by the ORM in this part.

mod line;

pub use line::AccountMoveLine;

use erp_sdk::prelude::*;

/// The task that adds posting, the posted-row guards and numbering at posting.
pub const POST_TASK: &str = "a later release";

/// The context key that skips the balance check of drafts (`check_move_validity`).
pub const CHECK_KEY: &str = "check_move_validity";

/// The selection keys of a line's `display_type` that carry no amount and no account.
pub(crate) const TEXT_LINE_TYPES: [&str; 3] = ["line_section", "line_subsection", "line_note"];

/// `account.move`: a journal entry. Only drafts exist until.
#[model(
    name = "account.move",
    description = "Journal Entry",
    order = "date desc, name desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    owner_only_stored_fields = [],
    constraints = [(
        "_draft_until_posting",
        "CHECK (state = 'draft')",
        "Journal entries stay in draft until posting is delivered (a later release)."
    )],
)]
pub struct AccountMove {
    /// Number of the entry; empty on a draft that was never numbered (numbering comes with posting).
    #[field(char, copy = false, string = "Number")]
    pub name: Option<Arc<str>>,
    /// Reference of the entry.
    #[field(char, copy = false, string = "Reference")]
    pub r#ref: Option<Arc<str>>,
    /// Accounting date; today when none is given.
    #[field(date, required = true, string = "Date")]
    pub date: Option<Date>,
    /// Draft, posted or cancelled; only the posting service may change it (task).
    #[field(
        selection,
        selection = [("draft", "Draft"), ("posted", "Posted"), ("cancel", "Cancelled")],
        required = true,
        readonly = true,
        copy = false,
        default = "draft",
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
    /// The kind of entry; invoices and receipts add their kinds with the invoicing slices.
    #[field(
        selection,
        selection = [("entry", "Journal Entry")],
        required = true,
        default = "entry",
        string = "Type"
    )]
    pub move_type: Option<Arc<str>>,
    /// The journal; the first miscellaneous journal of the company when none is given.
    #[field(
        many2one,
        comodel = "account.journal",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Journal"
    )]
    pub journal_id: Option<RecId>,
    /// The company; follows the journal when none is given.
    #[field(many2one, comodel = "res.company", required = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// Currency of the entry: the journal's, else the company's.
    #[field(
        many2one,
        comodel = "res.currency",
        required = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// The company currency, read through the company.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        string = "Company Currency"
    )]
    pub company_currency_id: Option<RecId>,
    /// Partner of the entry.
    #[field(
        many2one,
        comodel = "res.partner",
        check_company = true,
        ondelete = "restrict",
        index = "btree_not_null",
        string = "Partner"
    )]
    pub partner_id: Option<RecId>,
    /// The journal items; deleted with the entry.
    #[field(
        one2many,
        comodel = "account.move.line",
        inverse_name = "move_id",
        copy = true,
        string = "Journal Items"
    )]
    pub line_ids: Vec<RecId>,
}

#[methods(AccountMove, addon = "account")]
impl AccountMove {
    /// Fills the defaults of a draft and checks the balance of the new
    /// entries once, after their lines.
    #[method(override)]
    async fn create(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let env = this.env().clone();
        let skip = skip_balance_check(&env)?;
        for vals in &mut vals_list {
            refuse_state(vals, "state")?;
            fill_defaults(tx, &env, vals).await?;
        }
        let records = unchecked(this.as_records());
        let created = sup.call(tx, &records, (vals_list,)).await?;
        if !skip {
            check_balanced(tx, &created.ids()).await?;
        }
        Ok(created.with_env(env))
    }

    /// A journal change of a never-posted draft clears its number; line commands
    /// are checked for balance once, after all of them ran.
    #[method(override)]
    async fn write(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        mut vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        refuse_state(&vals, "state")?;
        let skip = skip_balance_check(this.env())?;
        if vals.contains("journal_id") && !vals.contains("name") {
            vals.insert("name".into(), Cell::Null);
        }
        let lines_changed = vals.commands("line_ids").is_some();
        sup.call(tx, &unchecked(this.as_records()), (vals,)).await?;
        if lines_changed && !skip {
            check_balanced(tx, &this.ids()).await?;
        }
        Ok(())
    }

    /// Deleting a draft deletes its lines; no balance check runs on the way.
    #[method(override)]
    async fn unlink(this: &RecordSet<AccountMove>, tx: &mut Transaction, sup: Super) -> Result<()> {
        sup.call(tx, &unchecked(this.as_records()), ()).await
    }

    /// Posting is not delivered yet: refused naming the task that adds it.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_post(_this: &RecordSet<AccountMove>, _tx: &mut Transaction) -> Result<()> {
        Err(posting_refusal())
    }
}

/// The refusal of every way to post or cancel an entry before [`POST_TASK`].
pub(crate) fn posting_refusal() -> Error {
    Error::user(format!(
        "Journal entries can only be saved as drafts for now: posting comes with task {POST_TASK}."
    ))
}

/// Refuses a caller-supplied state other than draft (`field` is `state` on entries,
/// `parent_state` on lines).
pub(crate) fn refuse_state(vals: &RawVals, field: &str) -> Result<()> {
    match vals.get(field) {
        None => Ok(()),
        Some(Cell::Str(state)) if field == "state" && state.as_ref() == "draft" => Ok(()),
        Some(_) => Err(posting_refusal()),
    }
}

/// True when the caller's context skips the balance check; any value but a boolean is refused.
pub(crate) fn skip_balance_check(env: &Env) -> Result<bool> {
    match env.context().get(CHECK_KEY) {
        None | Some(Value::Bool(true)) => Ok(false),
        Some(Value::Bool(false)) => Ok(true),
        Some(other) => Err(Error::programming(format!(
            "The context key {CHECK_KEY} takes true or false, not {other:?}."
        ))),
    }
}

/// The same records with the balance check skipped, so nested line commands do not check an entry
/// that is only half written; the caller checks once at the end.
pub(crate) fn unchecked(records: &Records) -> Records {
    records.with_context([(CHECK_KEY, Value::Bool(false))])
}

/// Refuses the entries among `move_ids` whose balances do not sum to zero. Amounts are exact on
/// the company currency increment, so the sum is compared with zero without rounding.
/// One aggregate for all the entries.
pub(crate) async fn check_balanced(tx: &mut Transaction, move_ids: &[i64]) -> Result<()> {
    if move_ids.is_empty() {
        return Ok(());
    }
    tx.flush().await?;
    let rows = tx
        .query(
            "SELECT move_id, SUM(balance)::text FROM account_move_line \
             WHERE move_id = ANY($1::int8[]) GROUP BY move_id HAVING SUM(balance) <> 0 \
             ORDER BY move_id",
            &[&move_ids],
        )
        .await?;
    let mut unbalanced = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row
            .try_get(0)
            .map_err(|e| Error::db(format!("An entry id could not be read: {e}")))?;
        let difference: String = row
            .try_get(1)
            .map_err(|e| Error::db(format!("An entry balance could not be read: {e}")))?;
        unbalanced.push((id, difference));
    }
    match unbalanced.as_slice() {
        [] => Ok(()),
        [(id, difference)] => Err(Error::user(format!(
            "Debits and credits must be equal before saving; in entry {id} they differ by {difference}."
        ))),
        many => Err(Error::user(format!(
            "Debits and credits must be equal before saving; they differ in these entries:\n{}",
            many.iter()
                .map(|(id, difference)| format!("- entry {id}: off by {difference}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))),
    }
}

/// The record id in a many2one cell, `None` when empty.
pub(crate) fn id_cell(cell: Option<&Cell>, field: &str) -> Result<Option<i64>> {
    match cell {
        None | Some(Cell::Null | Cell::Bool(false)) => Ok(None),
        Some(Cell::Id(id)) => Ok(Some(*id)),
        Some(Cell::Int(id)) => Ok(Some(i64::from(*id))),
        Some(other) => Err(Error::validation(format!(
            "{field} takes a record id, not {other:?}."
        ))),
    }
}

/// Journal, company, currency, date and type of a new draft, in the order the reference derives
/// them: the journal from the company (the given one, else the active one), the company from the
/// journal, the currency from the journal then the company.
async fn fill_defaults(tx: &mut Transaction, env: &Env, vals: &mut RawVals) -> Result<()> {
    let given_company = id_cell(vals.get("company_id"), "company_id")?;
    let journal = match id_cell(vals.get("journal_id"), "journal_id")? {
        Some(journal) => journal,
        None => {
            let company = match given_company {
                Some(company) => company,
                None => env.company_id()?.ok_or_else(|| {
                    Error::user("No company is active: give the journal entry a company.")
                })?,
            };
            let currency = id_cell(vals.get("currency_id"), "currency_id")?;
            let journal = default_journal(tx, env, company, currency).await?;
            vals.insert("journal_id".into(), Cell::Id(journal));
            journal
        }
    };
    let journals = env.records("account.journal")?;
    let record = journals.browse([journal])?;
    if given_company.is_none() {
        let company = match env.company_id()? {
            // the active company when the journal belongs to it or one of its parents
            Some(active) => {
                let shared = journals
                    .search(
                        tx,
                        &Domain::and([
                            Domain::condition("id", Operator::Eq, journal)?,
                            Domain::condition("company_id", Operator::ParentOf, active)?,
                        ]),
                    )
                    .await?;
                if shared.is_empty() {
                    id_cell(Some(&record.cell(tx, "company_id").await?), "company_id")?
                } else {
                    Some(active)
                }
            }
            None => id_cell(Some(&record.cell(tx, "company_id").await?), "company_id")?,
        };
        if let Some(company) = company {
            vals.insert("company_id".into(), Cell::Id(company));
        }
    }
    if id_cell(vals.get("currency_id"), "currency_id")?.is_none() {
        let currency = match id_cell(Some(&record.cell(tx, "currency_id").await?), "currency_id")? {
            Some(currency) => Some(currency),
            None => match id_cell(vals.get("company_id"), "company_id")? {
                Some(company) => id_cell(
                    Some(
                        &env.records("res.company")?
                            .browse([company])?
                            .cell(tx, "currency_id")
                            .await?,
                    ),
                    "currency_id",
                )?,
                None => None,
            },
        };
        if let Some(currency) = currency {
            vals.insert("currency_id".into(), Cell::Id(currency));
        }
    }
    if matches!(
        vals.get("date"),
        None | Some(Cell::Null | Cell::Bool(false))
    ) {
        vals.insert("date".into(), Cell::Date(env.context_today()?));
    }
    Ok(())
}

/// The first miscellaneous journal the caller can read in `company` or one of its parents, in the
/// journal order; one in `currency` first when a currency is given.
async fn default_journal(
    tx: &mut Transaction,
    env: &Env,
    company: i64,
    currency: Option<i64>,
) -> Result<i64> {
    let found = env
        .records("account.journal")?
        .search(
            tx,
            &Domain::and([
                Domain::condition("type", Operator::Eq, "general")?,
                Domain::condition("company_id", Operator::ParentOf, company)?,
            ]),
        )
        .await?;
    let ids = found.ids();
    if let Some(currency) = currency {
        let currencies = found.cells(tx, "currency_id").await?;
        if let Some(position) = currencies.iter().position(|c| *c == Cell::Id(currency)) {
            return Ok(ids[position]);
        }
    }
    ids.first().copied().ok_or_else(|| {
        Error::user(format!(
            "No miscellaneous journal is available for company {company}: create one in \
             Accounting, Configuration, Journals, or give the entry a journal."
        ))
    })
}
