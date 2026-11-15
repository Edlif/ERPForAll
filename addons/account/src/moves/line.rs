// SPDX-License-Identifier: MIT OR Apache-2.0
//! `account.move.line`: journal items of draft entries.

use std::collections::BTreeMap;

use erp_sdk::GeneratedInput;
use erp_sdk::prelude::*;

use super::{
    TEXT_LINE_TYPES, check_balanced, id_cell, refuse_state, skip_balance_check, unchecked,
};

/// A journal item. `balance` is the only stored amount (company currency); `debit` and `credit`
/// are virtual columns over it and `is_storno`. Header fields read through the entry
/// (`move_name`, `ref`, the company currency) are not stored on the line.
#[model(
    name = "account.move.line",
    description = "Journal Item",
    order = "date desc, move_id desc, id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    owner_only_stored_fields = [],
    constraints = [
        (
            "_draft_until_posting",
            "CHECK (parent_state = 'draft')",
            "Journal items stay in draft until posting is delivered (a later release)."
        ),
        (
            "_check_accountable_required_fields",
            "CHECK (display_type IN ('line_section', 'line_subsection', 'line_note') OR account_id IS NOT NULL)",
            "A journal item that carries an amount needs an account."
        ),
        (
            "_check_non_accountable_fields_null",
            "CHECK (display_type NOT IN ('line_section', 'line_subsection', 'line_note') OR (COALESCE(balance, 0) = 0 AND COALESCE(amount_currency, 0) = 0 AND account_id IS NULL))",
            "A section or note line has no amount and no account."
        ),
    ],
)]
pub struct AccountMoveLine {
    /// The entry; a line never moves to another entry and is deleted with it.
    #[field(
        many2one,
        comodel = "account.move",
        required = true,
        ondelete = "cascade",
        string = "Journal Entry"
    )]
    pub move_id: Option<RecId>,
    /// The entry's company.
    #[field(
        many2one,
        comodel = "res.company",
        related = "move_id.company_id",
        store = true,
        index = false,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// The entry's journal.
    #[field(
        many2one,
        comodel = "account.journal",
        related = "move_id.journal_id",
        store = true,
        index = false,
        string = "Journal"
    )]
    pub journal_id: Option<RecId>,
    /// The entry's accounting date.
    #[field(date, related = "move_id.date", store = true, string = "Date")]
    pub date: Option<Date>,
    /// The entry's state.
    #[field(
        selection,
        related = "move_id.state",
        store = true,
        readonly = true,
        string = "Status"
    )]
    pub parent_state: Option<Arc<str>>,
    /// The entry's number, read through the entry.
    #[field(char, related = "move_id.name", string = "Number")]
    pub move_name: Option<Arc<str>>,
    /// The entry's reference, read through the entry.
    #[field(char, related = "move_id.ref", string = "Reference")]
    pub r#ref: Option<Arc<str>>,
    /// The company currency, read through the entry.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "move_id.company_currency_id",
        string = "Company Currency"
    )]
    pub company_currency_id: Option<RecId>,
    /// The account; required on every line that carries an amount.
    #[field(
        many2one,
        comodel = "account.account",
        check_company = true,
        ondelete = "restrict",
        index = false,
        string = "Account"
    )]
    pub account_id: Option<RecId>,
    /// Partner of the line.
    #[field(
        many2one,
        comodel = "res.partner",
        check_company = true,
        ondelete = "restrict",
        index = false,
        string = "Partner"
    )]
    pub partner_id: Option<RecId>,
    /// Label.
    #[field(char, string = "Label")]
    pub name: Option<Arc<str>>,
    /// Order inside the entry.
    #[field(integer, default = 10)]
    pub sequence: i32,
    /// What the line is: an accounting line (`product`) or a section, subsection or note.
    #[field(
        selection,
        selection = [
            ("product", "Product"),
            ("line_section", "Section"),
            ("line_subsection", "Subsection"),
            ("line_note", "Note"),
        ],
        required = true,
        default = "product",
        string = "Display Type"
    )]
    pub display_type: Option<Arc<str>>,
    /// Currency of the line; the entry's when none is given.
    #[field(
        many2one,
        comodel = "res.currency",
        required = true,
        index = false,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Amount in the line currency; equal to the balance in the company currency.
    #[field(
        monetary,
        currency_field = "currency_id",
        string = "Amount in Currency"
    )]
    pub amount_currency: Decimal,
    /// Signed amount in the company currency: debit minus credit.
    #[field(monetary, currency_field = "company_currency_id", string = "Balance")]
    pub balance: Decimal,
    /// Negative amounts kept on their side instead of swapping debit and credit.
    #[field(boolean, copy = false, string = "Storno")]
    pub is_storno: bool,
    /// Debit, generated from the balance.
    #[field(
        monetary,
        currency_field = "company_currency_id",
        generated = "least(balance, 0) if is_storno else greatest(balance, 0)",
        input_adapted = true,
        string = "Debit"
    )]
    pub debit: Decimal,
    /// Credit, generated from the balance.
    #[field(
        monetary,
        currency_field = "company_currency_id",
        generated = "-greatest(balance, 0) if is_storno else -least(balance, 0)",
        input_adapted = true,
        string = "Credit"
    )]
    pub credit: Decimal,
    /// Due date of the line.
    #[field(date, string = "Due Date")]
    pub date_maturity: Option<Date>,
}

/// An amount cell as an exact decimal; empty is zero.
fn amount_of(cell: Option<&Cell>, field: &str) -> Result<Option<Decimal>> {
    match cell {
        None => Ok(None),
        Some(Cell::Null | Cell::Bool(false)) => Ok(Some(Decimal::ZERO)),
        Some(Cell::Decimal(value)) => Ok(Some(*value)),
        Some(Cell::Int(value)) => Ok(Some(Decimal::from(*value))),
        Some(other) => Err(Error::validation(format!(
            "{field} takes an exact amount, not {other:?}."
        ))),
    }
}

fn is_text_line(vals: &RawVals) -> bool {
    matches!(vals.get("display_type"), Some(Cell::Str(kind)) if TEXT_LINE_TYPES.contains(&kind.as_ref()))
}

/// The balance a row's debit and credit stand. `stored`
/// is the persisted (debit, credit) of a written line: a side written as zero keeps the other
/// side's stored amount, a non-zero side clears the other one (the reference's inverses).
fn balance_of(
    debit: Option<Decimal>,
    credit: Option<Decimal>,
    stored: Option<(Decimal, Decimal)>,
) -> Result<Decimal> {
    let overflow = || Error::validation("The debit and credit of a journal item are too large.");
    let (stored_debit, stored_credit) = stored.unwrap_or((Decimal::ZERO, Decimal::ZERO));
    match (debit, credit) {
        (Some(debit), Some(credit)) if !debit.is_zero() && !credit.is_zero() => {
            Err(Error::validation(format!(
                "A journal item cannot have both a debit and a credit (debit {debit}, credit \
                 {credit}): use two lines."
            )))
        }
        (Some(debit), Some(credit)) => debit.checked_sub(credit).ok_or_else(overflow),
        (Some(debit), None) if debit.is_zero() => Ok(-stored_credit),
        (Some(debit), None) => Ok(debit),
        (None, Some(credit)) if credit.is_zero() => Ok(stored_debit),
        (None, Some(credit)) => Ok(-credit),
        (None, None) => Err(Error::internal(
            "a journal item's debit and credit were translated without either",
        )),
    }
}

/// Translates a written debit or credit into the balance; a balance given with them must agree.
fn adapt_row(row: &mut RawVals, stored: Option<(Decimal, Decimal)>) -> Result<()> {
    let debit = amount_of(row.remove("debit").as_ref(), "debit")?;
    let credit = amount_of(row.remove("credit").as_ref(), "credit")?;
    if debit.is_none() && credit.is_none() {
        return Ok(());
    }
    let balance = balance_of(debit, credit, stored)?;
    match amount_of(row.get("balance"), "balance")? {
        Some(given) if given != balance => Err(Error::validation(format!(
            "The balance {given} of a journal item contradicts its debit and credit (balance \
             {balance})."
        ))),
        _ => {
            row.insert("balance".into(), Cell::Decimal(balance));
            Ok(())
        }
    }
}

/// The amounts of a company-currency line agree: the written one sets the other.
fn align_company_amounts(vals: &mut RawVals) -> Result<()> {
    let balance = amount_of(vals.get("balance"), "balance")?;
    let amount = amount_of(vals.get("amount_currency"), "amount_currency")?;
    match (balance, amount) {
        (Some(balance), Some(amount)) if balance != amount => Err(Error::validation(format!(
            "In the company currency the amount in currency equals the balance (balance \
             {balance}, amount in currency {amount})."
        ))),
        (Some(balance), None) => {
            vals.insert("amount_currency".into(), Cell::Decimal(balance));
            Ok(())
        }
        (None, Some(amount)) => {
            vals.insert("balance".into(), Cell::Decimal(amount));
            Ok(())
        }
        _ => Ok(()),
    }
}

const CONVERSION_NOT_DELIVERED: &str = "Converting between a line's currency and the company currency is not \
     delivered yet (next part of task a later release): give both the amount in currency and the balance.";

/// A foreign-currency line needs both amounts until the rate conversion is delivered.
fn check_foreign_amounts(vals: &RawVals) -> Result<()> {
    let balance = amount_of(vals.get("balance"), "balance")?.unwrap_or_default();
    let amount = amount_of(vals.get("amount_currency"), "amount_currency")?.unwrap_or_default();
    if balance.is_zero() != amount.is_zero() {
        return Err(Error::validation(CONVERSION_NOT_DELIVERED));
    }
    Ok(())
}

fn refuse_line_inputs(vals: &RawVals) -> Result<()> {
    refuse_state(vals, "parent_state")?;
    if matches!(vals.get("is_storno"), Some(Cell::Bool(true))) {
        return Err(Error::validation(
            "Storno journal items need the company's storno setting, which comes with the next part \
             of task a later release.",
        ));
    }
    Ok(())
}

/// (currency, company currency) of the entries of `move_ids`, by entry.
async fn move_currencies(
    tx: &mut Transaction,
    env: &Env,
    move_ids: &[i64],
) -> Result<BTreeMap<i64, (Option<i64>, Option<i64>)>> {
    let moves = env
        .records("account.move")?
        .browse(move_ids.iter().copied())?;
    let currencies = moves.cells(tx, "currency_id").await?;
    let companies = moves.cells(tx, "company_currency_id").await?;
    let mut by_move = BTreeMap::new();
    for ((id, currency), company) in moves.ids().into_iter().zip(currencies).zip(companies) {
        by_move.insert(
            id,
            (
                id_cell(Some(&currency), "currency_id")?,
                id_cell(Some(&company), "company_currency_id")?,
            ),
        );
    }
    Ok(by_move)
}

#[methods(AccountMoveLine, addon = "account")]
impl AccountMoveLine {
    /// Turns written debits and credits into balances before the generated-column guard.
    /// On a write, a side written as zero keeps the stored amount of the other side.
    #[method]
    async fn _adapt_generated_input(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        input: GeneratedInput,
    ) -> Result<Vec<RawVals>> {
        let create = input.is_create();
        let mut rows = input.into_rows();
        if create {
            for row in &mut rows {
                adapt_row(row, None)?;
            }
            return Ok(rows);
        }
        let balances = this.mapped(tx, AccountMoveLine::BALANCE).await?;
        let stornos = this.mapped(tx, AccountMoveLine::IS_STORNO).await?;
        for ((row, balance), storno) in rows.iter_mut().zip(balances).zip(stornos) {
            let (positive, negative) = (balance.max(Decimal::ZERO), balance.min(Decimal::ZERO));
            // the generated expressions of debit and credit, on the stored row
            let stored = if storno {
                (negative, -positive)
            } else {
                (positive, -negative)
            };
            adapt_row(row, Some(stored))?;
        }
        Ok(rows)
    }

    /// Fills the line currency from the entry and keeps company-currency amounts equal; the
    /// entries are checked for balance after the lines exist (unless skipped).
    #[method(override)]
    async fn create(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let env = this.env().clone();
        let skip = skip_balance_check(&env)?;
        let mut move_ids = Vec::with_capacity(vals_list.len());
        for vals in &vals_list {
            refuse_line_inputs(vals)?;
            if let Some(id) = id_cell(vals.get("move_id"), "move_id")? {
                move_ids.push(id);
            }
        }
        move_ids.sort_unstable();
        move_ids.dedup();
        let currencies = move_currencies(tx, &env, &move_ids).await?;
        for vals in &mut vals_list {
            let entry = id_cell(vals.get("move_id"), "move_id")?
                .and_then(|id| currencies.get(&id).copied());
            let Some((entry_currency, company_currency)) = entry else {
                continue; // the required check names the missing entry
            };
            let currency = match id_cell(vals.get("currency_id"), "currency_id")? {
                Some(currency) => Some(currency),
                None => {
                    if let Some(currency) = entry_currency {
                        vals.insert("currency_id".into(), Cell::Id(currency));
                    }
                    entry_currency
                }
            };
            if is_text_line(vals) {
                continue; // the CHECK refuses an amount or an account on a section or note
            }
            if currency == company_currency {
                align_company_amounts(vals)?;
            } else {
                check_foreign_amounts(vals)?;
            }
        }
        let created = sup.call(tx, this.as_records(), (vals_list,)).await?;
        if !skip {
            check_balanced(tx, &move_ids).await?;
        }
        Ok(created)
    }

    /// A line stays in its entry (/096); amounts are aligned per currency and the
    /// entries checked for balance after the write.
    #[method(override)]
    async fn write(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        mut vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        refuse_line_inputs(&vals)?;
        let env = this.env().clone();
        let skip = skip_balance_check(&env)?;
        let entries = this.mapped(tx, AccountMoveLine::MOVE_ID).await?;
        let mut move_ids: Vec<i64> = entries
            .iter()
            .filter_map(|id| id.and_then(|id| id.as_real()).map(|id| id.get()))
            .collect();
        if let Some(target) = id_cell(vals.get("move_id"), "move_id")? {
            if move_ids.iter().any(|id| *id != target) || move_ids.len() != this.len() {
                return Err(Error::validation(
                    "A journal item stays in the entry it was created in: delete it and add a new \
                     line to the other entry.",
                ));
            }
            vals.remove("move_id");
        }
        move_ids.sort_unstable();
        move_ids.dedup();
        let amounts = ["balance", "amount_currency", "currency_id"]
            .iter()
            .any(|f| vals.contains(f));
        if !amounts {
            return sup.call(tx, this.as_records(), (vals,)).await;
        }
        // one write per (currency, company currency, stored balance when only the currency changes)
        let currencies = match id_cell(vals.get("currency_id"), "currency_id")? {
            Some(currency) => vec![Some(currency); this.len()],
            None => this
                .mapped(tx, AccountMoveLine::CURRENCY_ID)
                .await?
                .into_iter()
                .map(|id| id.and_then(|id| id.as_real()).map(|id| id.get()))
                .collect(),
        };
        let company_currencies = this
            .mapped(tx, AccountMoveLine::COMPANY_CURRENCY_ID)
            .await?;
        let text_lines = this.mapped(tx, AccountMoveLine::DISPLAY_TYPE).await?;
        let currency_only = !vals.contains("balance") && !vals.contains("amount_currency");
        let balances = if currency_only {
            this.mapped(tx, AccountMoveLine::BALANCE).await?
        } else {
            vec![Decimal::ZERO; this.len()]
        };
        let mut groups: BTreeMap<(bool, Decimal), Vec<i64>> = BTreeMap::new();
        let rows = this
            .ids()
            .into_iter()
            .zip(text_lines)
            .zip(company_currencies)
            .zip(currencies)
            .zip(balances);
        for ((((id, kind), company), currency), balance) in rows {
            let text = kind
                .as_deref()
                .is_some_and(|kind| TEXT_LINE_TYPES.contains(&kind));
            let company = company.and_then(|id| id.as_real()).map(|id| id.get());
            let same = !text && currency == company;
            if !text && !same {
                check_foreign_amounts(&vals)?;
                // a currency-only write would keep the stored amounts under the new currency
                if currency_only && !balance.is_zero() {
                    return Err(Error::validation(CONVERSION_NOT_DELIVERED));
                }
            }
            groups.entry((same, balance)).or_default().push(id);
        }
        // the chain continues once; further groups re-enter the public write with aligned values,
        // which form a single group there (the balance check runs once below)
        let mut sup = Some(sup);
        for ((same, stored_balance), ids) in groups {
            let mut group_vals = vals.clone();
            if same {
                if currency_only {
                    group_vals.insert("amount_currency".into(), Cell::Decimal(stored_balance));
                } else {
                    align_company_amounts(&mut group_vals)?;
                }
            }
            let records = this.as_records().browse(ids)?;
            match sup.take() {
                Some(next) => next.call(tx, &records, (group_vals,)).await?,
                None => unchecked(&records).write(tx, group_vals).await?,
            }
        }
        if !skip && (vals.contains("balance") || vals.contains("amount_currency")) {
            check_balanced(tx, &move_ids).await?;
        }
        Ok(())
    }

    /// Deleting lines of a draft rechecks the balance of what remains of their entries.
    #[method(override)]
    async fn unlink(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let skip = skip_balance_check(this.env())?;
        let mut move_ids: Vec<i64> = this
            .mapped(tx, AccountMoveLine::MOVE_ID)
            .await?
            .into_iter()
            .filter_map(|id| id.and_then(|id| id.as_real()).map(|id| id.get()))
            .collect();
        move_ids.sort_unstable();
        move_ids.dedup();
        sup.call(tx, this.as_records(), ()).await?;
        if !skip {
            check_balanced(tx, &move_ids).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(value: i64) -> Decimal {
        Decimal::from(value)
    }

    #[test]
    fn one_side_written_keeps_or_clears_the_other() {
        assert_eq!(balance_of(Some(d(150)), None, None).unwrap(), d(150));
        assert_eq!(balance_of(None, Some(d(150)), None).unwrap(), d(-150));
        assert_eq!(
            balance_of(Some(d(10)), None, Some((d(0), d(20)))).unwrap(),
            d(10)
        );
        assert_eq!(
            balance_of(Some(d(0)), None, Some((d(0), d(20)))).unwrap(),
            d(-20)
        );
        assert_eq!(
            balance_of(None, Some(d(0)), Some((d(7), d(0)))).unwrap(),
            d(7)
        );
        assert_eq!(balance_of(Some(d(0)), Some(d(30)), None).unwrap(), d(-30));
        assert!(balance_of(Some(d(1)), Some(d(1)), None).is_err());
    }
}
