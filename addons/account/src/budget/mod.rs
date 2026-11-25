// SPDX-License-Identifier: MIT OR Apache-2.0
//! Financial Budgets (`account.budget`, `account.budget.line`) with exact-decimal
//! practical vs. theoretical achievement metrics.

use erp_sdk::Operation;
use erp_sdk::prelude::*;

/// Pure exact-decimal budget line calculation: returns `(theoretical_amount, achievement_pct)`.
/// Theoretical amount is linearly pro-rated over `[date_from, date_to]` as of `today`.
pub fn compute_budget_line_metrics(
    planned: Decimal,
    practical: Decimal,
    date_from: Date,
    date_to: Date,
    today: Date,
) -> (Decimal, Decimal) {
    let total_days = (date_to.days() - date_from.days() + 1).max(1);
    let theoretical = if today.days() < date_from.days() {
        Decimal::ZERO
    } else if today.days() >= date_to.days() {
        planned
    } else {
        let elapsed = today.days() - date_from.days() + 1;
        (planned * Decimal::from(elapsed) / Decimal::from(total_days)).round_dp(2)
    };
    let pct = if !theoretical.is_zero() {
        (practical / theoretical * Decimal::from(100)).round_dp(2)
    } else if !planned.is_zero() {
        (practical / planned * Decimal::from(100)).round_dp(2)
    } else {
        Decimal::ZERO
    };
    (theoretical, pct)
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Budget query failed: {err}"))
}

fn default_year_start(_ctx: &erp_sdk::DefaultContext) -> Value {
    let (y, _, _) = Date::today().ymd();
    Value::Date(Date::from_ymd(y, 1, 1).unwrap_or_else(Date::today))
}

fn default_year_end(_ctx: &erp_sdk::DefaultContext) -> Value {
    let (y, _, _) = Date::today().ymd();
    Value::Date(Date::from_ymd(y, 12, 31).unwrap_or_else(Date::today))
}

/// `account.budget`: periodic financial & analytic budget comparing planned vs actual performance.
#[model(
    name = "account.budget",
    description = "Budget",
    order = "date_from desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountBudget {
    /// Budget name.
    #[field(char, required = true, string = "Budget Name")]
    pub name: Option<Arc<str>>,
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
    /// Budget start date.
    #[field(date, required = true, default_fn = default_year_start, string = "Start Date")]
    pub date_from: Option<Date>,
    /// Budget end date.
    #[field(date, required = true, default_fn = default_year_end, string = "End Date")]
    pub date_to: Option<Date>,
    /// Workflow status.
    #[field(
        selection,
        selection = [
            ("draft", "Draft"),
            ("confirm", "Confirmed"),
            ("validate", "Validated"),
            ("done", "Done"),
            ("cancel", "Cancelled"),
        ],
        required = true,
        readonly = true,
        copy = false,
        default = "draft",
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
    /// Budget lines.
    #[field(
        one2many,
        comodel = "account.budget.line",
        inverse_name = "budget_id",
        string = "Budget Lines"
    )]
    pub line_ids: Vec<RecId>,
    /// Total planned amount across all lines.
    #[field(
        monetary,
        currency_field = "currency_id",
        compute = "_compute_totals",
        string = "Total Planned"
    )]
    pub total_planned: Decimal,
    /// Total practical amount achieved across all lines.
    #[field(
        monetary,
        currency_field = "currency_id",
        compute = "_compute_totals",
        string = "Total Practical"
    )]
    pub total_practical: Decimal,
}

#[methods(AccountBudget, addon = "account")]
impl AccountBudget {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountBudget>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<erp_sdk::ValueMap> {
        let mut values: erp_sdk::ValueMap =
            sup.call(tx, this.as_records(), (names.clone(),)).await?;
        if names.iter().any(|n| n == "company_id")
            && !values.contains_key("company_id")
            && let Some(cid) = this.env().company_id()?
        {
            values.insert("company_id".to_owned(), Value::Int(cid));
        }
        Ok(values)
    }

    #[compute(depends = ["line_ids", "line_ids.planned_amount", "line_ids.practical_amount"])]
    async fn _compute_totals(
        this: &RecordSet<AccountBudget>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountBudget>,
    ) -> Result<()> {
        for (i, rid) in this.as_records().rec_ids().iter().enumerate() {
            let (planned, practical) = if let Some(real) = rid.as_real() {
                let rows = tx
                    .query(
                        "SELECT COALESCE(SUM(planned_amount), 0), COALESCE(SUM(practical_amount), 0) \
                         FROM account_budget_line WHERE budget_id = $1",
                        &[&real.get()],
                    )
                    .await?;
                if let Some(row) = rows.first() {
                    (
                        row.try_get::<_, Decimal>(0).map_err(db)?,
                        row.try_get::<_, Decimal>(1).map_err(db)?,
                    )
                } else {
                    (Decimal::ZERO, Decimal::ZERO)
                }
            } else {
                (Decimal::ZERO, Decimal::ZERO)
            };
            out.set(i, AccountBudget::TOTAL_PLANNED, planned)?;
            out.set(i, AccountBudget::TOTAL_PRACTICAL, practical)?;
        }
        Ok(())
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_budget_confirm(
        this: &RecordSet<AccountBudget>,
        tx: &mut Transaction,
    ) -> Result<()> {
        set_budget_state(this, tx, "confirm").await
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_budget_validate(
        this: &RecordSet<AccountBudget>,
        tx: &mut Transaction,
    ) -> Result<()> {
        set_budget_state(this, tx, "validate").await?;
        Self::action_refresh_practical(this, tx).await
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_budget_done(
        this: &RecordSet<AccountBudget>,
        tx: &mut Transaction,
    ) -> Result<()> {
        Self::action_refresh_practical(this, tx).await?;
        set_budget_state(this, tx, "done").await
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_budget_cancel(
        this: &RecordSet<AccountBudget>,
        tx: &mut Transaction,
    ) -> Result<()> {
        set_budget_state(this, tx, "cancel").await
    }

    #[method]
    #[rpc(effect = write)]
    pub async fn action_budget_draft(
        this: &RecordSet<AccountBudget>,
        tx: &mut Transaction,
    ) -> Result<()> {
        set_budget_state(this, tx, "draft").await
    }

    /// Recomputes stored `practical_amount`, `theoretical_amount`, and `percentage` on every
    /// line of this budget from posted journal items / analytic lines.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_refresh_practical(
        this: &RecordSet<AccountBudget>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records().check_access(tx, Operation::Write).await?;
        for budget in this.iter() {
            let bid = budget.as_records().ids()[0];
            let company_id = budget
                .get(tx, AccountBudget::COMPANY_ID)
                .await?
                .map(|c| c.get())
                .unwrap_or(1);
            let today = erp_addon_base::company_calendar::business_date(tx, company_id)
                .await
                .unwrap_or_else(|_| Date::today());
            let rows = tx
                .query(
                    "SELECT id::int8, account_id::int8, analytic_account_id::int8, \
                            date_from, date_to, COALESCE(planned_amount, 0) \
                     FROM account_budget_line WHERE budget_id = $1 ORDER BY id",
                    &[&bid],
                )
                .await?;
            for row in rows {
                let lid: i64 = row.try_get(0).map_err(db)?;
                let account_id: Option<i64> = row.try_get(1).map_err(db)?;
                let analytic_id: Option<i64> = row.try_get(2).map_err(db)?;
                let d_from: Date = row.try_get(3).map_err(db)?;
                let d_to: Date = row.try_get(4).map_err(db)?;
                let planned: Decimal = row.try_get(5).map_err(db)?;

                let practical: Decimal = if let Some(aid) = analytic_id {
                    let arows = tx
                        .query(
                            "SELECT COALESCE(SUM(amount), 0) FROM account_analytic_line \
                             WHERE account_id = $1 AND date >= $2 AND date <= $3",
                            &[&aid, &d_from, &d_to],
                        )
                        .await?;
                    arows
                        .first()
                        .and_then(|r| r.try_get(0).ok())
                        .unwrap_or(Decimal::ZERO)
                } else if let Some(acc) = account_id {
                    let mrows = tx
                        .query(
                            "SELECT COALESCE(SUM(-balance), 0) FROM account_move_line \
                             WHERE account_id = $1 AND company_id = $2 \
                               AND parent_state = 'posted' AND date >= $3 AND date <= $4",
                            &[&acc, &company_id, &d_from, &d_to],
                        )
                        .await?;
                    mrows
                        .first()
                        .and_then(|r| r.try_get(0).ok())
                        .unwrap_or(Decimal::ZERO)
                } else {
                    Decimal::ZERO
                };

                let (theoretical, pct) =
                    compute_budget_line_metrics(planned, practical, d_from, d_to, today);
                tx.execute(
                    "UPDATE account_budget_line \
                     SET practical_amount = $2, theoretical_amount = $3, percentage = $4, \
                         write_date = now() AT TIME ZONE 'UTC' \
                     WHERE id = $1",
                    &[&lid, &practical, &theoretical, &pct],
                )
                .await?;
            }
            tx.invalidate_records(
                budget.as_records(),
                Some(&["line_ids", "total_planned", "total_practical"]),
            )
            .await?;
            tx.invalidate_model(
                "account.budget.line",
                Some(&["practical_amount", "theoretical_amount", "percentage"]),
            )
            .await?;
        }
        Ok(())
    }
}

async fn set_budget_state(
    this: &RecordSet<AccountBudget>,
    tx: &mut Transaction,
    target: &str,
) -> Result<()> {
    this.as_records().check_access(tx, Operation::Write).await?;
    let ids = this.as_records().ids();
    tx.execute(
        "UPDATE account_budget SET state = $2, write_date = now() AT TIME ZONE 'UTC' \
         WHERE id = ANY($1::int8[])",
        &[&ids, &target],
    )
    .await?;
    tx.invalidate_records(this.as_records(), Some(&["state"]))
        .await
}

/// `account.budget.line`: one budgetary position tied to a GL account and/or analytic account.
#[model(
    name = "account.budget.line",
    description = "Budget Line",
    order = "date_from, id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountBudgetLine {
    /// Owning budget.
    #[field(
        many2one,
        comodel = "account.budget",
        required = true,
        ondelete = "cascade",
        string = "Budget"
    )]
    pub budget_id: Option<RecId>,
    /// Owning company.
    #[field(
        many2one,
        comodel = "res.company",
        related = "budget_id.company_id",
        store = true,
        string = "Company"
    )]
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
    /// Budgetary position label.
    #[field(char, required = true, string = "Budgetary Position")]
    pub name: Option<Arc<str>>,
    /// General ledger account tracked (when no analytic account is set).
    #[field(
        many2one,
        comodel = "account.account",
        check_company = true,
        ondelete = "restrict",
        string = "Account"
    )]
    pub account_id: Option<RecId>,
    /// Analytic account tracked (takes precedence when set).
    #[field(
        many2one,
        comodel = "account.analytic.account",
        check_company = true,
        ondelete = "restrict",
        string = "Analytic Account"
    )]
    pub analytic_account_id: Option<RecId>,
    /// Line period start date.
    #[field(date, required = true, default_fn = default_year_start, string = "Start Date")]
    pub date_from: Option<Date>,
    /// Line period end date.
    #[field(date, required = true, default_fn = default_year_end, string = "End Date")]
    pub date_to: Option<Date>,
    /// Planned amount (positive for income targets, negative for expense budgets).
    #[field(monetary, currency_field = "currency_id", string = "Planned Amount")]
    pub planned_amount: Decimal,
    /// Actual posted amount achieved in period.
    #[field(
        monetary,
        currency_field = "currency_id",
        readonly = true,
        string = "Practical Amount"
    )]
    pub practical_amount: Decimal,
    /// Pro-rata theoretical target as of today.
    #[field(
        monetary,
        currency_field = "currency_id",
        readonly = true,
        string = "Theoretical Amount"
    )]
    pub theoretical_amount: Decimal,
    /// Achievement percentage (`practical / theoretical * 100`).
    #[field(float, digits = (16, 2), readonly = true, string = "Achievement %")]
    pub percentage: Decimal,
}

#[methods(AccountBudgetLine, addon = "account")]
impl AccountBudgetLine {}

#[cfg(test)]
mod tests {
    use super::compute_budget_line_metrics;
    use erp_sdk::{Date, Decimal};
    use std::str::FromStr;

    #[test]
    fn budget_metrics_compute_exact_prorata_theoretical_and_percentage() {
        let d_from = Date::from_ymd(2026, 1, 1).unwrap();
        let d_to = Date::from_ymd(2026, 1, 10).unwrap(); // 10 days inclusive
        let today = Date::from_ymd(2026, 1, 5).unwrap(); // 5 days elapsed => 50%
        let planned = Decimal::from_str("1000.00").unwrap();
        let practical = Decimal::from_str("400.00").unwrap();
        let (theoretical, pct) =
            compute_budget_line_metrics(planned, practical, d_from, d_to, today);
        assert_eq!(theoretical, Decimal::from_str("500.00").unwrap());
        assert_eq!(pct, Decimal::from_str("80.00").unwrap());
    }
}
