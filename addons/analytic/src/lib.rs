// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `analytic` for ERPForAll: analytic plans, analytic accounts, analytic items, and
//! analytic distribution models.

erp_sdk::addon!(name = "analytic", depends = ["base"], exact_only = true);

use erp_sdk::prelude::*;

fn default_today(_ctx: &erp_sdk::DefaultContext) -> Value {
    Value::Date(Date::today())
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Analytic query failed: {err}"))
}

/// `account.analytic.plan`: grouping dimension for analytic accounts (e.g. Projects, Departments).
#[model(
    name = "account.analytic.plan",
    description = "Analytic Plan",
    order = "name, id"
)]
pub struct AccountAnalyticPlan {
    /// Plan name.
    #[field(char, required = true, translate = true, string = "Name")]
    pub name: Option<Arc<str>>,
    /// Optional parent plan for hierarchical plans.
    #[field(
        many2one,
        comodel = "account.analytic.plan",
        ondelete = "restrict",
        string = "Parent"
    )]
    pub parent_id: Option<RecId>,
    /// Description of this analytic dimension.
    #[field(text, string = "Description")]
    pub description: Option<Arc<str>>,
    /// Default applicability across accounting documents.
    #[field(
        selection,
        selection = [
            ("optional", "Optional"),
            ("mandatory", "Mandatory"),
            ("unavailable", "Unavailable"),
        ],
        required = true,
        default = "optional",
        string = "Default Applicability"
    )]
    pub default_applicability: Option<Arc<str>>,
    /// Display color index.
    #[field(integer, default = 0, string = "Color")]
    pub color: i32,
}

#[methods(AccountAnalyticPlan, addon = "analytic")]
impl AccountAnalyticPlan {}

/// `account.analytic.account`: cost center, project, or department tracking analytic profit/loss.
#[model(
    name = "account.analytic.account",
    description = "Analytic Account",
    order = "name, id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "ancestor", empty = "deny")
)]
pub struct AccountAnalyticAccount {
    /// Analytic account name.
    #[field(char, required = true, index = true, string = "Analytic Account")]
    pub name: Option<Arc<str>>,
    /// Short reference code.
    #[field(char, index = true, string = "Reference")]
    pub code: Option<Arc<str>>,
    /// Analytic plan dimension.
    #[field(
        many2one,
        comodel = "account.analytic.plan",
        required = true,
        ondelete = "restrict",
        string = "Plan"
    )]
    pub plan_id: Option<RecId>,
    /// Optional customer/partner associated with this analytic account.
    #[field(
        many2one,
        comodel = "res.partner",
        check_company = true,
        ondelete = "set null",
        string = "Customer"
    )]
    pub partner_id: Option<RecId>,
    /// Owning company.
    #[field(many2one, comodel = "res.company", required = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// Company currency for analytic totals.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Whether this analytic account is active.
    #[field(boolean, default = true, string = "Active")]
    pub active: bool,
    /// Analytic items posted to this account.
    #[field(
        one2many,
        comodel = "account.analytic.line",
        inverse_name = "account_id",
        string = "Analytic Lines"
    )]
    pub line_ids: Vec<RecId>,
    /// Total negative analytic amounts (costs), shown positive.
    #[field(
        monetary,
        currency_field = "currency_id",
        compute = "_compute_debit_credit_balance",
        string = "Debit"
    )]
    pub debit: Decimal,
    /// Total positive analytic amounts (revenues).
    #[field(
        monetary,
        currency_field = "currency_id",
        compute = "_compute_debit_credit_balance",
        string = "Credit"
    )]
    pub credit: Decimal,
    /// Net analytic balance (`credit - debit`).
    #[field(
        monetary,
        currency_field = "currency_id",
        compute = "_compute_debit_credit_balance",
        string = "Balance"
    )]
    pub balance: Decimal,
}

#[methods(AccountAnalyticAccount, addon = "analytic")]
impl AccountAnalyticAccount {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountAnalyticAccount>,
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
        if names.iter().any(|n| n == "plan_id") && !values.contains_key("plan_id") {
            let rows = tx
                .query(
                    "SELECT id::int8 FROM account_analytic_plan ORDER BY id LIMIT 1",
                    &[],
                )
                .await?;
            if let Some(pid) = rows.first().and_then(|r| r.try_get::<_, i64>(0).ok()) {
                values.insert("plan_id".to_owned(), Value::Int(pid));
            }
        }
        Ok(values)
    }

    #[compute(depends = ["line_ids", "line_ids.amount"])]
    async fn _compute_debit_credit_balance(
        this: &RecordSet<AccountAnalyticAccount>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountAnalyticAccount>,
    ) -> Result<()> {
        let rec_ids = this.as_records().rec_ids();
        let real_ids: Vec<i64> = rec_ids
            .iter()
            .filter_map(|r| r.as_real())
            .map(|r| r.get())
            .collect();
        let mut totals = std::collections::HashMap::<i64, (Decimal, Decimal, Decimal)>::new();
        if !real_ids.is_empty() {
            let rows = tx
                .query(
                    "SELECT account_id::int8, \
                            COALESCE(SUM(-amount) FILTER (WHERE amount < 0), 0), \
                            COALESCE(SUM(amount) FILTER (WHERE amount > 0), 0), \
                            COALESCE(SUM(amount), 0) \
                     FROM account_analytic_line \
                     WHERE account_id = ANY($1::int8[]) \
                     GROUP BY account_id",
                    &[&real_ids],
                )
                .await?;
            for row in rows {
                let aid: i64 = row.try_get(0).map_err(db)?;
                let debit: Decimal = row.try_get(1).map_err(db)?;
                let credit: Decimal = row.try_get(2).map_err(db)?;
                let balance: Decimal = row.try_get(3).map_err(db)?;
                totals.insert(aid, (debit, credit, balance));
            }
        }
        for (i, rid) in rec_ids.iter().enumerate() {
            let (d, c, b) = rid
                .as_real()
                .and_then(|r| totals.get(&r.get()).copied())
                .unwrap_or((Decimal::ZERO, Decimal::ZERO, Decimal::ZERO));
            out.set(i, AccountAnalyticAccount::DEBIT, d)?;
            out.set(i, AccountAnalyticAccount::CREDIT, c)?;
            out.set(i, AccountAnalyticAccount::BALANCE, b)?;
        }
        Ok(())
    }
}

/// `account.analytic.line`: a single analytic cost or revenue item.
#[model(
    name = "account.analytic.line",
    description = "Analytic Line",
    order = "date desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountAnalyticLine {
    /// Line description.
    #[field(char, required = true, string = "Description")]
    pub name: Option<Arc<str>>,
    /// Posting date.
    #[field(date, required = true, default_fn = default_today, index = true, string = "Date")]
    pub date: Option<Date>,
    /// Target analytic account.
    #[field(
        many2one,
        comodel = "account.analytic.account",
        required = true,
        check_company = true,
        ondelete = "restrict",
        index = true,
        string = "Analytic Account"
    )]
    pub account_id: Option<RecId>,
    /// Analytic plan derived from the account.
    #[field(
        many2one,
        comodel = "account.analytic.plan",
        related = "account_id.plan_id",
        store = true,
        string = "Plan"
    )]
    pub plan_id: Option<RecId>,
    /// Optional partner.
    #[field(
        many2one,
        comodel = "res.partner",
        check_company = true,
        ondelete = "set null",
        string = "Partner"
    )]
    pub partner_id: Option<RecId>,
    /// Company derived from the analytic account.
    #[field(
        many2one,
        comodel = "res.company",
        related = "account_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Currency derived from the company.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Signed analytic amount: positive = revenue/credit, negative = expense/debit.
    #[field(monetary, currency_field = "currency_id", string = "Amount")]
    pub amount: Decimal,
    /// Optional quantity (hours, units).
    #[field(float, digits = (16, 2), default = 0, string = "Quantity")]
    pub unit_amount: Decimal,
    /// Source document reference.
    #[field(char, string = "Reference")]
    pub r#ref: Option<Arc<str>>,
}

#[methods(AccountAnalyticLine, addon = "analytic")]
impl AccountAnalyticLine {}

/// `account.analytic.distribution.model`: rule automatically suggesting an analytic account
/// based on partner or account code prefix.
#[model(
    name = "account.analytic.distribution.model",
    description = "Analytic Distribution Model",
    order = "sequence, id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "ancestor", empty = "deny")
)]
pub struct AccountAnalyticDistributionModel {
    /// Rule name.
    #[field(char, required = true, string = "Rule Name")]
    pub name: Option<Arc<str>>,
    /// Priority order.
    #[field(integer, default = 10, string = "Sequence")]
    pub sequence: i32,
    /// Owning company.
    #[field(many2one, comodel = "res.company", required = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// Optional partner filter.
    #[field(
        many2one,
        comodel = "res.partner",
        check_company = true,
        string = "Partner"
    )]
    pub partner_id: Option<RecId>,
    /// Optional account code prefix filter (e.g. "6" for expenses).
    #[field(char, string = "Accounts Prefix")]
    pub account_prefix: Option<Arc<str>>,
    /// Analytic account applied when matched.
    #[field(
        many2one,
        comodel = "account.analytic.account",
        required = true,
        check_company = true,
        ondelete = "cascade",
        string = "Analytic Account"
    )]
    pub analytic_account_id: Option<RecId>,
    /// Allocation percentage (0..100).
    #[field(float, digits = (16, 2), default = 100, string = "Allocation %")]
    pub percentage: Decimal,
}

#[methods(AccountAnalyticDistributionModel, addon = "analytic")]
impl AccountAnalyticDistributionModel {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountAnalyticDistributionModel>,
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
}
