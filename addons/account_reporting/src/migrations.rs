// SPDX-License-Identifier: MIT OR Apache-2.0
//! Upgrade only the builtin provider binding; protected editable report content stays intact.
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
