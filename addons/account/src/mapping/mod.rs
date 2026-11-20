// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company account defaults. Chart packs use the same exact-company keys as readers.

mod models;
mod storage;

use erp_addon_base::company_calendar::business_days;
use erp_sdk::prelude::*;
use erp_sdk::{ErrorKind, Operation, PrincipalKind};

use crate::fiscal::ensure::ANCESTOR_OF_C;

const COMPANY_ACCESS: &str = "Accounting configuration is not accessible for this company";
const TARGET_ACCESS: &str = "Accounting configuration target does not exist or is not accessible";
const INVALID_ID: &str = "Invalid accounting configuration identifier";
const INVALID_ROLE: &str = "Invalid accounting role declaration";

/// One of the accounting addon's declared company-level default account roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DefaultRole {
    /// Default receivable on partner properties.
    Receivable,
    /// Default payable on partner properties.
    Payable,
    /// Default income on product category properties.
    Income,
    /// Default expense on product category properties.
    Expense,
    /// Default stock valuation asset on product category properties.
    StockValuation,
}

impl DefaultRole {
    /// The stable role key in chart-pack configuration.
    pub fn key(self) -> &'static str {
        match self {
            Self::Receivable => "receivable",
            Self::Payable => "payable",
            Self::Income => "income",
            Self::Expense => "expense",
            Self::StockValuation => "stock_valuation",
        }
    }

    /// Translate a chart-pack role key before validating or writing its account.
    ///
    /// # Errors
    ///
    /// `UserError` for an unrecognized role.
    pub fn from_key(key: &str) -> Result<Self> {
        match key {
            "receivable" => Ok(Self::Receivable),
            "payable" => Ok(Self::Payable),
            "income" => Ok(Self::Income),
            "expense" => Ok(Self::Expense),
            "stock_valuation" => Ok(Self::StockValuation),
            _ => Err(Error::user(format!("Unknown accounting role {key}"))),
        }
    }

    fn accepts(self, account_type: &str) -> bool {
        match self {
            Self::Receivable => account_type == "asset_receivable",
            Self::Payable => account_type == "liability_payable",
            // The reference income and expense domains both admit ordinary posting accounts.
            Self::Income | Self::Expense => {
                crate::journal::settings::ACCOUNT_TYPES.contains(&account_type)
                    && !matches!(
                        account_type,
                        "asset_receivable"
                            | "liability_payable"
                            | "asset_cash"
                            | "liability_credit_card"
                            | "off_balance"
                    )
            }
            Self::StockValuation => matches!(
                account_type,
                "asset_current" | "asset_non_current" | "asset_fixed" | "asset_prepayments"
            ),
        }
    }
}

/// Select the effective company default at the current company date or a document's business date.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum ResolutionDate {
    /// Company's local date at the caller transaction's start.
    Current,
    /// A business date explicitly chosen by the consumer.
    Document(Date),
}

/// An exact-company key for a declared default account. A branch owns its own key.
#[derive(Clone, Copy, Debug)]
pub struct CompanyDefault {
    company: i64,
    role: DefaultRole,
}

impl CompanyDefault {
    /// Admit a company identifier and a declared role before database access.
    ///
    /// # Errors
    ///
    /// `Validation` for a nonpositive id.
    pub fn new(company: i64, role: DefaultRole) -> Result<Self> {
        if company <= 0 {
            return Err(Error::validation(INVALID_ID));
        }
        Ok(Self { company, role })
    }

    /// Resolve an optional account after authorizing the company and validating the selected target.
    /// Both an absent key and an explicitly empty selected revision give `None`.
    ///
    /// # Errors
    ///
    /// Company or account access failure, an invalid configured account, an invalid date, or a
    /// database error. Invalid configured accounts never become absence.
    pub async fn resolve(self, tx: &mut Transaction, date: ResolutionDate) -> Result<Option<i64>> {
        self.resolve_as(tx, &tx.env(), date).await
    }

    pub(crate) async fn resolve_as(
        self,
        tx: &mut Transaction,
        env: &Env,
        date: ResolutionDate,
    ) -> Result<Option<i64>> {
        self.authorize(tx, env, Operation::Read).await?;
        let date = match date {
            ResolutionDate::Current => self.business_date(tx, env).await?,
            ResolutionDate::Document(date) if (1..=9999).contains(&date.ymd().0) => date,
            ResolutionDate::Document(_) => {
                return Err(Error::validation("Invalid accounting configuration date"));
            }
        };
        let role = self.role_id(tx).await?;
        let account = storage::resolve(tx, self.company, role, date).await?;
        if let Some(account) = account {
            self.validate_account(tx, env, account, false).await?;
        }
        Ok(account)
    }

    /// Resolve a required default; refuse when the company has no usable choice for this role.
    ///
    /// # Errors
    ///
    /// `UserError` for absence, or any error from [`Self::resolve`].
    pub async fn require(self, tx: &mut Transaction, date: ResolutionDate) -> Result<i64> {
        match self.resolve(tx, date).await? {
            Some(account) => Ok(account),
            None => {
                let name = self.company_name(tx, &tx.env()).await?;
                Err(Error::user(format!(
                    "No {} account configured for company {name}",
                    self.role.key().replace('_', " ")
                )))
            }
        }
    }

    /// Record an ordinary default choice. `None` explicitly empties the default for this role.
    /// The edit joins the caller's transaction. Its first recorded value is open backwards,
    /// and later changes begin on the company's local transaction-start date.
    ///
    /// # Errors
    ///
    /// Invalid input, role or account; company or target access failure; a refused database write.
    pub async fn set(self, tx: &mut Transaction, account: Option<i64>) -> Result<()> {
        self.set_as(tx, &tx.env(), account).await
    }

    pub(crate) async fn set_as(
        self,
        tx: &mut Transaction,
        env: &Env,
        account: Option<i64>,
    ) -> Result<()> {
        self.edit(tx, env, account, false).await?;
        Ok(())
    }

    /// Seed a chart-provided choice after the pack has resolved its account reference. An existing
    /// key, including an explicitly empty choice, takes precedence over all repeated seeds.
    /// Call this in the chart installation transaction and propagate any error to its owner.
    ///
    /// Returns whether the chart created a new key.
    ///
    /// # Errors
    ///
    /// As [`Self::set`].
    pub async fn install_if_absent(
        self,
        tx: &mut Transaction,
        account: Option<i64>,
    ) -> Result<bool> {
        self.edit(tx, &tx.env(), account, true).await
    }

    async fn edit(
        self,
        tx: &mut Transaction,
        env: &Env,
        account: Option<i64>,
        seed: bool,
    ) -> Result<bool> {
        if account.is_some_and(|account| account <= 0) {
            return Err(Error::validation(INVALID_ID));
        }
        self.authorize(tx, env, Operation::Write).await?;
        tx.savepoint(async move |tx| {
            let role = self.role_id(tx).await?;
            let mut writer = storage::lock_generation(tx, self.company).await?;
            let key = storage::key_for_write(tx, self.company, role).await?;
            if seed && key.is_some() {
                return Ok(false);
            }
            if let Some(account) = account {
                self.validate_account(tx, env, account, true).await?;
            }
            let date = self.business_date(tx, env).await?;
            writer.advance(tx).await?;
            writer.write(tx, role, key, date, account).await?;
            if self.role == DefaultRole::StockValuation {
                let company = env.records("res.company")?.browse([self.company])?;
                tx.invalidate_records(&company, Some(&[crate::company::valuation::FIELD]))
                    .await?;
            }
            Ok(true)
        })
        .await
    }

    async fn business_date(self, tx: &mut Transaction, env: &Env) -> Result<Date> {
        business_days(tx, env, &[self.company])
            .await?
            .get(&self.company)
            .map(|day| day.date)
            .ok_or_else(|| Error::access(COMPANY_ACCESS))
    }

    async fn authorize(self, tx: &mut Transaction, env: &Env, operation: Operation) -> Result<()> {
        if env.principal().kind() != PrincipalKind::System {
            let companies = tx.companies(env).await.map_err(company_error)?;
            if !companies
                .iter()
                .any(|company| company.get() == self.company)
            {
                return Err(Error::access(COMPANY_ACCESS));
            }
        }
        let company = env.records("res.company")?.browse([self.company])?;
        company
            .check_access(tx, operation)
            .await
            .map_err(company_error)?;
        if company.exists(tx).await?.is_empty() {
            return Err(Error::access(COMPANY_ACCESS));
        }
        if operation == Operation::Write
            && !env.su()
            && !tx.has_group(env, "account.group_account_manager").await?
        {
            return Err(Error::access(
                "Only accounting administrators can change company default accounts",
            ));
        }
        Ok(())
    }

    async fn company_name(self, tx: &mut Transaction, env: &Env) -> Result<Arc<str>> {
        let company = env.records("res.company")?.browse([self.company])?;
        let Cell::Str(name) = company.cell(tx, "name").await? else {
            return Err(Error::invariant("An accounting company has no name"));
        };
        Ok(name)
    }

    async fn role_id(self, tx: &mut Transaction) -> Result<i64> {
        let rows = tx
            .query(
                "SELECT id FROM account_default_role WHERE key = $1",
                &[&self.role.key()],
            )
            .await?;
        rows.first()
            .ok_or_else(|| Error::programming(INVALID_ROLE))?
            .try_get(0)
            .map_err(decode)
    }

    async fn validate_account(
        self,
        tx: &mut Transaction,
        env: &Env,
        account_id: i64,
        lock: bool,
    ) -> Result<()> {
        let account = env.records("account.account")?.browse([account_id])?;
        account
            .check_access(tx, Operation::Read)
            .await
            .map_err(target_error)?;
        let fields = ["active", "account_type", "company_ids"].map(|field| {
            account
                .spec()
                .field(field)
                .map(|field| field.id)
                .ok_or_else(|| Error::programming(format!("Missing account field {field}")))
        });
        let [active, account_type, companies] = fields;
        tx.flush_fields(account.model_id(), &[active?, account_type?, companies?])
            .await?;
        // Taking an account row share lock during a configuration write also stabilizes its type
        // and active state. Membership and company-tree mutation fences belong to ACCMAP-020.
        let lock_clause = if lock { " FOR SHARE OF account" } else { "" };
        let sql = format!(
            "SELECT account.active, account.account_type, EXISTS (SELECT 1 \
             FROM account_account_res_company_rel m JOIN res_company a ON a.id = m.res_company_id \
             WHERE m.account_account_id = account.id AND {ANCESTOR_OF_C}) \
             FROM account_account account CROSS JOIN res_company c \
             WHERE account.id = $1 AND c.id = $2{lock_clause}"
        );
        let rows = tx.query(&sql, &[&account_id, &self.company]).await?;
        let row = rows.first().ok_or_else(|| Error::access(TARGET_ACCESS))?;
        let active: bool = row.try_get(0).map_err(decode)?;
        let account_type: &str = row.try_get(1).map_err(decode)?;
        let applicable: bool = row.try_get(2).map_err(decode)?;
        if !applicable {
            let name = self.company_name(tx, env).await?;
            return Err(Error::user(format!(
                "Account is not applicable to company {name}"
            )));
        }
        if !active || !self.role.accepts(account_type) {
            return Err(Error::user(format!(
                "Configured account is inactive or invalid for role {}",
                self.role.key().replace('_', " ")
            )));
        }
        Ok(())
    }
}

fn company_error(error: Error) -> Error {
    if matches!(error.kind(), ErrorKind::Access | ErrorKind::Missing) {
        return Error::access(COMPANY_ACCESS);
    }
    error
}

fn target_error(error: Error) -> Error {
    if matches!(error.kind(), ErrorKind::Access | ErrorKind::Missing) {
        return Error::access(TARGET_ACCESS);
    }
    error
}

fn decode(error: impl std::fmt::Display) -> Error {
    Error::db(format!(
        "Accounting configuration could not be read: {error}"
    ))
}
