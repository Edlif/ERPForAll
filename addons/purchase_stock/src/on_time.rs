// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vendor On-Time Delivery Rate (`on_time_rate` and `purchase_receipt_done_count` on
//! `res.partner`), computed from completed purchase receipts (`purchase.stock.receipt.link`).

use std::collections::BTreeMap;

use erp_addon_base::ResPartner;
use erp_sdk::prelude::*;

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Vendor on-time delivery query failed: {err}"))
}

/// Pure exact-decimal on-time delivery percentage (`0.0`..=`100.0`, rounded to 1 decimal place).
pub fn compute_on_time_rate(on_time_count: i64, total_done_count: i64) -> Decimal {
    if total_done_count <= 0 {
        return Decimal::ZERO;
    }
    let clamped = on_time_count.clamp(0, total_done_count);
    ((Decimal::from(clamped) * Decimal::from(100)) / Decimal::from(total_done_count)).round_dp(1)
}

/// Extension of `res.partner` exposing vendor delivery reliability metrics.
#[extend(ResPartner, addon = "purchase_stock")]
pub struct ResPartnerPurchaseOnTime {
    /// Percentage of completed purchase receipts delivered on or before the scheduled date.
    #[field(
        float,
        digits = (16, 1),
        compute = "_compute_on_time_rate",
        groups = "purchase.group_purchase_user",
        string = "On-Time Delivery (%)"
    )]
    pub on_time_rate: Decimal,
    /// Total number of completed purchase receipts from this vendor.
    #[field(
        integer,
        compute = "_compute_on_time_rate",
        groups = "purchase.group_purchase_user",
        string = "Completed Receipts"
    )]
    pub purchase_receipt_done_count: i32,
}

#[methods(ResPartner, addon = "purchase_stock")]
impl ResPartnerPurchaseOnTime {
    #[compute(depends = [])]
    async fn _compute_on_time_rate(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ResPartner>,
    ) -> Result<()> {
        let rec_ids = this.as_records().rec_ids();
        let real_ids: Vec<i64> = rec_ids
            .iter()
            .filter_map(|r| r.as_real().or(r.origin()).map(|id| id.get()))
            .collect();
        let mut stats: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
        if !real_ids.is_empty() {
            let rows = tx
                .query(
                    "SELECT o.partner_id::int8, \
                            COUNT(*)::int8, \
                            COUNT(*) FILTER (WHERE p.date_done <= COALESCE(p.scheduled_date, p.date_done))::int8 \
                     FROM purchase_stock_receipt_link l \
                     JOIN purchase_order o ON o.id = l.order_id \
                     JOIN stock_picking p ON p.id = l.picking_id \
                     WHERE o.partner_id = ANY($1) AND p.state = 'done' \
                     GROUP BY o.partner_id",
                    &[&real_ids],
                )
                .await?;
            for row in rows {
                let pid: i64 = row.try_get(0).map_err(db)?;
                let total: i64 = row.try_get(1).map_err(db)?;
                let on_time: i64 = row.try_get(2).map_err(db)?;
                stats.insert(pid, (on_time, total));
            }
        }
        for (i, rec_id) in rec_ids.iter().enumerate() {
            let (on_time, total) = rec_id
                .as_real()
                .or(rec_id.origin())
                .and_then(|id| stats.get(&id.get()).copied())
                .unwrap_or((0, 0));
            let rate = compute_on_time_rate(on_time, total);
            let done_i32 = i32::try_from(total).unwrap_or(i32::MAX);
            out.set(i, ResPartnerPurchaseOnTime::ON_TIME_RATE, rate)?;
            out.set(
                i,
                ResPartnerPurchaseOnTime::PURCHASE_RECEIPT_DONE_COUNT,
                done_i32,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::compute_on_time_rate;
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn vendor_on_time_rate_is_exact_decimal() {
        assert_eq!(
            compute_on_time_rate(3, 4),
            Decimal::from_str("75.0").unwrap()
        );
        assert_eq!(
            compute_on_time_rate(2, 3),
            Decimal::from_str("66.7").unwrap()
        );
        assert_eq!(compute_on_time_rate(0, 0), Decimal::ZERO);
    }
}
