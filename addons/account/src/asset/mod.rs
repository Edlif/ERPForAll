// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixed Assets & Depreciation Schedules (`account.asset`, `account.asset.depreciation.line`).

use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::fiscal::calendar::add_months;
use crate::moves::post_moves;

/// One row of a computed depreciation schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepreciationRow {
    pub sequence: i32,
    pub date: Date,
    pub amount: Decimal,
    pub cumulative: Decimal,
    pub remaining: Decimal,
}

/// Pure exact-decimal depreciation schedule generator supporting Straight Line (`degressive = false`)
/// and Declining Balance (`degressive = true`). Strictly conserves exact total:
/// `sum(row.amount) == depreciable` and `last.remaining == 0`.
pub fn build_depreciation_schedule(
    depreciable: Decimal,
    start_date: Date,
    periods: i32,
    months_per_period: i32,
    degressive: bool,
    factor: Decimal,
) -> Result<Vec<DepreciationRow>> {
    if depreciable <= Decimal::ZERO {
        return Err(Error::user(
            "Depreciable value (Original Value minus Salvage Value) must be greater than zero.",
        ));
    }
    if periods <= 0 || periods > 600 {
        return Err(Error::user(
            "Number of depreciations must be between 1 and 600.",
        ));
    }
    if months_per_period <= 0 || months_per_period > 120 {
        return Err(Error::user("Months per period must be between 1 and 120."));
    }
    let mut rows = Vec::with_capacity(periods as usize);
    let mut remaining = depreciable.round_dp(2);
    let mut cumulative = Decimal::ZERO;
    let linear_base = (remaining / Decimal::from(periods)).round_dp(2);

    for i in 1..=periods {
        let dep_date = add_months(start_date, (i - 1) * months_per_period)?;
        let amount = if i == periods {
            remaining
        } else if degressive {
            let periods_left = periods - i + 1;
            let linear_floor = (remaining / Decimal::from(periods_left)).round_dp(2);
            let deg = (remaining * factor).round_dp(2);
            deg.max(linear_floor).min(remaining)
        } else {
            linear_base.min(remaining)
        };
        cumulative += amount;
        remaining -= amount;
        rows.push(DepreciationRow {
            sequence: i,
            date: dep_date,
            amount,
            cumulative,
            remaining,
        });
    }
    Ok(rows)
}

fn default_today(_ctx: &erp_sdk::DefaultContext) -> Value {
    Value::Date(Date::today())
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Asset query failed: {err}"))
}

/// `account.asset`: fixed asset with automatic straight-line or declining-balance depreciation.
#[model(
    name = "account.asset",
    description = "Fixed Asset",
    order = "acquisition_date desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountAsset {
    /// Asset name (e.g. "Delivery Van #1", "Server Rack").
    #[field(char, required = true, string = "Asset Name")]
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
    /// Lifecycle status.
    #[field(
        selection,
        selection = [
            ("draft", "Draft"),
            ("open", "Running"),
            ("close", "Closed"),
        ],
        required = true,
        readonly = true,
        copy = false,
        default = "draft",
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
    /// Acquisition / depreciation start date.
    #[field(date, required = true, default_fn = default_today, string = "Acquisition Date")]
    pub acquisition_date: Option<Date>,
    /// Purchase / original cost of the asset.
    #[field(monetary, currency_field = "currency_id", string = "Original Value")]
    pub original_value: Decimal,
    /// Salvage / non-depreciable residual value.
    #[field(
        monetary,
        currency_field = "currency_id",
        string = "Not Depreciable Value"
    )]
    pub salvage_value: Decimal,
    /// Remaining book value (original value minus posted depreciation).
    #[field(
        monetary,
        currency_field = "currency_id",
        compute = "_compute_residual",
        string = "Book Value"
    )]
    pub value_residual: Decimal,
    /// Straight-line or declining-balance method.
    #[field(
        selection,
        selection = [
            ("linear", "Straight Line"),
            ("degressive", "Declining Balance"),
        ],
        required = true,
        default = "linear",
        string = "Method"
    )]
    pub method: Option<Arc<str>>,
    /// Number of depreciation entries.
    #[field(
        integer,
        required = true,
        default = 5,
        string = "Number of Depreciations"
    )]
    pub method_number: i32,
    /// Duration of one period in months (1 = monthly, 12 = yearly).
    #[field(integer, required = true, default = 12, string = "Months per Period")]
    pub method_period: i32,
    /// Declining balance factor (e.g. 0.30 = 30%).
    #[field(float, digits = (16, 4), default = 0.30, string = "Declining Factor")]
    pub method_progress_factor: Decimal,
    /// Journal used to post depreciation entries.
    #[field(
        many2one,
        comodel = "account.journal",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Journal"
    )]
    pub journal_id: Option<RecId>,
    /// Fixed Asset balance sheet account.
    #[field(
        many2one,
        comodel = "account.account",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Fixed Asset Account"
    )]
    pub account_asset_id: Option<RecId>,
    /// Accumulated Depreciation account (credited on each depreciation entry).
    #[field(
        many2one,
        comodel = "account.account",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Depreciation Account"
    )]
    pub account_depreciation_id: Option<RecId>,
    /// Depreciation Expense account (debited on each depreciation entry).
    #[field(
        many2one,
        comodel = "account.account",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Expense Account"
    )]
    pub account_depreciation_expense_id: Option<RecId>,
    /// Computed depreciation board lines.
    #[field(
        one2many,
        comodel = "account.asset.depreciation.line",
        inverse_name = "asset_id",
        string = "Depreciation Board"
    )]
    pub depreciation_line_ids: Vec<RecId>,
}

#[methods(AccountAsset, addon = "account")]
impl AccountAsset {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountAsset>,
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

    #[compute(depends = [
        "original_value",
        "depreciation_line_ids",
        "depreciation_line_ids.amount",
        "depreciation_line_ids.state",
    ])]
    async fn _compute_residual(
        this: &RecordSet<AccountAsset>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountAsset>,
    ) -> Result<()> {
        let originals = this.mapped(tx, AccountAsset::ORIGINAL_VALUE).await?;
        for (i, (rid, orig)) in this
            .as_records()
            .rec_ids()
            .iter()
            .zip(originals)
            .enumerate()
        {
            let posted = if let Some(real) = rid.as_real() {
                let rows = tx
                    .query(
                        "SELECT COALESCE(SUM(amount), 0) FROM account_asset_depreciation_line \
                         WHERE asset_id = $1 AND state = 'posted'",
                        &[&real.get()],
                    )
                    .await?;
                rows.first()
                    .and_then(|r| r.try_get::<_, Decimal>(0).ok())
                    .unwrap_or(Decimal::ZERO)
            } else {
                Decimal::ZERO
            };
            out.set(
                i,
                AccountAsset::VALUE_RESIDUAL,
                (orig - posted).max(Decimal::ZERO),
            )?;
        }
        Ok(())
    }

    /// Generates (or regenerates) the draft depreciation schedule lines.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_compute_depreciation_board(
        this: &RecordSet<AccountAsset>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let asset = this.ensure_one()?;
        asset
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let aid = asset.as_records().ids()[0];
        let state = asset.get(tx, AccountAsset::STATE).await?;
        if state.as_deref() == Some("close") {
            return Err(Error::user("Cannot recompute a closed asset."));
        }
        let orig = asset.get(tx, AccountAsset::ORIGINAL_VALUE).await?;
        let salvage = asset.get(tx, AccountAsset::SALVAGE_VALUE).await?;
        let start = asset
            .get(tx, AccountAsset::ACQUISITION_DATE)
            .await?
            .unwrap_or_else(Date::today);
        let periods = asset.get(tx, AccountAsset::METHOD_NUMBER).await?;
        let months = asset.get(tx, AccountAsset::METHOD_PERIOD).await?;
        let method = asset
            .get(tx, AccountAsset::METHOD)
            .await?
            .unwrap_or_else(|| Arc::from("linear"));
        let factor = asset.get(tx, AccountAsset::METHOD_PROGRESS_FACTOR).await?;

        let schedule = build_depreciation_schedule(
            orig - salvage,
            start,
            periods,
            months,
            method.as_ref() == "degressive",
            factor,
        )?;
        // Remove existing unposted schedule lines before creating fresh rows.
        tx.execute(
            "DELETE FROM account_asset_depreciation_line WHERE asset_id = $1 AND state = 'draft'",
            &[&aid],
        )
        .await?;
        let mut vals_list = Vec::with_capacity(schedule.len());
        for row in schedule {
            let mut v = RawVals::new();
            v.insert("asset_id".into(), Cell::Id(aid));
            v.insert("sequence".into(), Cell::Int(row.sequence));
            v.insert("depreciation_date".into(), Cell::Date(row.date));
            v.insert("amount".into(), Cell::Decimal(row.amount));
            v.insert("depreciated_value".into(), Cell::Decimal(row.cumulative));
            v.insert("remaining_value".into(), Cell::Decimal(row.remaining));
            vals_list.push(v);
        }
        asset
            .env()
            .records("account.asset.depreciation.line")?
            .create(tx, vals_list)
            .await?;
        tx.invalidate_records(
            asset.as_records(),
            Some(&["depreciation_line_ids", "value_residual"]),
        )
        .await
    }

    /// Confirms the asset: computes the schedule if empty and transitions state to Running.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_validate(
        this: &RecordSet<AccountAsset>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let asset = this.ensure_one()?;
        asset
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let aid = asset.as_records().ids()[0];
        let count_rows = tx
            .query(
                "SELECT count(*)::int8 FROM account_asset_depreciation_line WHERE asset_id = $1",
                &[&aid],
            )
            .await?;
        let count: i64 = count_rows
            .first()
            .and_then(|r| r.try_get(0).ok())
            .unwrap_or(0);
        if count == 0 {
            Self::action_compute_depreciation_board(asset, tx).await?;
        }
        tx.execute(
            "UPDATE account_asset SET state = 'open', write_date = now() AT TIME ZONE 'UTC' \
             WHERE id = $1",
            &[&aid],
        )
        .await?;
        tx.invalidate_records(asset.as_records(), Some(&["state"]))
            .await
    }

    /// Posts the next unposted depreciation entry to the General Ledger (Debit Expense, Credit
    /// Accumulated Depreciation) and closes the asset once all lines are posted.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_post_next_depreciation(
        this: &RecordSet<AccountAsset>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let asset = this.ensure_one()?;
        asset
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let aid = asset.as_records().ids()[0];
        let state = asset.get(tx, AccountAsset::STATE).await?;
        if state.as_deref() != Some("open") {
            return Err(Error::user(
                "Confirm the asset before posting depreciation entries.",
            ));
        }
        let next_rows = tx
            .query(
                "SELECT id::int8, sequence, depreciation_date, COALESCE(amount, 0) \
                 FROM account_asset_depreciation_line \
                 WHERE asset_id = $1 AND state = 'draft' \
                 ORDER BY sequence, id LIMIT 1",
                &[&aid],
            )
            .await?;
        let Some(nrow) = next_rows.first() else {
            return Err(Error::user(
                "All depreciation entries for this asset have already been posted.",
            ));
        };
        let line_id: i64 = nrow.try_get(0).map_err(db)?;
        let seq: i32 = nrow.try_get(1).map_err(db)?;
        let dep_date: Date = nrow.try_get(2).map_err(db)?;
        let amount: Decimal = nrow.try_get(3).map_err(db)?;

        let name = asset
            .get(tx, AccountAsset::NAME)
            .await?
            .unwrap_or_else(|| Arc::from("Asset"));
        let company_id = asset
            .get(tx, AccountAsset::COMPANY_ID)
            .await?
            .ok_or_else(|| Error::validation("Asset has no company."))?
            .get();
        let currency_id = asset
            .get(tx, AccountAsset::CURRENCY_ID)
            .await?
            .ok_or_else(|| Error::validation("Asset has no currency."))?
            .get();
        let journal_id = asset
            .get(tx, AccountAsset::JOURNAL_ID)
            .await?
            .ok_or_else(|| Error::validation("Asset has no journal."))?
            .get();
        let dep_acc = asset
            .get(tx, AccountAsset::ACCOUNT_DEPRECIATION_ID)
            .await?
            .ok_or_else(|| Error::validation("Asset has no depreciation account."))?
            .get();
        let exp_acc = asset
            .get(tx, AccountAsset::ACCOUNT_DEPRECIATION_EXPENSE_ID)
            .await?
            .ok_or_else(|| Error::validation("Asset has no depreciation expense account."))?
            .get();

        let env = asset
            .env()
            .with_context_replaced(erp_sdk::Context::default());
        let label = format!("Depreciation {seq}: {name}");
        let mut mvals = RawVals::new();
        mvals.insert("move_type".into(), Cell::Str("entry".into()));
        mvals.insert("journal_id".into(), Cell::Id(journal_id));
        mvals.insert("company_id".into(), Cell::Id(company_id));
        mvals.insert("currency_id".into(), Cell::Id(currency_id));
        mvals.insert("date".into(), Cell::Date(dep_date));
        mvals.insert("ref".into(), Cell::Str(label.as_str().into()));

        let unchecked_moves = crate::moves::unchecked(&env.records("account.move")?);
        let created_move = unchecked_moves.create(tx, vec![mvals]).await?;
        let move_id = created_move.ids()[0];

        let mut debit_line = RawVals::new();
        debit_line.insert("move_id".into(), Cell::Id(move_id));
        debit_line.insert("account_id".into(), Cell::Id(exp_acc));
        debit_line.insert("name".into(), Cell::Str(label.as_str().into()));
        debit_line.insert("currency_id".into(), Cell::Id(currency_id));
        debit_line.insert("balance".into(), Cell::Decimal(amount));
        debit_line.insert("amount_currency".into(), Cell::Decimal(amount));

        let mut credit_line = RawVals::new();
        credit_line.insert("move_id".into(), Cell::Id(move_id));
        credit_line.insert("account_id".into(), Cell::Id(dep_acc));
        credit_line.insert("name".into(), Cell::Str(label.as_str().into()));
        credit_line.insert("currency_id".into(), Cell::Id(currency_id));
        credit_line.insert("balance".into(), Cell::Decimal(-amount));
        credit_line.insert("amount_currency".into(), Cell::Decimal(-amount));

        let unchecked_lines = crate::moves::unchecked(&env.records("account.move.line")?);
        unchecked_lines
            .create(tx, vec![debit_line, credit_line])
            .await?;
        post_moves(tx, &env.records("account.move")?.browse([move_id])?).await?;

        tx.execute(
            "UPDATE account_asset_depreciation_line \
             SET state = 'posted', move_id = $2, write_date = now() AT TIME ZONE 'UTC' \
             WHERE id = $1",
            &[&line_id, &move_id],
        )
        .await?;
        let rem_rows = tx
            .query(
                "SELECT count(*)::int8 FROM account_asset_depreciation_line \
                 WHERE asset_id = $1 AND state = 'draft'",
                &[&aid],
            )
            .await?;
        let remaining: i64 = rem_rows
            .first()
            .and_then(|r| r.try_get(0).ok())
            .unwrap_or(0);
        if remaining == 0 {
            tx.execute(
                "UPDATE account_asset SET state = 'close', write_date = now() AT TIME ZONE 'UTC' \
                 WHERE id = $1",
                &[&aid],
            )
            .await?;
        }
        tx.invalidate_records(
            asset.as_records(),
            Some(&["state", "value_residual", "depreciation_line_ids"]),
        )
        .await?;
        tx.invalidate_model(
            "account.asset.depreciation.line",
            Some(&["state", "move_id"]),
        )
        .await
    }

    /// Manually closes a running asset.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_close(this: &RecordSet<AccountAsset>, tx: &mut Transaction) -> Result<()> {
        let asset = this.ensure_one()?;
        asset
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let aid = asset.as_records().ids()[0];
        tx.execute(
            "UPDATE account_asset SET state = 'close', write_date = now() AT TIME ZONE 'UTC' \
             WHERE id = $1",
            &[&aid],
        )
        .await?;
        tx.invalidate_records(asset.as_records(), Some(&["state"]))
            .await
    }
}

/// `account.asset.depreciation.line`: one scheduled depreciation installment of an asset.
#[model(
    name = "account.asset.depreciation.line",
    description = "Asset Depreciation Line",
    order = "sequence, id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountAssetDepreciationLine {
    /// Parent asset.
    #[field(
        many2one,
        comodel = "account.asset",
        required = true,
        ondelete = "cascade",
        string = "Asset"
    )]
    pub asset_id: Option<RecId>,
    /// Owning company.
    #[field(
        many2one,
        comodel = "res.company",
        related = "asset_id.company_id",
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
    /// Installment number.
    #[field(integer, default = 1, string = "Period")]
    pub sequence: i32,
    /// Scheduled depreciation date.
    #[field(date, required = true, string = "Depreciation Date")]
    pub depreciation_date: Option<Date>,
    /// Depreciation amount for this period.
    #[field(monetary, currency_field = "currency_id", string = "Depreciation")]
    pub amount: Decimal,
    /// Cumulative depreciation after this period.
    #[field(
        monetary,
        currency_field = "currency_id",
        readonly = true,
        string = "Cumulative"
    )]
    pub depreciated_value: Decimal,
    /// Remaining depreciable balance after this period.
    #[field(
        monetary,
        currency_field = "currency_id",
        readonly = true,
        string = "Remaining"
    )]
    pub remaining_value: Decimal,
    /// Posted journal entry when booked.
    #[field(
        many2one,
        comodel = "account.move",
        readonly = true,
        ondelete = "set null",
        string = "Journal Entry"
    )]
    pub move_id: Option<RecId>,
    /// Draft until posted.
    #[field(
        selection,
        selection = [("draft", "Scheduled"), ("posted", "Posted")],
        required = true,
        readonly = true,
        default = "draft",
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
}

#[methods(AccountAssetDepreciationLine, addon = "account")]
impl AccountAssetDepreciationLine {}

#[cfg(test)]
mod tests {
    use super::build_depreciation_schedule;
    use erp_sdk::{Date, Decimal};
    use std::str::FromStr;

    #[test]
    fn linear_and_degressive_schedules_conserve_exact_total() {
        let start = Date::from_ymd(2026, 1, 1).unwrap();
        let depreciable = Decimal::from_str("1000.00").unwrap();
        let linear =
            build_depreciation_schedule(depreciable, start, 3, 12, false, Decimal::ZERO).unwrap();
        assert_eq!(linear.len(), 3);
        let sum_linear: Decimal = linear.iter().map(|r| r.amount).sum();
        assert_eq!(sum_linear, depreciable);
        assert_eq!(linear.last().unwrap().remaining, Decimal::ZERO);

        let deg = build_depreciation_schedule(
            depreciable,
            start,
            4,
            12,
            true,
            Decimal::from_str("0.40").unwrap(),
        )
        .unwrap();
        let sum_deg: Decimal = deg.iter().map(|r| r.amount).sum();
        assert_eq!(sum_deg, depreciable);
        assert_eq!(deg[0].amount, Decimal::from_str("400.00").unwrap());
        assert_eq!(deg.last().unwrap().remaining, Decimal::ZERO);
    }
}
