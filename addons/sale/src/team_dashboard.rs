// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sales Team Dashboard (`crm.team` extension): live `dashboard_card` v1 JSON showing
//! Quotations, Confirmed Sales Orders, To Invoice, and Target vs Confirmed Sales.

use std::collections::BTreeMap;

use erp_addon_sales_team::CrmTeam;
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};

/// Pure exact-decimal achievement percentage against a team's invoicing target.
pub fn compute_target_achievement(confirmed_amount: Decimal, target: Decimal) -> Decimal {
    if target > Decimal::ZERO {
        ((confirmed_amount * Decimal::from(100)) / target).round_dp(1)
    } else {
        Decimal::ZERO
    }
}

/// Extension of `crm.team` providing Sales KPIs and a Kanban dashboard card.
#[extend(CrmTeam, addon = "sale")]
pub struct CrmTeamSaleDashboard {
    /// Contract v1 JSON rendered by `widget="dashboard_card"` on the Sales Teams Kanban board.
    #[field(
        text,
        compute = "_compute_sale_dashboard_card",
        string = "Sales Dashboard"
    )]
    pub sale_dashboard_card: Option<Arc<str>>,
}

fn db(e: impl std::fmt::Display) -> Error {
    Error::db(format!("sales team dashboard: {e}"))
}

#[methods(CrmTeam, addon = "sale")]
impl CrmTeamSaleDashboard {
    #[compute(
        depends = ["name", "invoiced_target", "company_id"],
        depends_context = ["uid", "allowed_company_ids"]
    )]
    async fn _compute_sale_dashboard_card(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
        out: &mut Assign<'_, CrmTeam>,
    ) -> Result<()> {
        if this.is_empty() {
            return Ok(());
        }
        let ids = this.as_records().ids();
        let targets = this.mapped(tx, CrmTeam::INVOICED_TARGET).await?;
        let companies = this.mapped(tx, CrmTeam::COMPANY_ID).await?;
        let env = this.env().clone();

        // Resolve company currency (or current company's currency for shared teams).
        let fallback_company = env.company_id()?;
        let mut currency_cache: BTreeMap<i64, Option<i64>> = BTreeMap::new();
        let mut team_currencies: Vec<Option<i64>> = Vec::with_capacity(ids.len());
        for comp in &companies {
            let cid = comp.map(RecId::get).or(fallback_company);
            let cur = if let Some(cid) = cid {
                if let Some(&cached) = currency_cache.get(&cid) {
                    cached
                } else {
                    let c = match env
                        .sudo(true)
                        .records("res.company")?
                        .browse([cid])?
                        .cell(tx, "currency_id")
                        .await
                    {
                        Ok(Cell::Id(cur_id)) => Some(cur_id),
                        _ => None,
                    };
                    currency_cache.insert(cid, c);
                    c
                }
            } else {
                None
            };
            team_currencies.push(cur);
        }

        // Aggregate sales orders per team and state bucket.
        let rows = tx
            .query(
                "SELECT team_id::int8, \
                        COUNT(*) FILTER (WHERE state IN ('draft', 'sent'))::int8, \
                        COALESCE(SUM(amount_untaxed) FILTER (WHERE state IN ('draft', 'sent')), 0), \
                        COUNT(*) FILTER (WHERE state = 'sale')::int8, \
                        COALESCE(SUM(amount_untaxed) FILTER (WHERE state = 'sale'), 0), \
                        COUNT(*) FILTER (WHERE state = 'sale' AND invoice_status = 'to invoice')::int8, \
                        COALESCE(SUM(amount_untaxed) FILTER (WHERE state = 'sale' AND invoice_status = 'to invoice'), 0) \
                 FROM sale_order \
                 WHERE team_id = ANY($1) \
                 GROUP BY team_id",
                &[&ids],
            )
            .await?;

        let mut metrics: BTreeMap<i64, (i64, Decimal, i64, Decimal, i64, Decimal)> =
            BTreeMap::new();
        for row in rows {
            let tid: i64 = row.try_get(0).map_err(db)?;
            metrics.insert(
                tid,
                (
                    row.try_get(1).map_err(db)?,
                    row.try_get(2).map_err(db)?,
                    row.try_get(3).map_err(db)?,
                    row.try_get(4).map_err(db)?,
                    row.try_get(5).map_err(db)?,
                    row.try_get(6).map_err(db)?,
                ),
            );
        }

        for (index, ((&tid, &target), &currency)) in
            ids.iter().zip(&targets).zip(&team_currencies).enumerate()
        {
            let (q_cnt, q_amt, s_cnt, s_amt, inv_cnt, inv_amt) = metrics
                .get(&tid)
                .copied()
                .unwrap_or((0, Decimal::ZERO, 0, Decimal::ZERO, 0, Decimal::ZERO));
            let pct = compute_target_achievement(s_amt, target);

            let buttons = vec![json!({
                "label": "New Quotation",
                "action": "sale.action_quotations",
                "view_mode": "form",
                "context": {"default_team_id": tid},
                "primary": true,
            })];
            let stats = vec![
                json!({
                    "label": "Quotations",
                    "count": q_cnt,
                    "amount": q_amt.round_dp(2).to_string(),
                    "currency_id": currency,
                    "action": "sale.action_quotations",
                    "domain": [["team_id", "=", tid], ["state", "in", ["draft", "sent"]]],
                    "context": {"default_team_id": tid},
                    "tone": "neutral",
                }),
                json!({
                    "label": "Sales Orders",
                    "count": s_cnt,
                    "amount": s_amt.round_dp(2).to_string(),
                    "currency_id": currency,
                    "action": "sale.action_orders",
                    "domain": [["team_id", "=", tid], ["state", "=", "sale"]],
                    "context": {"default_team_id": tid},
                    "tone": "neutral",
                }),
                json!({
                    "label": "To Invoice",
                    "count": inv_cnt,
                    "amount": inv_amt.round_dp(2).to_string(),
                    "currency_id": currency,
                    "action": "sale.action_orders",
                    "domain": [["team_id", "=", tid], ["state", "=", "sale"], ["invoice_status", "=", "to invoice"]],
                    "context": {"default_team_id": tid},
                    "tone": if inv_cnt > 0 { "warning" } else { "neutral" },
                }),
            ];
            let chart = if target > Decimal::ZERO || s_amt > Decimal::ZERO {
                json!({
                    "type": "bar",
                    "label": format!("Target ({pct}%)"),
                    "points": [
                        {"label": "Sales", "value": s_amt.round_dp(2).to_string(), "tone": "warning"},
                        {"label": "Target", "value": target.round_dp(2).to_string(), "tone": "neutral"},
                    ],
                })
            } else {
                Json::Null
            };
            let card = json!({
                "v": 1,
                "buttons": buttons,
                "stats": stats,
                "chart": chart,
            });
            out.set(
                index,
                CrmTeamSaleDashboard::SALE_DASHBOARD_CARD,
                Some(Arc::from(card.to_string())),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::compute_target_achievement;
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn target_achievement_is_exact_decimal() {
        assert_eq!(
            compute_target_achievement(
                Decimal::from_str("7500.00").unwrap(),
                Decimal::from_str("10000.00").unwrap(),
            ),
            Decimal::from_str("75.0").unwrap()
        );
        assert_eq!(
            compute_target_achievement(Decimal::from(500), Decimal::ZERO),
            Decimal::ZERO
        );
    }
}
