// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dynamic Purchase Report provider (`purchase.purchase_report`) following the upstream
//! ledger-style report design: Vendor -> Purchase Orders treegrid, locale dates with danger tone on
//! overdue expected arrivals, currency-symbol monetary cells, caret menus (`partner`, `open`),
//! subtotals, and drill-down.

use std::collections::BTreeMap;

use erp_addon_reporting::{
    definition::ReportDef,
    line::{
        Cell as ReportCell, CellKind, DrillTarget, ExpandPage, Expansion, Line, LineId, LineKind,
        Segment, SegmentKind,
    },
    options::Options,
    provider::{ProviderSpec, ReportCx, ReportProvider},
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, Operation};

struct PurchaseRow {
    order_id: i64,
    name: String,
    partner_id: i64,
    partner_name: String,
    order_date: Date,
    planned_date: Option<Date>,
    ordered: Decimal,
    received: Decimal,
    billed: Decimal,
    untaxed: Decimal,
    total: Decimal,
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Purchase report query failed: {err}"))
}

async fn load_purchase_rows(
    cx: &mut ReportCx<'_>,
    options: &Options,
) -> Result<(Vec<PurchaseRow>, Option<i64>)> {
    cx.env()
        .records("purchase.order")?
        .check_access(cx.tx(), Operation::Read)
        .await?;
    let companies = &options.companies;
    let cur_rows = cx
        .tx()
        .query(
            "SELECT currency_id::int8 FROM res_company WHERE id = ANY($1::int8[]) ORDER BY id LIMIT 1",
            &[companies],
        )
        .await?;
    let currency = cur_rows.first().and_then(|r| r.try_get::<_, i64>(0).ok());

    let date_from = options.period.from.unwrap_or(options.period.to);
    let date_to = options.period.to;
    let rows = cx
        .tx()
        .query(
            "SELECT o.id::int8, COALESCE(o.name, 'PO')::text, \
                    COALESCE(o.partner_id, 0)::int8, COALESCE(p.name, 'Unknown Vendor')::text, \
                    (o.date_order::date), (o.date_planned::date), \
                    COALESCE((SELECT SUM(l.product_qty) FROM purchase_order_line l WHERE l.order_id = o.id), 0), \
                    COALESCE((SELECT SUM(l.qty_received) FROM purchase_order_line l WHERE l.order_id = o.id), 0), \
                    COALESCE((SELECT SUM(l.qty_invoiced) FROM purchase_order_line l WHERE l.order_id = o.id), 0), \
                    COALESCE(o.amount_untaxed, 0), \
                    COALESCE(o.amount_total, 0) \
             FROM purchase_order o \
             LEFT JOIN res_partner p ON p.id = o.partner_id \
             WHERE o.company_id = ANY($1::int8[]) \
               AND o.state != 'cancel' \
               AND (o.date_order::date) >= $2 \
               AND (o.date_order::date) <= $3 \
             ORDER BY COALESCE(p.name, ''), o.date_order DESC, o.id DESC",
            &[companies, &date_from, &date_to],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(PurchaseRow {
            order_id: row.try_get(0).map_err(db)?,
            name: row.try_get(1).map_err(db)?,
            partner_id: row.try_get(2).map_err(db)?,
            partner_name: row.try_get(3).map_err(db)?,
            order_date: row.try_get(4).map_err(db)?,
            planned_date: row.try_get(5).map_err(db)?,
            ordered: row.try_get(6).map_err(db)?,
            received: row.try_get(7).map_err(db)?,
            billed: row.try_get(8).map_err(db)?,
            untaxed: row.try_get(9).map_err(db)?,
            total: row.try_get(10).map_err(db)?,
        });
    }
    Ok((out, currency))
}

#[allow(clippy::too_many_arguments)]
fn purchase_cells(
    order_date: Option<Date>,
    planned_date: Option<Date>,
    today: Date,
    ordered: Decimal,
    received: Decimal,
    billed: Decimal,
    untaxed: Decimal,
    total: Decimal,
    currency: Option<i64>,
    drillable: bool,
) -> Vec<ReportCell> {
    let overdue = planned_date.is_some_and(|d| d < today && received < ordered);
    vec![
        match order_date {
            Some(d) => ReportCell::date(d),
            None => ReportCell::empty(),
        },
        match planned_date {
            Some(d) => ReportCell::date(d).with_danger(overdue),
            None => ReportCell::empty(),
        },
        ReportCell::number(CellKind::Decimal, Some(ordered), 2).with_drillable(drillable),
        ReportCell::number(CellKind::Decimal, Some(received), 2).with_drillable(drillable),
        ReportCell::number(CellKind::Decimal, Some(billed), 2).with_drillable(drillable),
        ReportCell::monetary(Some(untaxed), 2, currency).with_drillable(drillable),
        ReportCell::monetary(Some(total), 2, currency).with_drillable(drillable),
    ]
}

async fn render_purchase_lines(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
) -> Result<Expansion> {
    let today = Date::today();
    let (rows, currency) = load_purchase_rows(cx, options).await?;
    let root = LineId::root(report.id);

    let mut by_vendor: BTreeMap<(String, i64), Vec<&PurchaseRow>> = BTreeMap::new();
    for row in &rows {
        by_vendor
            .entry((row.partner_name.clone(), row.partner_id))
            .or_default()
            .push(row);
    }

    let mut lines = Vec::new();
    let (mut g_ord, mut g_rec, mut g_bil, mut g_unt, mut g_tot) = (
        Decimal::ZERO,
        Decimal::ZERO,
        Decimal::ZERO,
        Decimal::ZERO,
        Decimal::ZERO,
    );

    for ((vendor_name, vendor_id), vendor_rows) in by_vendor {
        let (mut v_ord, mut v_rec, mut v_bil, mut v_unt, mut v_tot) = (
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
        );
        for r in &vendor_rows {
            v_ord += r.ordered;
            v_rec += r.received;
            v_bil += r.billed;
            v_unt += r.untaxed;
            v_tot += r.total;
        }
        g_ord += v_ord;
        g_rec += v_rec;
        g_bil += v_bil;
        g_unt += v_unt;
        g_tot += v_tot;

        let vid_str = vendor_id.to_string();
        let line_id = root.child(Segment::group("partner_id", "res.partner", &vid_str));
        let unfolded = options.unfold_all || options.unfolded.contains(&line_id);
        let mut group_line = Line::new(
            line_id.clone(),
            vendor_name.clone(),
            LineKind::Group,
            purchase_cells(
                None, None, today, v_ord, v_rec, v_bil, v_unt, v_tot, currency, true,
            ),
        );
        group_line.unfoldable = true;
        group_line.unfolded = unfolded;
        group_line.caret = if vendor_id > 0 {
            vec!["open", "partner"]
        } else {
            vec!["open"]
        };
        lines.push(group_line);

        if unfolded {
            for r in &vendor_rows {
                let detail_id = line_id.child(Segment::group(
                    "order_id",
                    "purchase.order",
                    &r.order_id.to_string(),
                ));
                let mut detail = Line::new(
                    detail_id,
                    r.name.clone(),
                    LineKind::Detail,
                    purchase_cells(
                        Some(r.order_date),
                        r.planned_date,
                        today,
                        r.ordered,
                        r.received,
                        r.billed,
                        r.untaxed,
                        r.total,
                        currency,
                        true,
                    ),
                );
                detail.caret = vec!["open"];
                lines.push(detail);
            }
        }
    }

    lines.push(Line::new(
        root.child(Segment::total()),
        "Total Purchases",
        LineKind::Total,
        purchase_cells(
            None, None, today, g_ord, g_rec, g_bil, g_unt, g_tot, currency, false,
        ),
    ));

    Ok(Expansion::new(lines))
}

async fn expand_vendor(
    cx: &mut ReportCx<'_>,
    options: &Options,
    line: &LineId,
) -> Result<Expansion> {
    let today = Date::today();
    let Some(last) = line.segments().last() else {
        return Ok(Expansion::new(Vec::new()));
    };
    let vendor_id: i64 = last.value().parse().unwrap_or(0);
    let (rows, currency) = load_purchase_rows(cx, options).await?;
    let mut children = Vec::new();
    for r in rows.into_iter().filter(|r| r.partner_id == vendor_id) {
        let detail_id = line.child(Segment::group(
            "order_id",
            "purchase.order",
            &r.order_id.to_string(),
        ));
        let mut detail = Line::new(
            detail_id,
            r.name,
            LineKind::Detail,
            purchase_cells(
                Some(r.order_date),
                r.planned_date,
                today,
                r.ordered,
                r.received,
                r.billed,
                r.untaxed,
                r.total,
                currency,
                true,
            ),
        );
        detail.caret = vec!["open"];
        children.push(detail);
    }
    Ok(Expansion::new(children))
}

fn drill_purchase(line: &LineId, target: &DrillTarget) -> Result<Json> {
    let mut partner_id: Option<i64> = None;
    let mut order_id: Option<i64> = None;
    for seg in line.segments() {
        if let SegmentKind::Group(field) = seg.kind() {
            if field == "partner_id" {
                partner_id = seg.value().parse().ok();
            } else if field == "order_id" {
                order_id = seg.value().parse().ok();
            }
        }
    }
    if matches!(target, DrillTarget::Item(name) if name == "partner")
        && let Some(pid) = partner_id
        && pid > 0
    {
        return Ok(json!({
            "type": "ir.actions.act_window",
            "name": "Vendor",
            "res_model": "res.partner",
            "res_id": pid,
            "target": "current",
            "views": [[false, "form"]],
        }));
    }
    if let Some(oid) = order_id {
        return Ok(json!({
            "type": "ir.actions.act_window",
            "name": "Purchase Order",
            "res_model": "purchase.order",
            "res_id": oid,
            "target": "current",
            "views": [[false, "form"]],
        }));
    }
    let mut domain = vec![json!(["state", "!=", "cancel"])];
    if let Some(pid) = partner_id
        && pid > 0
    {
        domain.push(json!(["partner_id", "=", pid]));
    }
    Ok(json!({
        "type": "ir.actions.act_window",
        "name": "Purchase Orders",
        "res_model": "purchase.order",
        "target": "current",
        "views": [[false, "list"], [false, "form"]],
        "domain": domain,
    }))
}

pub(crate) struct PurchaseReport;
const SPEC: ProviderSpec = ProviderSpec::new("purchase.purchase_report", 1);

impl ReportProvider for PurchaseReport {
    fn spec(&self) -> &ProviderSpec {
        &SPEC
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
        Box::pin(async move { render_purchase_lines(cx, report, options).await })
    }
    fn expand<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        _page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move { expand_vendor(cx, options, line).await })
    }
    fn drill<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        _options: &'a Options,
        line: &'a LineId,
        target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(async move { drill_purchase(line, target) })
    }
}

erp_addon_reporting::provider_declarations!(PurchaseReport);

#[cfg(test)]
mod tests {
    use super::purchase_cells;
    use erp_sdk::{Date, Decimal};
    use std::str::FromStr;

    #[test]
    fn purchase_report_marks_overdue_unreceived_arrival_as_danger() {
        let today = Date::from_ymd(2026, 11, 26).unwrap();
        let overdue = Date::from_ymd(2026, 11, 20).unwrap();
        let cells = purchase_cells(
            Some(Date::from_ymd(2026, 11, 15).unwrap()),
            Some(overdue),
            today,
            Decimal::from(10),
            Decimal::from(4),
            Decimal::ZERO,
            Decimal::from_str("100.00").unwrap(),
            Decimal::from_str("110.00").unwrap(),
            Some(1),
            true,
        );
        assert_eq!(cells.len(), 7);
        assert_eq!(cells[0].text_value(), Some("2026-11-15"));
        assert_eq!(cells[1].text_value(), Some("2026-11-20"));
    }
}
