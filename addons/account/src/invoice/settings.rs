// SPDX-License-Identifier: MIT OR Apache-2.0
//! The invoice settings: company-scope declared settings in the "Invoices" block of the
//! Accounting settings page. They name, by account code, the receivable account of customer
//! invoices and the payable account of vendor bills for a company that has several accounts of
//! that type; a company with one such account needs neither.

use erp_addon_base::ir_config_setting::read_values;
use erp_sdk::prelude::*;
use erp_sdk::serde_json;
use erp_sdk::settings::{ScopeKind, SettingDecl};
use erp_sdk::views::wire::{SettingsScope, WireId};

/// Code of the receivable account of customer invoices and credit notes.
pub const RECEIVABLE_SETTING: &str = "account.invoice_receivable_account_code";
/// Code of the payable account of vendor bills and credit notes.
pub const PAYABLE_SETTING: &str = "account.invoice_payable_account_code";
/// Label of [`RECEIVABLE_SETTING`].
pub const RECEIVABLE_LABEL: &str =
    "Invoices: receivable account of customer invoices (account code)";
/// Label of [`PAYABLE_SETTING`].
pub const PAYABLE_LABEL: &str = "Invoices: payable account of vendor bills (account code)";
/// The settings block both settings are shown in.
pub const BLOCK: &str = "Invoices";
/// Longest account code a setting takes, in bytes: the size of `account.account.code`.
const CODE_MAX_BYTES: usize = 64;

fn settings() -> Result<Vec<SettingDecl>> {
    let place = |decl: SettingDecl, sequence: u32| -> Result<SettingDecl> {
        decl.block(BLOCK)?.sequence(sequence).access(
            &["account.group_account_readonly"],
            &["account.group_account_manager"],
        )
    };
    Ok(vec![
        place(
            SettingDecl::string(
                RECEIVABLE_SETTING,
                RECEIVABLE_LABEL,
                ScopeKind::Company,
                "",
                CODE_MAX_BYTES,
            )?,
            10,
        )?,
        place(
            SettingDecl::string(
                PAYABLE_SETTING,
                PAYABLE_LABEL,
                ScopeKind::Company,
                "",
                CODE_MAX_BYTES,
            )?,
            20,
        )?,
    ])
}
erp_sdk::setting_declarations!(settings);

/// What the sync needs to know about the setting of one account type.
pub(super) struct Choice {
    /// "receivable" or "payable", as messages name the account.
    pub kind: &'static str,
    /// The setting key.
    pub key: &'static str,
    /// The setting label, as the settings page shows it.
    pub label: &'static str,
}

/// The setting that chooses the account of type `account_type`.
///
/// # Errors
///
/// `Invariant` for an account type that is neither receivable nor payable.
pub(super) fn choice_of(account_type: &str) -> Result<Choice> {
    match account_type {
        "asset_receivable" => Ok(Choice {
            kind: "receivable",
            key: RECEIVABLE_SETTING,
            label: RECEIVABLE_LABEL,
        }),
        "liability_payable" => Ok(Choice {
            kind: "payable",
            key: PAYABLE_SETTING,
            label: PAYABLE_LABEL,
        }),
        other => Err(Error::invariant(format!(
            "an invoice has no receivable or payable account of type {other}"
        ))),
    }
}

/// The account code `company` chose for accounts of type `account_type`; `None` when the setting
/// is empty (its default). A company-scope value is stored per company, not per root company, and
/// this one is read on the invoice's own company: a branch chooses among the accounts it can use,
/// which may differ from its parent's. The framework reads its own configuration with elevated
/// rights; the caller needs no settings group.
///
/// # Errors
///
/// Settings read failures; a stored value that is not a text.
pub(super) async fn configured_code(
    tx: &mut Transaction,
    company: i64,
    account_type: &str,
) -> Result<Option<String>> {
    let key = choice_of(account_type)?.key;
    let env = tx.env().sudo(true).with_company(company)?;
    let id = u64::try_from(company)
        .ok()
        .and_then(|id| WireId::try_from(id).ok())
        .ok_or_else(|| Error::validation(format!("company id {company} is invalid")))?;
    let read = read_values(
        &env,
        tx,
        SettingsScope::Company { company_id: id },
        vec![key.to_owned()],
    )
    .await?;
    let read = serde_json::to_value(&read)
        .map_err(|error| Error::internal(format!("invoice account settings: {error}")))?;
    let value = read["values"]
        .as_array()
        .and_then(|values| values.iter().find(|v| v["key"] == key))
        .map(|entry| entry["value"].clone())
        .ok_or_else(|| Error::internal(format!("{key} was not read")))?;
    let code = value
        .as_str()
        .ok_or_else(|| Error::internal(format!("{key} is not a text")))?;
    Ok(Some(code.trim().to_owned()).filter(|code| !code.is_empty()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    /// The two settings are company settings with an empty default, written by accounting
    /// managers only.
    #[test]
    fn the_account_settings_are_empty_company_settings() {
        let declarations = settings().unwrap();
        let keys: Vec<&str> = declarations.iter().map(|d| d.key()).collect();
        assert_eq!(keys, [RECEIVABLE_SETTING, PAYABLE_SETTING]);
        for decl in &declarations {
            assert_eq!(decl.write_groups(), &["account.group_account_manager"]);
            assert_eq!(decl.default_value(), &Value::Str("".into()));
            assert_eq!(decl.block_title(), BLOCK);
        }
        assert_eq!(declarations[0].label(), RECEIVABLE_LABEL);
        assert_eq!(declarations[1].label(), PAYABLE_LABEL);
    }
}
