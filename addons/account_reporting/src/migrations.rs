// SPDX-License-Identifier: MIT OR Apache-2.0
//! Upgrade built-in provider bindings, menu defaults and report definitions cleanly across versions.
use erp_sdk::migrations::SemverVersion;
use erp_sdk::prelude::*;

#[erp_sdk::migration(addon = "account_reporting", version = "1.6", stage = post)]
async fn guard_builtin_balance_sheet(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
    tx.execute(
        "UPDATE reporting_report r SET provider = 'account_reporting.balance_sheet' \
         FROM ir_model_data d WHERE d.module = 'account_reporting' AND d.name = 'report_balance_sheet' \
           AND d.model = 'reporting.report' AND d.res_id = r.id AND r.provider = 'reporting.definition'",
        &[],
    )
    .await?;
    Ok(())
}

#[erp_sdk::migration(addon = "account_reporting", version = "1.8", stage = post)]
async fn guard_builtin_partner_ledger(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
    tx.execute(
        "UPDATE reporting_report r SET provider = 'account_reporting.partner_ledger' \
         FROM ir_model_data d WHERE d.module = 'account_reporting' AND d.name = 'report_partner_ledger' \
           AND d.model = 'reporting.report' AND d.res_id = r.id AND r.provider = 'reporting.definition'",
        &[],
    )
    .await?;
    Ok(())
}

#[erp_sdk::migration(addon = "account_reporting", version = "1.9", stage = post)]
async fn general_ledger_menu_glyph(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
    tx.execute(
        "UPDATE ir_ui_menu m SET web_icon = 'lucide:book-open' FROM ir_model_data d \
         WHERE d.module = 'account_reporting' AND d.name = 'menu_general_ledger' \
         AND d.model = 'ir.ui.menu' AND d.res_id = m.id AND COALESCE(m.web_icon, '') = ''",
        &[],
    )
    .await?;
    Ok(())
}

async fn xmlid_res_id(tx: &mut Transaction, model: &str, name: &str) -> Result<Option<i64>> {
    let rows = tx
        .query(
            "SELECT res_id::int8 FROM ir_model_data \
             WHERE module = 'account_reporting' AND model = $1 AND name = $2",
            &[&model, &name],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(row.try_get(0).map_err(|e| {
        Error::db(format!("account_reporting migration xmlid: {e}"))
    })?))
}

async fn register_xmlid(tx: &mut Transaction, name: &str, model: &str, res_id: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO ir_model_data (module, name, model, res_id, noupdate) \
         VALUES ('account_reporting', $1, $2, $3, true) \
         ON CONFLICT (module, name) DO UPDATE SET res_id = EXCLUDED.res_id",
        &[&name, &model, &res_id],
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn ensure_column(
    tx: &mut Transaction,
    report_id: i64,
    xmlid: &str,
    sequence: i32,
    name: &str,
    label: &str,
    kind: &str,
    blank_if_zero: bool,
) -> Result<i64> {
    let existing = tx
        .query(
            "SELECT id::int8 FROM reporting_report_column WHERE report_id = $1 AND label = $2 LIMIT 1",
            &[&report_id, &label],
        )
        .await?;
    let env = tx.env().sudo(true);
    if let Some(row) = existing.first() {
        let id: i64 = row
            .try_get(0)
            .map_err(|e| Error::db(format!("read column id: {e}")))?;
        env.records("reporting.report.column")?
            .browse([id])?
            .write(
                tx,
                RawVals::new()
                    .set_cell("sequence", Cell::Int(sequence))
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("kind", Cell::Str(kind.into()))
                    .set_cell("blank_if_zero", Cell::Bool(blank_if_zero)),
            )
            .await?;
        register_xmlid(tx, xmlid, "reporting.report.column", id).await?;
        return Ok(id);
    }
    let created = env
        .records("reporting.report.column")?
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("report_id", Cell::Id(report_id))
                    .set_cell("sequence", Cell::Int(sequence))
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("label", Cell::Str(label.into()))
                    .set_cell("kind", Cell::Str(kind.into()))
                    .set_cell("blank_if_zero", Cell::Bool(blank_if_zero)),
            ],
        )
        .await?;
    let col_id = created.ensure_one()?.int()?;
    register_xmlid(tx, xmlid, "reporting.report.column", col_id).await?;
    Ok(col_id)
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
        let Some(report_id) = xmlid_res_id(tx, "reporting.report", report_xmlid).await? else {
            continue;
        };
        ensure_column(
            tx, report_id, col_xmlid, 5, "Due Date", "due_date", "date", false,
        )
        .await?;
    }
    Ok(())
}

/// Prepare existing `noupdate="1"` parent lines before XML data files insert the new classified
/// Balance Sheet & Profit and Loss children and ledger columns.
#[erp_sdk::migration(addon = "account_reporting", version = "1.13", stage = pre)]
async fn prepare_core_accounting_reports_pre(
    tx: &mut Transaction,
    _from: &SemverVersion,
) -> Result<()> {
    // 1. Balance Sheet: convert ASSET & LIABILITY containers from grouped lines into section parents.
    if let Some(bs_id) = xmlid_res_id(tx, "reporting.report", "report_balance_sheet").await? {
        let needs_bs = tx
            .query(
                "SELECT 1 FROM reporting_report_line WHERE report_id = $1 AND code = 'CURRENT_ASSETS' LIMIT 1",
                &[&bs_id],
            )
            .await?
            .is_empty();
        if needs_bs {
            tx.execute(
                "UPDATE reporting_report_line \
                 SET groupby = NULL, foldable = 'always_unfolded' \
                 WHERE report_id = $1 AND code IN ('ASSET', 'LIABILITY', 'EQUITY')",
                &[&bs_id],
            )
            .await?;
            tx.execute(
                "DELETE FROM reporting_report_expression e \
                 USING reporting_report_line l \
                 WHERE e.line_id = l.id AND l.report_id = $1 AND l.code IN ('ASSET', 'LIABILITY')",
                &[&bs_id],
            )
            .await?;
            tx.execute(
                "DELETE FROM ir_model_data \
                 WHERE module = 'account_reporting' AND model = 'reporting.report.expression' \
                   AND name IN ('position_asset_booked', 'position_liability_booked', 'position_liability_display')",
                &[],
            )
            .await?;
        }
    }

    // 2. Profit and Loss: convert INCOME & EXPENSE containers from grouped lines into section parents.
    if let Some(pl_id) = xmlid_res_id(tx, "reporting.report", "report_profit_loss").await? {
        let needs_pl = tx
            .query(
                "SELECT 1 FROM reporting_report_line WHERE report_id = $1 AND code = 'GROSS_PROFIT' LIMIT 1",
                &[&pl_id],
            )
            .await?
            .is_empty();
        if needs_pl {
            tx.execute(
                "UPDATE reporting_report_line \
                 SET groupby = NULL, foldable = 'always_unfolded' \
                 WHERE report_id = $1 AND code IN ('INCOME', 'EXPENSE')",
                &[&pl_id],
            )
            .await?;
            tx.execute(
                "DELETE FROM reporting_report_expression e \
                 USING reporting_report_line l \
                 WHERE e.line_id = l.id AND l.report_id = $1 AND l.code IN ('INCOME', 'EXPENSE')",
                &[&pl_id],
            )
            .await?;
            tx.execute(
                "DELETE FROM ir_model_data \
                 WHERE module = 'account_reporting' AND model = 'reporting.report.expression' \
                   AND name IN ('profit_loss_income_booked', 'profit_loss_income_amount', 'profit_loss_expense_booked')",
                &[],
            )
            .await?;
        }
    }

    // 3. Partner Ledger: remove legacy definition lines & old opening/closing columns before XML adds ledger columns.
    if let Some(pl_id) = xmlid_res_id(tx, "reporting.report", "report_partner_ledger").await? {
        let has_journal = !tx
            .query(
                "SELECT 1 FROM reporting_report_column WHERE report_id = $1 AND label = 'journal' LIMIT 1",
                &[&pl_id],
            )
            .await?
            .is_empty();
        if !has_journal {
            tx.execute(
                "DELETE FROM reporting_report_line WHERE report_id = $1",
                &[&pl_id],
            )
            .await?;
            tx.execute(
                "DELETE FROM reporting_report_column WHERE report_id = $1 AND label IN ('opening', 'closing')",
                &[&pl_id],
            )
            .await?;
            tx.execute(
                "DELETE FROM ir_model_data \
                 WHERE module = 'account_reporting' \
                   AND name IN ('partner_ledger_receivable', 'partner_ledger_payable', \
                                'partner_ledger_column_opening', 'partner_ledger_column_closing')",
                &[],
            )
            .await?;
        }
    }

    // 4. General Ledger & Trial Balance: clear legacy wrapper/synthetic lines before XML load.
    if let Some(gl_id) = xmlid_res_id(tx, "reporting.report", "report_general_ledger").await? {
        tx.execute(
            "DELETE FROM reporting_report_line WHERE report_id = $1 \
             AND code IN ('GL', 'OPENING', 'CLOSING', 'GL_FISCAL', 'OPENING_FISCAL', 'CLOSING_FISCAL', 'UNALLOCATED')",
            &[&gl_id],
        )
        .await?;
    }
    if let Some(tb_id) = xmlid_res_id(tx, "reporting.report", "report_ledger_trial_balance").await?
    {
        tx.execute(
            "DELETE FROM reporting_report_line WHERE report_id = $1 AND code = 'TRIAL_BALANCE'",
            &[&tb_id],
        )
        .await?;
    }

    Ok(())
}

#[erp_sdk::migration(addon = "account_reporting", version = "1.13", stage = post)]
async fn upgrade_core_accounting_reports_post(
    tx: &mut Transaction,
    _from: &SemverVersion,
) -> Result<()> {
    // 1. Partner Ledger flags & column order/names.
    if let Some(pl_id) = xmlid_res_id(tx, "reporting.report", "report_partner_ledger").await? {
        tx.execute(
            "UPDATE reporting_report \
             SET provider = 'account_reporting.partner_ledger', date_mode = 'range', \
                 date_default = 'this_year', comparison = false, totals_below = true, \
                 unfold_all = true, search_bar = true, hide_zero = 'optional' \
             WHERE id = $1",
            &[&pl_id],
        )
        .await?;
        ensure_column(
            tx,
            pl_id,
            "partner_ledger_column_journal",
            10,
            "Journal",
            "journal",
            "text",
            false,
        )
        .await?;
        ensure_column(
            tx,
            pl_id,
            "partner_ledger_column_account",
            20,
            "Account",
            "account",
            "text",
            false,
        )
        .await?;
        ensure_column(
            tx,
            pl_id,
            "partner_ledger_column_date",
            30,
            "Date",
            "date",
            "date",
            false,
        )
        .await?;
        ensure_column(
            tx,
            pl_id,
            "partner_ledger_column_due_date",
            40,
            "Due Date",
            "due_date",
            "date",
            false,
        )
        .await?;
        ensure_column(
            tx,
            pl_id,
            "partner_ledger_column_matching",
            50,
            "Matching",
            "matching",
            "text",
            false,
        )
        .await?;
        ensure_column(
            tx,
            pl_id,
            "partner_ledger_column_debit",
            60,
            "Debit",
            "debit",
            "monetary",
            true,
        )
        .await?;
        ensure_column(
            tx,
            pl_id,
            "partner_ledger_column_credit",
            70,
            "Credit",
            "credit",
            "monetary",
            true,
        )
        .await?;
        ensure_column(
            tx,
            pl_id,
            "partner_ledger_column_balance",
            80,
            "Balance",
            "balance",
            "monetary",
            false,
        )
        .await?;
    }

    // 2. General Ledger flags & clean column names + Partner column.
    if let Some(gl_id) = xmlid_res_id(tx, "reporting.report", "report_general_ledger").await? {
        tx.execute(
            "UPDATE reporting_report \
             SET date_mode = 'range', date_default = 'this_year', comparison = false, \
                 totals_below = true, unfold_all = true, search_bar = true, hide_zero = 'optional' \
             WHERE id = $1",
            &[&gl_id],
        )
        .await?;
        ensure_column(
            tx,
            gl_id,
            "gl_column_date",
            10,
            "Date",
            "date",
            "date",
            false,
        )
        .await?;
        ensure_column(
            tx,
            gl_id,
            "gl_column_entry",
            20,
            "Journal Entry",
            "entry",
            "text",
            false,
        )
        .await?;
        ensure_column(
            tx,
            gl_id,
            "gl_column_partner",
            25,
            "Partner",
            "partner",
            "text",
            false,
        )
        .await?;
        ensure_column(
            tx,
            gl_id,
            "gl_column_debit",
            30,
            "Debit",
            "debit",
            "monetary",
            true,
        )
        .await?;
        ensure_column(
            tx,
            gl_id,
            "gl_column_credit",
            40,
            "Credit",
            "credit",
            "monetary",
            true,
        )
        .await?;
        ensure_column(
            tx,
            gl_id,
            "gl_column_balance",
            50,
            "Balance",
            "balance",
            "monetary",
            false,
        )
        .await?;
    }

    // 3. Trial Balance provider, plain title/menu/action & clean columns.
    if let Some(tb_id) = xmlid_res_id(tx, "reporting.report", "report_ledger_trial_balance").await?
    {
        tx.execute(
            "UPDATE reporting_report \
             SET provider = 'account_reporting.trial_balance', \
                 date_mode = 'range', date_default = 'this_year', comparison = true, \
                 totals_below = false, unfold_all = false, search_bar = true, hide_zero = 'optional' \
             WHERE id = $1",
            &[&tb_id],
        )
        .await?;
        let env = tx.env().sudo(true);
        let tb_rec = env.records("reporting.report")?.browse([tb_id])?;
        if matches!(tb_rec.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Ledger Trial Balance")
        {
            tb_rec
                .write(
                    tx,
                    RawVals::new().set_cell("name", Cell::Str("Trial Balance".into())),
                )
                .await?;
        }
        if let Some(menu_id) = xmlid_res_id(tx, "ir.ui.menu", "menu_ledger_trial_balance").await? {
            let m = env.records("ir.ui.menu")?.browse([menu_id])?;
            if matches!(m.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Ledger Trial Balance")
            {
                m.write(
                    tx,
                    RawVals::new().set_cell("name", Cell::Str("Trial Balance".into())),
                )
                .await?;
            }
        }
        if let Some(act_id) =
            xmlid_res_id(tx, "ir.actions.client", "action_ledger_trial_balance").await?
        {
            let a = env.records("ir.actions.client")?.browse([act_id])?;
            if matches!(a.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Ledger Trial Balance")
            {
                a.write(
                    tx,
                    RawVals::new().set_cell("name", Cell::Str("Trial Balance".into())),
                )
                .await?;
            }
        }
        ensure_column(
            tx,
            tb_id,
            "trial_balance_column_opening",
            10,
            "Initial Balance",
            "opening",
            "monetary",
            false,
        )
        .await?;
        ensure_column(
            tx,
            tb_id,
            "trial_balance_column_debit",
            20,
            "Debit",
            "debit",
            "monetary",
            true,
        )
        .await?;
        ensure_column(
            tx,
            tb_id,
            "trial_balance_column_credit",
            30,
            "Credit",
            "credit",
            "monetary",
            true,
        )
        .await?;
        ensure_column(
            tx,
            tb_id,
            "trial_balance_column_closing",
            40,
            "End Balance",
            "closing",
            "monetary",
            false,
        )
        .await?;
    }

    // 4. Balance Sheet: finalize Equity parent hierarchy & totals_below on existing records.
    if let Some(bs_id) = xmlid_res_id(tx, "reporting.report", "report_balance_sheet").await? {
        tx.execute(
            "UPDATE reporting_report \
             SET date_mode = 'single', date_default = 'today', comparison = true, totals_below = true \
             WHERE id = $1",
            &[&bs_id],
        )
        .await?;
        if let Some(eq_id) = xmlid_res_id(tx, "reporting.report.line", "position_equity").await? {
            let env = tx.env().sudo(true);
            tx.execute(
                "UPDATE reporting_report_line \
                 SET sequence = 30, groupby = NULL, foldable = 'always_unfolded' \
                 WHERE id = $1",
                &[&eq_id],
            )
            .await?;
            tx.execute(
                "UPDATE reporting_report_expression SET formula = 'sum_children' \
                 WHERE line_id = $1 AND label = 'amount'",
                &[&eq_id],
            )
            .await?;
            let eq_rec = env.records("reporting.report.line")?.browse([eq_id])?;
            if matches!(eq_rec.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Total equity")
            {
                eq_rec
                    .write(
                        tx,
                        RawVals::new().set_cell("name", Cell::Str("Equity".into())),
                    )
                    .await?;
            }
            if let Some(earn_id) =
                xmlid_res_id(tx, "reporting.report.line", "position_earnings").await?
            {
                tx.execute(
                    "UPDATE reporting_report_line SET parent_id = $1, sequence = 31, groupby = 'account_id' WHERE id = $2",
                    &[&eq_id, &earn_id],
                )
                .await?;
                let r = env.records("reporting.report.line")?.browse([earn_id])?;
                if matches!(r.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Unallocated earnings")
                {
                    r.write(
                        tx,
                        RawVals::new().set_cell("name", Cell::Str("Unallocated Earnings".into())),
                    )
                    .await?;
                }
            }
            if let Some(cap_id) =
                xmlid_res_id(tx, "reporting.report.line", "position_capital").await?
            {
                tx.execute(
                    "UPDATE reporting_report_line SET parent_id = $1, sequence = 32, groupby = 'account_id' WHERE id = $2",
                    &[&eq_id, &cap_id],
                )
                .await?;
                let r = env.records("reporting.report.line")?.browse([cap_id])?;
                if matches!(r.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Posted equity")
                {
                    r.write(
                        tx,
                        RawVals::new()
                            .set_cell("name", Cell::Str("Retained Earnings & Capital".into())),
                    )
                    .await?;
                }
            }
            if let Some(fin_id) =
                xmlid_res_id(tx, "reporting.report.line", "position_financing").await?
            {
                tx.execute(
                    "UPDATE reporting_report_line SET sequence = 40 WHERE id = $1",
                    &[&fin_id],
                )
                .await?;
                let r = env.records("reporting.report.line")?.browse([fin_id])?;
                if matches!(r.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Liabilities and equity")
                {
                    r.write(
                        tx,
                        RawVals::new().set_cell("name", Cell::Str("Liabilities and Equity".into())),
                    )
                    .await?;
                }
            }
        }
    }

    // 5. Profit and Loss: finalize flags, column label & Net Profit capitalization (preserving admin renames).
    if let Some(pl_id) = xmlid_res_id(tx, "reporting.report", "report_profit_loss").await? {
        tx.execute(
            "UPDATE reporting_report \
             SET date_mode = 'range', date_default = 'this_year', comparison = true, totals_below = true \
             WHERE id = $1",
            &[&pl_id],
        )
        .await?;
        let env = tx.env().sudo(true);
        if let Some(col_id) =
            xmlid_res_id(tx, "reporting.report.column", "profit_loss_amount").await?
        {
            let c = env.records("reporting.report.column")?.browse([col_id])?;
            if matches!(c.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Amount") {
                c.write(
                    tx,
                    RawVals::new().set_cell("name", Cell::Str("Balance".into())),
                )
                .await?;
            }
        }
        if let Some(res_id) =
            xmlid_res_id(tx, "reporting.report.line", "profit_loss_result").await?
        {
            let r = env.records("reporting.report.line")?.browse([res_id])?;
            if matches!(r.cell(tx, "name").await?, Cell::Str(ref s) if s.as_ref() == "Net profit") {
                r.write(
                    tx,
                    RawVals::new().set_cell("name", Cell::Str("Net Profit".into())),
                )
                .await?;
            }
        }
    }

    // 6. Cash Flow Statement, Executive Summary, Account Movements, Posted Journal Activity.
    tx.execute(
        "UPDATE reporting_report r SET totals_below = true, date_default = 'this_year' \
         FROM ir_model_data d \
         WHERE d.module = 'account_reporting' \
           AND d.name IN ('report_cash_flow', 'report_executive_summary', 'report_account_movements', 'report_posted_journal_activity') \
           AND d.model = 'reporting.report' AND d.res_id = r.id",
        &[],
    )
    .await?;

    Ok(())
}
