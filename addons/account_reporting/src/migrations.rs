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

#[erp_sdk::migration(addon = "account_reporting", version = "1.12", stage = post)]
async fn upgrade_aged_and_analysis_ledger_layout(
    tx: &mut Transaction,
    _from: &SemverVersion,
) -> Result<()> {
    tx.execute(
        "UPDATE reporting_report r \
         SET date_mode = 'single', date_default = 'today', comparison = false, totals_below = true \
         FROM ir_model_data d \
         WHERE d.module = 'account_reporting' \
           AND d.name IN ('report_aged_receivable', 'report_aged_payable') \
           AND d.model = 'reporting.report' AND d.res_id = r.id",
        &[],
    )
    .await?;
    tx.execute(
        "UPDATE reporting_report r SET totals_below = true \
         FROM ir_model_data d \
         WHERE d.module = 'account_reporting' \
           AND d.name IN ('report_tax_report', 'report_invoice_analysis', 'report_analytic_report') \
           AND d.model = 'reporting.report' AND d.res_id = r.id",
        &[],
    )
    .await?;
    for (report_xmlid, col_xmlid) in [
        ("report_aged_receivable", "aged_ar_col_due"),
        ("report_aged_payable", "aged_ap_col_due"),
    ] {
        let rows = tx
            .query(
                "SELECT r.id::int8 FROM reporting_report r \
                 JOIN ir_model_data d ON d.model = 'reporting.report' AND d.res_id = r.id \
                 WHERE d.module = 'account_reporting' AND d.name = $1",
                &[&report_xmlid],
            )
            .await?;
        let Some(row) = rows.first() else {
            continue;
        };
        let report_id: i64 = row
            .try_get(0)
            .map_err(|e| Error::db(format!("aged migration: {e}")))?;
        let exists = tx
            .query(
                "SELECT 1 FROM reporting_report_column WHERE report_id = $1 AND label = 'due_date' LIMIT 1",
                &[&report_id],
            )
            .await?;
        if exists.is_empty() {
            let env = tx.env().sudo(true);
            let created = env
                .records("reporting.report.column")?
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("report_id", Cell::Id(report_id))
                            .set_cell("sequence", Cell::Int(5))
                            .set_cell("name", Cell::Str("Due Date".into()))
                            .set_cell("label", Cell::Str("due_date".into()))
                            .set_cell("kind", Cell::Str("date".into())),
                    ],
                )
                .await?;
            let col_id = created.ensure_one()?.int()?;
            tx.execute(
                "INSERT INTO ir_model_data (module, name, model, res_id, noupdate) \
                 VALUES ('account_reporting', $1, 'reporting.report.column', $2, true) \
                 ON CONFLICT (module, name) DO NOTHING",
                &[&col_xmlid, &col_id],
            )
            .await?;
        }
    }
    Ok(())
}
