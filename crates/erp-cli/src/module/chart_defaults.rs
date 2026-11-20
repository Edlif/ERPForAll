// SPDX-License-Identifier: MIT OR Apache-2.0
//! The company default account stage of chart activation, after account identities are installed.

use std::collections::BTreeMap;

use erp_addon_account::mapping::{CompanyDefault, DefaultRole};
use erp_modules::pack::{Record, Value};
use erp_sdk::{Error, Result, Transaction};

const ROLES: [(DefaultRole, &str); 5] = [
    (DefaultRole::Receivable, "receivable_account_id"),
    (DefaultRole::Payable, "payable_account_id"),
    (DefaultRole::Income, "income_account_id"),
    (DefaultRole::Expense, "expense_account_id"),
    (DefaultRole::StockValuation, "account_stock_valuation_id"),
];

/// Validated company account default plan for one staged chart and one target company.
pub struct ChartAccountDefaults {
    choices: Vec<(CompanyDefault, Option<i64>)>,
}

impl ChartAccountDefaults {
    /// Plan the declared account defaults of a staged company record.
    ///
    /// Supply the installed account identities of the selected template, keyed by the
    /// staged reference's canonical key (plain or dependency-qualified). Omission
    /// leaves a role alone, and an explicit pack null prepares an empty choice.
    /// The caller owns the account materialization phase and its activation snapshot.
    ///
    /// # Errors
    ///
    /// A company identifier, staged default value or resolved account identity is invalid,
    /// or a declared account reference has no installed identity.
    pub fn prepare(
        company: i64,
        staged_company: &Record,
        accounts_by_key: &BTreeMap<String, i64>,
    ) -> Result<Self> {
        if company <= 0 {
            return Err(Error::validation(
                "Invalid accounting configuration identifier",
            ));
        }
        if staged_company.model != "res.company" {
            return Err(Error::validation(
                "Chart account defaults require a staged company record",
            ));
        }
        let mut choices = Vec::with_capacity(ROLES.len());
        for (role, field) in ROLES {
            let Some(value) = staged_company.fields.get(field) else {
                continue;
            };
            let account = match value {
                Value::Null => None,
                Value::Ref { .. } => {
                    let key = value.canon();
                    let account = accounts_by_key.get(&key).copied().ok_or_else(|| {
                        Error::validation(format!(
                            "Company default {field} has no resolved account for key {key}."
                        ))
                    })?;
                    if account <= 0 {
                        return Err(Error::validation(format!(
                            "Company default {field} has an invalid account identity for key {key}."
                        )));
                    }
                    Some(account)
                }
                _ => {
                    return Err(Error::validation(format!(
                        "Company default {field} requires a staged account reference."
                    )));
                }
            };
            choices.push((CompanyDefault::new(company, role)?, account));
        }
        Ok(Self { choices })
    }

    /// Seed prepared company defaults in the caller's activation transaction.
    ///
    /// All choices use one savepoint. An existing value, including an empty choice set by
    /// an administrator, takes precedence over the seed. Release approval and the decisions
    /// to replace configured values belong to the surrounding activation workflow.
    ///
    /// # Errors
    ///
    /// Company or account access refusal, a target invalid for its role, or a database error.
    pub async fn apply(self, tx: &mut Transaction) -> Result<()> {
        if self.choices.is_empty() {
            return Ok(());
        }
        tx.savepoint(async move |tx| {
            for (key, account) in self.choices {
                key.install_if_absent(tx, account).await?;
            }
            Ok(())
        })
        .await
    }
}
