// SPDX-License-Identifier: MIT OR Apache-2.0
//! Payment Follow-Up Levels (`account.followup.level`) and Customer Follow-Up Statements
//! (`account.followup.report`).

use erp_sdk::Operation;
use erp_sdk::prelude::*;
use erp_sdk::serde_json::json;

/// Pure exact-decimal overdue summary computed from open customer invoices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverdueSummary {
    pub total_due: Decimal,
    pub total_overdue: Decimal,
    pub overdue_count: i32,
    pub max_days_overdue: i32,
    pub matched_level_id: Option<i64>,
    pub status: &'static str,
}

/// Classifies open customer invoices `(due_date, amount_residual)` against `today` and configured
/// follow-up levels `(level_id, delay_days)`.
pub fn classify_overdue(
    invoices: &[(Date, Decimal)],
    today: Date,
    levels: &[(i64, i32)],
) -> OverdueSummary {
    let mut total_due = Decimal::ZERO;
    let mut total_overdue = Decimal::ZERO;
    let mut overdue_count = 0i32;
    let mut max_days_overdue = 0i32;

    for &(due, residual) in invoices {
        if residual <= Decimal::ZERO {
            continue;
        }
        total_due += residual;
        let days = today.days() - due.days();
        if days > 0 {
            total_overdue += residual;
            overdue_count += 1;
            if days > max_days_overdue {
                max_days_overdue = days;
            }
        }
    }

    let matched_level_id = if overdue_count > 0 {
        levels
            .iter()
            .filter(|&&(_, delay)| max_days_overdue >= delay)
            .max_by_key(|&&(_, delay)| delay)
            .map(|&(id, _)| id)
    } else {
        None
    };

    let status = if matched_level_id.is_some() {
        "in_need_of_action"
    } else if overdue_count > 0 {
        "with_overdue_invoices"
    } else {
        "no_action_needed"
    };

    OverdueSummary {
        total_due,
        total_overdue,
        overdue_count,
        max_days_overdue,
        matched_level_id,
        status,
    }
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Follow-up query failed: {err}"))
}

/// `account.followup.level`: configurable escalation step for overdue customer receivables.
#[model(
    name = "account.followup.level",
    description = "Follow-up Level",
    order = "delay, id",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "ancestor", empty = "deny")
)]
pub struct AccountFollowupLevel {
    /// Human-readable name (e.g. "First Reminder", "Final Notice").
    #[field(char, required = true, string = "Description")]
    pub name: Option<Arc<str>>,
    /// Number of days past the invoice due date before triggering this level.
    #[field(integer, required = true, default = 15, string = "Due Days")]
    pub delay: i32,
    /// Owning company.
    #[field(many2one, comodel = "res.company", required = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// Send email reminder when executing this follow-up level.
    #[field(boolean, default = true, string = "Send Email")]
    pub send_email: bool,
    /// Send SMS reminder when executing this follow-up level.
    #[field(boolean, default = false, string = "Send SMS")]
    pub send_sms: bool,
    /// Trigger automatically when invoices cross the due-days threshold.
    #[field(boolean, default = false, string = "Automatic")]
    pub auto_execute: bool,
    /// Attach open invoices to the reminder.
    #[field(boolean, default = true, string = "Attach Open Invoices")]
    pub join_invoices: bool,
    /// Reminder message template shown on customer follow-up statements.
    #[field(text, string = "Reminder Message")]
    pub note: Option<Arc<str>>,
}

#[methods(AccountFollowupLevel, addon = "account")]
impl AccountFollowupLevel {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountFollowupLevel>,
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

/// `account.followup.report`: customer overdue statement and follow-up tracker.
#[model(
    name = "account.followup.report",
    description = "Customer Follow-up",
    order = "total_overdue desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountFollowupReport {
    /// Auto-numbered follow-up reference (`FUP/00001`).
    #[field(
        char,
        required = true,
        readonly = true,
        copy = false,
        default = "New",
        string = "Reference"
    )]
    pub name: Option<Arc<str>>,
    /// Customer being followed up.
    #[field(
        many2one,
        comodel = "res.partner",
        required = true,
        check_company = true,
        ondelete = "restrict",
        string = "Customer"
    )]
    pub partner_id: Option<RecId>,
    /// Owning company.
    #[field(many2one, comodel = "res.company", required = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// Company currency for due/overdue totals.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Highest applicable follow-up level reached by overdue invoices.
    #[field(
        many2one,
        comodel = "account.followup.level",
        check_company = true,
        ondelete = "set null",
        string = "Follow-up Level"
    )]
    pub followup_level_id: Option<RecId>,
    /// Follow-up action status.
    #[field(
        selection,
        selection = [
            ("in_need_of_action", "In Need of Action"),
            ("with_overdue_invoices", "With Overdue Invoices"),
            ("no_action_needed", "No Action Needed"),
        ],
        required = true,
        readonly = true,
        default = "no_action_needed",
        string = "Follow-up Status"
    )]
    pub status: Option<Arc<str>>,
    /// Total open receivable amount due from this customer.
    #[field(
        monetary,
        currency_field = "currency_id",
        readonly = true,
        string = "Total Due"
    )]
    pub total_due: Decimal,
    /// Portion of open receivables already past due date.
    #[field(
        monetary,
        currency_field = "currency_id",
        readonly = true,
        string = "Total Overdue"
    )]
    pub total_overdue: Decimal,
    /// Number of overdue posted customer invoices.
    #[field(integer, readonly = true, string = "Overdue Invoices")]
    pub overdue_invoice_count: i32,
    /// Age in days of the oldest overdue invoice.
    #[field(integer, readonly = true, string = "Max Days Overdue")]
    pub max_days_overdue: i32,
    /// Date of the most recent reminder sent.
    #[field(date, readonly = true, string = "Last Reminder")]
    pub last_reminder_date: Option<Date>,
    /// Next scheduled follow-up reminder date.
    #[field(date, string = "Next Reminder")]
    pub next_reminder_date: Option<Date>,
    /// Itemized breakdown of open/overdue invoices and reminder log.
    #[field(text, readonly = true, string = "Statement Summary")]
    pub summary: Option<Arc<str>>,
}

#[methods(AccountFollowupReport, addon = "account")]
impl AccountFollowupReport {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountFollowupReport>,
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

    #[method(override)]
    async fn create(
        this: &RecordSet<AccountFollowupReport>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        for vals in &mut vals_list {
            let needs_name = match vals.get("name") {
                None | Some(Cell::Null) => true,
                Some(Cell::Str(s)) => s.trim().is_empty() || s.as_ref() == "New",
                _ => false,
            };
            if needs_name {
                let rows = tx
                    .query(
                        "SELECT count(*)::int8 + 1 FROM account_followup_report",
                        &[],
                    )
                    .await?;
                let next: i64 = rows.first().and_then(|r| r.try_get(0).ok()).unwrap_or(1);
                vals.insert("name".into(), Cell::Str(format!("FUP/{next:05}").into()));
            }
        }
        let created = sup.call(tx, this.as_records(), (vals_list,)).await?;
        for rec in created.iter() {
            let _: () = rec.call(tx, "action_compute_overdue", ()).await?;
        }
        Ok(created)
    }

    /// Recomputes exact-decimal open/overdue balances, matches the appropriate follow-up level,
    /// and rebuilds the itemized customer statement summary.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_compute_overdue(
        this: &RecordSet<AccountFollowupReport>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records().check_access(tx, Operation::Write).await?;
        for rec in this.iter() {
            let fid = rec.as_records().ids()[0];
            let partner_id = rec
                .get(tx, AccountFollowupReport::PARTNER_ID)
                .await?
                .ok_or_else(|| Error::validation("Choose a customer for this follow-up."))?
                .get();
            let company_id = rec
                .get(tx, AccountFollowupReport::COMPANY_ID)
                .await?
                .ok_or_else(|| Error::validation("Choose a company for this follow-up."))?
                .get();
            let today = erp_addon_base::company_calendar::business_date(tx, company_id)
                .await
                .unwrap_or_else(|_| Date::today());

            let inv_rows = tx
                .query(
                    "SELECT COALESCE(name, 'Invoice')::text, \
                            COALESCE(invoice_date_due, invoice_date, date), \
                            COALESCE(amount_residual, 0) \
                     FROM account_move \
                     WHERE partner_id = $1 AND company_id = $2 \
                       AND state = 'posted' AND move_type = 'out_invoice' \
                       AND payment_state IN ('not_paid', 'partial') \
                       AND COALESCE(amount_residual, 0) > 0 \
                     ORDER BY COALESCE(invoice_date_due, date), id",
                    &[&partner_id, &company_id],
                )
                .await?;
            let mut pairs = Vec::with_capacity(inv_rows.len());
            let mut lines = Vec::new();
            for row in &inv_rows {
                let num: String = row.try_get(0).map_err(db)?;
                let due: Date = row.try_get(1).map_err(db)?;
                let residual: Decimal = row.try_get(2).map_err(db)?;
                pairs.push((due, residual));
                let days = today.days() - due.days();
                let flag = if days > 0 {
                    format!("{days} day(s) overdue")
                } else {
                    "not yet due".to_owned()
                };
                lines.push(format!(
                    "- {num} | Due {due} | Remaining {residual} ({flag})"
                ));
            }

            let level_rows = tx
                .query(
                    "SELECT id::int8, delay FROM account_followup_level \
                     WHERE company_id = $1 ORDER BY delay, id",
                    &[&company_id],
                )
                .await?;
            let levels: Vec<(i64, i32)> = level_rows
                .iter()
                .map(|r| Ok((r.try_get(0).map_err(db)?, r.try_get(1).map_err(db)?)))
                .collect::<Result<_>>()?;

            let summary_calc = classify_overdue(&pairs, today, &levels);
            let summary_text = if lines.is_empty() {
                "No open customer invoices.".to_owned()
            } else {
                lines.join("\n")
            };

            tx.execute(
                "UPDATE account_followup_report \
                 SET total_due = $2, total_overdue = $3, overdue_invoice_count = $4, \
                     max_days_overdue = $5, followup_level_id = $6, status = $7, \
                     summary = $8, write_date = now() AT TIME ZONE 'UTC' \
                 WHERE id = $1",
                &[
                    &fid,
                    &summary_calc.total_due,
                    &summary_calc.total_overdue,
                    &summary_calc.overdue_count,
                    &summary_calc.max_days_overdue,
                    &summary_calc.matched_level_id,
                    &summary_calc.status,
                    &summary_text,
                ],
            )
            .await?;
            tx.invalidate_records(
                rec.as_records(),
                Some(&[
                    "total_due",
                    "total_overdue",
                    "overdue_invoice_count",
                    "max_days_overdue",
                    "followup_level_id",
                    "status",
                    "summary",
                ]),
            )
            .await?;
        }
        Ok(())
    }

    /// Sends a follow-up reminder for overdue invoices, logs the dispatch date, schedules the
    /// next reminder, and marks the follow-up status as up to date.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_send_reminder(
        this: &RecordSet<AccountFollowupReport>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.ensure_one()?;
        Self::action_compute_overdue(rec, tx).await?;
        let fid = rec.as_records().ids()[0];
        let total_due = rec.get(tx, AccountFollowupReport::TOTAL_DUE).await?;
        if total_due <= Decimal::ZERO {
            return Err(Error::user(
                "This customer has no open balance requiring a reminder.",
            ));
        }
        let company_id = rec
            .get(tx, AccountFollowupReport::COMPANY_ID)
            .await?
            .map(|id| id.get())
            .unwrap_or(1);
        let today = erp_addon_base::company_calendar::business_date(tx, company_id)
            .await
            .unwrap_or_else(|_| Date::today());
        let next_date = Date::from_days(today.days() + 7);
        let prev = rec
            .get(tx, AccountFollowupReport::SUMMARY)
            .await?
            .unwrap_or_default();
        let updated_summary =
            format!("{prev}\n[Reminder sent on {today} — Total Due: {total_due}]");
        tx.execute(
            "UPDATE account_followup_report \
             SET last_reminder_date = $2, next_reminder_date = $3, \
                 status = 'no_action_needed', summary = $4, \
                 write_date = now() AT TIME ZONE 'UTC' \
             WHERE id = $1",
            &[&fid, &today, &next_date, &updated_summary],
        )
        .await?;
        tx.invalidate_records(
            rec.as_records(),
            Some(&[
                "last_reminder_date",
                "next_reminder_date",
                "status",
                "summary",
            ]),
        )
        .await?;
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.client",
            "tag": "display_notification",
            "params": {
                "message": format!("Follow-up reminder logged for {today} (next due {next_date})."),
                "type": "success",
            },
        }))))
    }

    /// Opens the customer's open posted invoices in a list/form view.
    #[method]
    #[rpc(effect = read)]
    pub async fn action_view_overdue_invoices(
        this: &RecordSet<AccountFollowupReport>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.ensure_one()?;
        let partner_id = rec
            .get(tx, AccountFollowupReport::PARTNER_ID)
            .await?
            .ok_or_else(|| Error::validation("No customer set on this follow-up."))?
            .get();
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.act_window",
            "name": "Open Customer Invoices",
            "res_model": "account.move",
            "target": "current",
            "views": [[false, "list"], [false, "form"]],
            "domain": [
                ["partner_id", "=", partner_id],
                ["state", "=", "posted"],
                ["move_type", "=", "out_invoice"],
                ["payment_state", "in", ["not_paid", "partial"]],
            ],
        }))))
    }
}

#[cfg(test)]
mod tests {
    use super::classify_overdue;
    use erp_sdk::{Date, Decimal};
    use std::str::FromStr;

    #[test]
    fn overdue_classification_computes_exact_totals_and_highest_level() {
        let today = Date::from_ymd(2026, 11, 25).unwrap();
        let invoices = vec![
            (
                Date::from_ymd(2026, 10, 20).unwrap(),
                Decimal::from_str("250.50").unwrap(),
            ), // 36 days overdue
            (
                Date::from_ymd(2026, 11, 5).unwrap(),
                Decimal::from_str("100.25").unwrap(),
            ), // 20 days overdue
            (
                Date::from_ymd(2026, 12, 10).unwrap(),
                Decimal::from_str("400.00").unwrap(),
            ), // future
        ];
        let levels = vec![(1, 15), (2, 30), (3, 60)];
        let res = classify_overdue(&invoices, today, &levels);
        assert_eq!(res.total_due, Decimal::from_str("750.75").unwrap());
        assert_eq!(res.total_overdue, Decimal::from_str("350.75").unwrap());
        assert_eq!(res.overdue_count, 2);
        assert_eq!(res.max_days_overdue, 36);
        assert_eq!(res.matched_level_id, Some(2));
        assert_eq!(res.status, "in_need_of_action");
    }
}
