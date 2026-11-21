// SPDX-License-Identifier: MIT OR Apache-2.0
//! Upgrade built-in provider bindings and menu defaults without replacing edited report definitions.
use erp_sdk::migrations::SemverVersion;
use erp_sdk::prelude::*;

#[erp_sdk::migration(addon = "account_reporting", version = "1.6", stage = post)]
async fn guard_builtin_balance_sheet(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
    tx.execute(
        "UPDATE reporting_report r SET provider = 'account_reporting.balance_sheet' \
         FROM ir_model_data d WHERE d.module = 'account_reporting' AND d.name = 'report_balance_sheet' \
           AND d.model = 'reporting.report' AND d.res_id = r.id AND r.provider = 'reporting.definition'",
        &[],
    ).await?;
    Ok(())
}

#[erp_sdk::migration(addon = "account_reporting", version = "1.8", stage = post)]
async fn guard_builtin_partner_ledger(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
    tx.execute(
        "UPDATE reporting_report r SET provider = 'account_reporting.partner_ledger' \
         FROM ir_model_data d WHERE d.module = 'account_reporting' AND d.name = 'report_partner_ledger' \
           AND d.model = 'reporting.report' AND d.res_id = r.id AND r.provider = 'reporting.definition'",
        &[],
    ).await?;
    Ok(())
}

#[erp_sdk::migration(addon = "account_reporting", version = "1.9", stage = post)]
async fn general_ledger_menu_glyph(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
    // The GL definition file is protected on upgrade. Preserve any icon selected by an administrator.
    tx.execute(
        "UPDATE ir_ui_menu m SET web_icon = 'lucide:book-open' FROM ir_model_data d \
         WHERE d.module = 'account_reporting' AND d.name = 'menu_general_ledger' \
         AND d.model = 'ir.ui.menu' AND d.res_id = m.id AND COALESCE(m.web_icon, '') = ''",
        &[],
    )
    .await?;
    Ok(())
}
