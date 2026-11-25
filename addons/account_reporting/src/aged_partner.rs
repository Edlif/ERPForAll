// SPDX-License-Identifier: MIT OR Apache-2.0
//! Aged Receivable & Aged Payable report providers (`account_reporting.aged_receivable`,
//! `account_reporting.aged_payable`) with exact-decimal 6-bucket aging and drill-down.

use std::collections::BTreeMap;

use erp_addon_reporting::{
    definition::ReportDef,
    line::{
        Cell as ReportCell, DrillTarget, ExpandPage, Expansion, Line, LineId, LineKind, Segment,
        SegmentKind,
    },
    options::Options,
    provider::{ProviderSpec, ReportCx, ReportProvider},
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, Operation};

/// Exact-decimal 6-bucket aging + total (`[not_due, d1_30, d31_60, d61_90, d91_120, older, total]`).
pub(crate) fn aging_buckets(items: &[(Date, Decimal)], as_of: Date) -> [Decimal; 7] {
    let mut b = [Decimal::ZERO; 7];
    for &(due, amount) in items {
        let days = as_of.days() - due.days();
        let idx = if days <= 0 {
            0
        } else if days <= 30 {
            1
        } else if days <= 60 {
            2
        } else if days <= 90 {
            3
        } else if days <= 120 {
            4
        } else {
            5
        };
        b[idx] += amount;
        b[6] += amount;
    }
    b
}

struct OpenRow {
    move_id: i64,
    move_name: String,
    partner_id: i64,
    partner_name: String,
    due_date: Date,
    amount: Decimal,
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Aged report query failed: {err}"))
}

async fn load_open_rows(
    cx: &mut ReportCx<'_>,
    options: &Options,
    account_type: &str,
    sign: Decimal,
) -> Result<(Vec<OpenRow>, Option<i64>)> {
    cx.env()
        .records("account.move.line")?
        .check_access(cx.tx(), Operation::Read)
        .await?;
    let companies = &options.companies;
    let as_of = options.period.to;
    let cur_rows = cx
        .tx()
        .query(
            "SELECT currency_id::int8 FROM res_company WHERE id = ANY($1::int8[]) ORDER BY id LIMIT 1",
            &[companies],
        )
        .await?;
    let currency = cur_rows.first().and_then(|r| r.try_get::<_, i64>(0).ok());

    let rows = cx
        .tx()
        .query(
            "SELECT m.id::int8, COALESCE(m.name, 'Entry')::text, \
                    COALESCE(l.partner_id, m.partner_id, 0)::int8, \
                    COALESCE(p.name, 'Unknown Partner')::text, \
                    COALESCE(l.date_maturity, m.invoice_date_due, l.date), \
                    COALESCE(o.amount_residual, l.balance) \
             FROM account_move_line l \
             JOIN account_move m ON m.id = l.move_id \
             JOIN account_account a ON a.id = l.account_id \
             LEFT JOIN account_open_item o ON o.line_id = l.id \
             LEFT JOIN res_partner p ON p.id = COALESCE(l.partner_id, m.partner_id) \
             WHERE l.company_id = ANY($1::int8[]) \
               AND l.parent_state = 'posted' \
               AND l.date <= $2 \
               AND a.account_type = $3 \
               AND l.full_reconcile_id IS NULL \
               AND COALESCE(o.amount_residual, l.balance) <> 0 \
             ORDER BY COALESCE(p.name, ''), COALESCE(l.date_maturity, l.date), m.id",
            &[companies, &as_of, &account_type],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let raw_amount: Decimal = row.try_get(5).map_err(db)?;
        let signed = raw_amount * sign;
        if signed.is_zero() {
            continue;
        }
        out.push(OpenRow {
            move_id: row.try_get(0).map_err(db)?,
            move_name: row.try_get(1).map_err(db)?,
            partner_id: row.try_get(2).map_err(db)?,
            partner_name: row.try_get(3).map_err(db)?,
            due_date: row.try_get(4).map_err(db)?,
            amount: signed,
        });
    }
    Ok((out, currency))
}

fn bucket_cells(buckets: [Decimal; 7], currency: Option<i64>, drillable: bool) -> Vec<ReportCell> {
    buckets
        .into_iter()
        .map(|amt| ReportCell::monetary(Some(amt), 2, currency).with_drillable(drillable))
        .collect()
}

async fn render_lines(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
    account_type: &str,
    sign: Decimal,
    total_label: &str,
) -> Result<Expansion> {
    let as_of = options.period.to;
    let (rows, currency) = load_open_rows(cx, options, account_type, sign).await?;
    let root = LineId::root(report.id).child(Segment::line("AGED"));

    let mut by_partner: BTreeMap<(String, i64), Vec<&OpenRow>> = BTreeMap::new();
    for row in &rows {
        by_partner
            .entry((row.partner_name.clone(), row.partner_id))
            .or_default()
            .push(row);
    }

    let mut lines = Vec::new();
    let mut grand_items: Vec<(Date, Decimal)> = Vec::new();

    for ((partner_name, partner_id), partner_rows) in by_partner {
        let pairs: Vec<(Date, Decimal)> = partner_rows
            .iter()
            .map(|r| (r.due_date, r.amount))
            .collect();
        grand_items.extend_from_slice(&pairs);
        let buckets = aging_buckets(&pairs, as_of);
        let pid_str = partner_id.to_string();
        let line_id = root.child(Segment::group("partner_id", "res.partner", &pid_str));
        let unfolded = options.unfold_all || options.unfolded.contains(&line_id);
        let mut line = Line::new(
            line_id.clone(),
            partner_name,
            LineKind::Group,
            bucket_cells(buckets, currency, true),
        );
        line.unfoldable = true;
        line.unfolded = unfolded;
        lines.push(line);

        if unfolded {
            for r in partner_rows {
                let d_buckets = aging_buckets(&[(r.due_date, r.amount)], as_of);
                let detail_id = line_id.child(Segment::group(
                    "move_id",
                    "account.move",
                    &r.move_id.to_string(),
                ));
                let detail = Line::new(
                    detail_id,
                    format!("{} (Due {})", r.move_name, r.due_date),
                    LineKind::Detail,
                    bucket_cells(d_buckets, currency, true),
                );
                lines.push(detail);
            }
        }
    }

    let grand = aging_buckets(&grand_items, as_of);
    let total_line = Line::new(
        root.child(Segment::total()),
        total_label,
        LineKind::Total,
        bucket_cells(grand, currency, false),
    );
    lines.push(total_line);

    Ok(Expansion::new(lines))
}

async fn expand_partner(
    cx: &mut ReportCx<'_>,
    options: &Options,
    line: &LineId,
    account_type: &str,
    sign: Decimal,
) -> Result<Expansion> {
    let as_of = options.period.to;
    let Some(last) = line.segments().last() else {
        return Ok(Expansion::new(Vec::new()));
    };
    let partner_id: i64 = last.value().parse().unwrap_or(0);
    let (rows, currency) = load_open_rows(cx, options, account_type, sign).await?;
    let mut children = Vec::new();
    for r in rows.into_iter().filter(|r| r.partner_id == partner_id) {
        let d_buckets = aging_buckets(&[(r.due_date, r.amount)], as_of);
        let detail_id = line.child(Segment::group(
            "move_id",
            "account.move",
            &r.move_id.to_string(),
        ));
        children.push(Line::new(
            detail_id,
            format!("{} (Due {})", r.move_name, r.due_date),
            LineKind::Detail,
            bucket_cells(d_buckets, currency, true),
        ));
    }
    Ok(Expansion::new(children))
}

fn drill_target(line: &LineId, account_type: &str) -> Result<Json> {
    let mut partner_id: Option<i64> = None;
    let mut move_id: Option<i64> = None;
    for seg in line.segments() {
        if let SegmentKind::Group(field) = seg.kind() {
            if field == "partner_id" {
                partner_id = seg.value().parse().ok();
            } else if field == "move_id" {
                move_id = seg.value().parse().ok();
            }
        }
    }
    if let Some(mid) = move_id {
        return Ok(json!({
            "type": "ir.actions.act_window",
            "name": "Journal Entry",
            "res_model": "account.move",
            "res_id": mid,
            "target": "current",
            "views": [[false, "form"]],
        }));
    }
    let mut domain = vec![
        json!(["parent_state", "=", "posted"]),
        json!(["account_id.account_type", "=", account_type]),
        json!(["full_reconcile_id", "=", false]),
    ];
    if let Some(pid) = partner_id
        && pid > 0
    {
        domain.push(json!(["partner_id", "=", pid]));
    }
    Ok(json!({
        "type": "ir.actions.act_window",
        "name": "Open Ledger Items",
        "res_model": "account.move.line",
        "target": "current",
        "views": [[false, "list"], [false, "form"]],
        "domain": domain,
    }))
}

pub(crate) struct AgedReceivable;
const AR_SPEC: ProviderSpec = ProviderSpec::new("account_reporting.aged_receivable", 1);

impl ReportProvider for AgedReceivable {
    fn spec(&self) -> &ProviderSpec {
        &AR_SPEC
    }
    fn init_options<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        _options: &'a mut Options,
        _strict: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn lines<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move {
            render_lines(
                cx,
                report,
                options,
                "asset_receivable",
                Decimal::ONE,
                "Total Aged Receivable",
            )
            .await
        })
    }
    fn expand<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        _page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move {
            expand_partner(cx, options, line, "asset_receivable", Decimal::ONE).await
        })
    }
    fn drill<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        _options: &'a Options,
        line: &'a LineId,
        _target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(async move { drill_target(line, "asset_receivable") })
    }
}

pub(crate) struct AgedPayable;
const AP_SPEC: ProviderSpec = ProviderSpec::new("account_reporting.aged_payable", 1);

impl ReportProvider for AgedPayable {
    fn spec(&self) -> &ProviderSpec {
        &AP_SPEC
    }
    fn init_options<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        _options: &'a mut Options,
        _strict: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn lines<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move {
            render_lines(
                cx,
                report,
                options,
                "liability_payable",
                Decimal::NEGATIVE_ONE,
                "Total Aged Payable",
            )
            .await
        })
    }
    fn expand<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        _page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move {
            expand_partner(
                cx,
                options,
                line,
                "liability_payable",
                Decimal::NEGATIVE_ONE,
            )
            .await
        })
    }
    fn drill<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        _options: &'a Options,
        line: &'a LineId,
        _target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(async move { drill_target(line, "liability_payable") })
    }
}

mod ar_decl {
    use super::AgedReceivable;
    erp_addon_reporting::provider_declarations!(AgedReceivable);
}

mod ap_decl {
    use super::AgedPayable;
    erp_addon_reporting::provider_declarations!(AgedPayable);
}

#[cfg(test)]
mod tests {
    use super::aging_buckets;
    use erp_sdk::{Date, Decimal};
    use std::str::FromStr;

    #[test]
    fn aging_buckets_classify_exact_decimals_and_conserve_total() {
        let as_of = Date::from_ymd(2026, 11, 25).unwrap();
        let items = vec![
            (
                Date::from_ymd(2026, 12, 10).unwrap(),
                Decimal::from_str("100.10").unwrap(),
            ), // not due
            (
                Date::from_ymd(2026, 11, 10).unwrap(),
                Decimal::from_str("200.20").unwrap(),
            ), // 15d -> 1-30
            (
                Date::from_ymd(2026, 10, 10).unwrap(),
                Decimal::from_str("300.30").unwrap(),
            ), // 46d -> 31-60
            (
                Date::from_ymd(2026, 9, 10).unwrap(),
                Decimal::from_str("400.40").unwrap(),
            ), // 76d -> 61-90
            (
                Date::from_ymd(2026, 8, 10).unwrap(),
                Decimal::from_str("500.50").unwrap(),
            ), // 107d -> 91-120
            (
                Date::from_ymd(2026, 6, 1).unwrap(),
                Decimal::from_str("600.60").unwrap(),
            ), // >120d -> older
        ];
        let b = aging_buckets(&items, as_of);
        assert_eq!(b[0], Decimal::from_str("100.10").unwrap());
        assert_eq!(b[1], Decimal::from_str("200.20").unwrap());
        assert_eq!(b[2], Decimal::from_str("300.30").unwrap());
        assert_eq!(b[3], Decimal::from_str("400.40").unwrap());
        assert_eq!(b[4], Decimal::from_str("500.50").unwrap());
        assert_eq!(b[5], Decimal::from_str("600.60").unwrap());
        assert_eq!(b[6], Decimal::from_str("2102.10").unwrap());
        assert_eq!(b[0] + b[1] + b[2] + b[3] + b[4] + b[5], b[6]);
    }
}
