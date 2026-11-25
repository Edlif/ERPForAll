// SPDX-License-Identifier: MIT OR Apache-2.0
//! Recurring & Deferred Entries (`account.recurring.entry`) for automated periodic postings,
//! deferred revenue recognition, and prepaid expense amortization.

use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::fiscal::calendar::add_months;
use crate::moves::post_moves;

/// Pure exact-decimal period installment allocator conserving `sum == total_amount` across
/// `1..=total_periods` (last period absorbs rounding remainder).
pub fn compute_deferred_period_amount(
    total_amount: Decimal,
    total_periods: i32,
    period_index: i32,
) -> Result<Decimal> {
    if total_amount <= Decimal::ZERO {
        return Err(Error::user("Total amount must be greater than zero."));
    }
    if total_periods <= 0 {
        return Err(Error::user("Total periods must be greater than zero."));
    }
    if period_index < 1 || period_index > total_periods {
        return Err(Error::user("Period index is out of range."));
    }
    let base = (total_amount / Decimal::from(total_periods)).round_dp(2);
    if period_index == total_periods {
        Ok(total_amount - base * Decimal::from(total_periods - 1))
    } else {
        Ok(base)
    }
}

fn default_today(_ctx: &erp_sdk::DefaultContext) -> Value {
    Value::Date(Date::today())
}

/// `account.recurring.entry`: schedule that posts periodic recurring or deferred journal entries.
#[model(
    name = "account.recurring.entry",
    description = "Recurring / Deferred Entry",
    order = "start_date desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountRecurringEntry {
    /// Schedule description (e.g. "Prepaid Office Rent Amortization").
    #[field(char, required = true, string = "Name")]
    pub name: Option<Arc<str>>,
    /// Classification of this periodic schedule.
    #[field(
        selection,
        selection = [
            ("recurring", "Recurring Entry"),
            ("deferred_revenue", "Deferred Revenue"),
            ("deferred_expense", "Deferred Expense"),
        ],
        required = true,
        default = "deferred_expense",
        string = "Schedule Type"
    )]
    pub entry_kind: Option<Arc<str>>,
    /// Owning company.
    #[field(many2one, comodel = "res.company", required = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// Company currency.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Miscellaneous journal for generated entries.
    #[field(
        many2one,
        comodel = "account.journal",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Journal"
    )]
    pub journal_id: Option<RecId>,
    /// Account debited in each period entry.
    #[field(
        many2one,
        comodel = "account.account",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Debit Account"
    )]
    pub debit_account_id: Option<RecId>,
    /// Account credited in each period entry.
    #[field(
        many2one,
        comodel = "account.account",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Credit Account"
    )]
    pub credit_account_id: Option<RecId>,
    /// Optional partner copied onto generated entries.
    #[field(
        many2one,
        comodel = "res.partner",
        check_company = true,
        string = "Partner"
    )]
    pub partner_id: Option<RecId>,
    /// First posting date.
    #[field(date, required = true, default_fn = default_today, string = "Start Date")]
    pub start_date: Option<Date>,
    /// Months between postings (1 = monthly, 3 = quarterly).
    #[field(integer, required = true, default = 1, string = "Interval (Months)")]
    pub period_months: i32,
    /// Total number of scheduled postings.
    #[field(integer, required = true, default = 12, string = "Total Periods")]
    pub total_periods: i32,
    /// Total amount recognized across all periods.
    #[field(monetary, currency_field = "currency_id", string = "Total Amount")]
    pub total_amount: Decimal,
    /// Number of periods already posted.
    #[field(
        integer,
        readonly = true,
        copy = false,
        default = 0,
        string = "Posted Periods"
    )]
    pub posted_periods: i32,
    /// Next scheduled posting date.
    #[field(date, readonly = true, copy = false, string = "Next Posting Date")]
    pub next_date: Option<Date>,
    /// Most recent generated journal entry.
    #[field(
        many2one,
        comodel = "account.move",
        readonly = true,
        copy = false,
        ondelete = "set null",
        string = "Last Journal Entry"
    )]
    pub last_move_id: Option<RecId>,
    /// Schedule status.
    #[field(
        selection,
        selection = [
            ("draft", "Draft"),
            ("running", "Running"),
            ("done", "Completed"),
        ],
        required = true,
        readonly = true,
        copy = false,
        default = "draft",
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
}

#[methods(AccountRecurringEntry, addon = "account")]
impl AccountRecurringEntry {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountRecurringEntry>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<erp_sdk::ValueMap> {
        let mut values: erp_sdk::ValueMap =
            sup.call(tx, this.as_records(), (names.clone(),)).await?;
        if let Some(cid) = this.env().company_id()? {
            if names.iter().any(|n| n == "company_id") && !values.contains_key("company_id") {
                values.insert("company_id".to_owned(), Value::Int(cid));
            }
            if names.iter().any(|n| n == "journal_id") && !values.contains_key("journal_id") {
                let rows = tx
                    .query(
                        "SELECT id::int8 FROM account_journal \
                         WHERE company_id = $1 AND type = 'general' AND active = true \
                         ORDER BY sequence, id LIMIT 1",
                        &[&cid],
                    )
                    .await?;
                if let Some(jid) = rows.first().and_then(|r| r.try_get::<_, i64>(0).ok()) {
                    values.insert("journal_id".to_owned(), Value::Int(jid));
                }
            }
        }
        Ok(values)
    }

    /// Activates the schedule and sets `next_date = start_date`.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_start(
        this: &RecordSet<AccountRecurringEntry>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Write).await?;
        let rid = rec.as_records().ids()[0];
        let total = rec.get(tx, AccountRecurringEntry::TOTAL_AMOUNT).await?;
        let periods = rec.get(tx, AccountRecurringEntry::TOTAL_PERIODS).await?;
        let interval = rec.get(tx, AccountRecurringEntry::PERIOD_MONTHS).await?;
        if total <= Decimal::ZERO {
            return Err(Error::user("Total amount must be greater than zero."));
        }
        if periods <= 0 || interval <= 0 {
            return Err(Error::user(
                "Total periods and interval months must be greater than zero.",
            ));
        }
        let start = rec
            .get(tx, AccountRecurringEntry::START_DATE)
            .await?
            .unwrap_or_else(Date::today);
        tx.execute(
            "UPDATE account_recurring_entry \
             SET state = 'running', next_date = COALESCE(next_date, $2), \
                 write_date = now() AT TIME ZONE 'UTC' \
             WHERE id = $1",
            &[&rid, &start],
        )
        .await?;
        tx.invalidate_records(rec.as_records(), Some(&["state", "next_date"]))
            .await
    }

    /// Generates and posts the next periodic journal entry and advances the schedule.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_post_next_entry(
        this: &RecordSet<AccountRecurringEntry>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Write).await?;
        let rid = rec.as_records().ids()[0];
        let state = rec.get(tx, AccountRecurringEntry::STATE).await?;
        if state.as_deref() != Some("running") {
            return Err(Error::user(
                "Start the schedule before posting periodic entries.",
            ));
        }
        let posted = rec.get(tx, AccountRecurringEntry::POSTED_PERIODS).await?;
        let total_periods = rec.get(tx, AccountRecurringEntry::TOTAL_PERIODS).await?;
        if posted >= total_periods {
            return Err(Error::user(
                "All scheduled periods have already been posted.",
            ));
        }
        let next_idx = posted + 1;
        let total_amount = rec.get(tx, AccountRecurringEntry::TOTAL_AMOUNT).await?;
        let amount = compute_deferred_period_amount(total_amount, total_periods, next_idx)?;
        let interval = rec.get(tx, AccountRecurringEntry::PERIOD_MONTHS).await?;
        let post_date = rec
            .get(tx, AccountRecurringEntry::NEXT_DATE)
            .await?
            .or(rec.get(tx, AccountRecurringEntry::START_DATE).await?)
            .unwrap_or_else(Date::today);
        let name = rec
            .get(tx, AccountRecurringEntry::NAME)
            .await?
            .unwrap_or_else(|| Arc::from("Recurring Entry"));
        let company_id = rec
            .get(tx, AccountRecurringEntry::COMPANY_ID)
            .await?
            .ok_or_else(|| Error::validation("Schedule has no company."))?
            .get();
        let currency_id = rec
            .get(tx, AccountRecurringEntry::CURRENCY_ID)
            .await?
            .ok_or_else(|| Error::validation("Schedule has no currency."))?
            .get();
        let journal_id = rec
            .get(tx, AccountRecurringEntry::JOURNAL_ID)
            .await?
            .ok_or_else(|| Error::validation("Schedule has no journal."))?
            .get();
        let debit_acc = rec
            .get(tx, AccountRecurringEntry::DEBIT_ACCOUNT_ID)
            .await?
            .ok_or_else(|| Error::validation("Schedule has no debit account."))?
            .get();
        let credit_acc = rec
            .get(tx, AccountRecurringEntry::CREDIT_ACCOUNT_ID)
            .await?
            .ok_or_else(|| Error::validation("Schedule has no credit account."))?
            .get();
        let partner_id = rec
            .get(tx, AccountRecurringEntry::PARTNER_ID)
            .await?
            .map(|p| p.get());

        let env = rec.env().with_context_replaced(erp_sdk::Context::default());
        let label = format!("{name} ({next_idx}/{total_periods})");
        let mut mvals = RawVals::new();
        mvals.insert("move_type".into(), Cell::Str("entry".into()));
        mvals.insert("journal_id".into(), Cell::Id(journal_id));
        mvals.insert("company_id".into(), Cell::Id(company_id));
        mvals.insert("currency_id".into(), Cell::Id(currency_id));
        mvals.insert("date".into(), Cell::Date(post_date));
        mvals.insert("ref".into(), Cell::Str(label.as_str().into()));
        if let Some(pid) = partner_id {
            mvals.insert("partner_id".into(), Cell::Id(pid));
        }

        let unchecked_moves = crate::moves::unchecked(&env.records("account.move")?);
        let created = unchecked_moves.create(tx, vec![mvals]).await?;
        let move_id = created.ids()[0];

        let mut dline = RawVals::new();
        dline.insert("move_id".into(), Cell::Id(move_id));
        dline.insert("account_id".into(), Cell::Id(debit_acc));
        dline.insert("name".into(), Cell::Str(label.as_str().into()));
        dline.insert("currency_id".into(), Cell::Id(currency_id));
        dline.insert("balance".into(), Cell::Decimal(amount));
        dline.insert("amount_currency".into(), Cell::Decimal(amount));
        if let Some(pid) = partner_id {
            dline.insert("partner_id".into(), Cell::Id(pid));
        }

        let mut cline = RawVals::new();
        cline.insert("move_id".into(), Cell::Id(move_id));
        cline.insert("account_id".into(), Cell::Id(credit_acc));
        cline.insert("name".into(), Cell::Str(label.as_str().into()));
        cline.insert("currency_id".into(), Cell::Id(currency_id));
        cline.insert("balance".into(), Cell::Decimal(-amount));
        cline.insert("amount_currency".into(), Cell::Decimal(-amount));
        if let Some(pid) = partner_id {
            cline.insert("partner_id".into(), Cell::Id(pid));
        }

        let unchecked_lines = crate::moves::unchecked(&env.records("account.move.line")?);
        unchecked_lines.create(tx, vec![dline, cline]).await?;
        post_moves(tx, &env.records("account.move")?.browse([move_id])?).await?;

        let new_state = if next_idx >= total_periods {
            "done"
        } else {
            "running"
        };
        let next_date = if next_idx >= total_periods {
            None
        } else {
            Some(add_months(post_date, interval)?)
        };
        tx.execute(
            "UPDATE account_recurring_entry \
             SET posted_periods = $2, next_date = $3, last_move_id = $4, state = $5, \
                 write_date = now() AT TIME ZONE 'UTC' \
             WHERE id = $1",
            &[&rid, &next_idx, &next_date, &move_id, &new_state],
        )
        .await?;
        tx.invalidate_records(
            rec.as_records(),
            Some(&["posted_periods", "next_date", "last_move_id", "state"]),
        )
        .await
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_reset_draft(
        this: &RecordSet<AccountRecurringEntry>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Write).await?;
        let rid = rec.as_records().ids()[0];
        tx.execute(
            "UPDATE account_recurring_entry SET state = 'draft', \
             write_date = now() AT TIME ZONE 'UTC' WHERE id = $1",
            &[&rid],
        )
        .await?;
        tx.invalidate_records(rec.as_records(), Some(&["state"]))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::compute_deferred_period_amount;
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn deferred_installments_conserve_exact_total_with_remainder() {
        let total = Decimal::from_str("100.00").unwrap();
        let p1 = compute_deferred_period_amount(total, 3, 1).unwrap();
        let p2 = compute_deferred_period_amount(total, 3, 2).unwrap();
        let p3 = compute_deferred_period_amount(total, 3, 3).unwrap();
        assert_eq!(p1, Decimal::from_str("33.33").unwrap());
        assert_eq!(p2, Decimal::from_str("33.33").unwrap());
        assert_eq!(p3, Decimal::from_str("33.34").unwrap());
        assert_eq!(p1 + p2 + p3, total);
    }
}
