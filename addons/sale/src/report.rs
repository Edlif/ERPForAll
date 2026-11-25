// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dynamic Sales Report provider (`sale.sales_report`) following the upstream ledger-style report
//! design: Customer -> Sales Orders treegrid, locale dates, currency-symbol monetary cells,
//! exact-decimal margin calculation, caret menus (`partner`, `open`), subtotals, and drill-down.

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

use crate::margin::compute_order_margin_percent;

struct OrderRow {
    order_id: i64,
    name: String,
    partner_id: i64,
    partner_name: String,
    order_date: Date,
    state: String,
    qty: Decimal,
    untaxed: Decimal,
    total: Decimal,
    margin: Decimal,
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Sales report query failed: {err}"))
}

fn state_label(code: &str) -> &'static str {
    match code {
        "draft" => "Quotation",
        "sent" => "Quotation Sent",
        "sale" => "Sales Order",
        "cancel" => "Cancelled",
        _ => "Order",
    }
}

async fn load_sales_rows(
    cx: &mut ReportCx<'_>,
    options: &Options,
) -> Result<(Vec<OrderRow>, Option<i64>)> {
    cx.env()
        .records("sale.order")?
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
            "SELECT o.id::int8, COALESCE(o.name, 'Order')::text, \
                    COALESCE(o.partner_id, 0)::int8, COALESCE(p.name, 'Unknown Customer')::text, \
                    (o.date_order::date), COALESCE(o.state, 'draft')::text, \
                    COALESCE((SELECT SUM(l.product_uom_qty) FROM sale_order_line l \
                              WHERE l.order_id = o.id AND l.display_type IS NULL \
                                AND NOT COALESCE(l.is_downpayment, false)), 0), \
                    COALESCE(o.amount_untaxed, 0), \
                    COALESCE(o.amount_total, 0), \
                    COALESCE((SELECT SUM(COALESCE(l.price_subtotal, 0) - COALESCE(l.purchase_price, 0) * COALESCE(l.product_uom_qty, 0)) \
                              FROM sale_order_line l \
                              WHERE l.order_id = o.id AND l.display_type IS NULL \
                                AND NOT COALESCE(l.is_downpayment, false)), 0) \
             FROM sale_order o \
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
        out.push(OrderRow {
            order_id: row.try_get(0).map_err(db)?,
            name: row.try_get(1).map_err(db)?,
            partner_id: row.try_get(2).map_err(db)?,
            partner_name: row.try_get(3).map_err(db)?,
            order_date: row.try_get(4).map_err(db)?,
            state: row.try_get(5).map_err(db)?,
            qty: row.try_get(6).map_err(db)?,
            untaxed: row.try_get(7).map_err(db)?,
            total: row.try_get(8).map_err(db)?,
            margin: row.try_get(9).map_err(db)?,
        });
    }
    Ok((out, currency))
}

#[allow(clippy::too_many_arguments)]
fn sales_cells(
    order_date: Option<Date>,
    status: Option<&str>,
    qty: Decimal,
    untaxed: Decimal,
    total: Decimal,
    margin: Decimal,
    currency: Option<i64>,
    drillable: bool,
) -> Vec<ReportCell> {
    let margin_pct = compute_order_margin_percent(margin, untaxed);
    vec![
        match order_date {
            Some(d) => ReportCell::date(d),
            None => ReportCell::empty(),
        },
        match status {
            Some(s) => ReportCell::text(s),
            None => ReportCell::empty(),
        },
        ReportCell::number(CellKind::Decimal, Some(qty), 2).with_drillable(drillable),
        ReportCell::monetary(Some(untaxed), 2, currency).with_drillable(drillable),
        ReportCell::monetary(Some(total), 2, currency).with_drillable(drillable),
        ReportCell::monetary(Some(margin), 2, currency).with_drillable(drillable),
        ReportCell::number(CellKind::Percent, Some(margin_pct), 2).with_drillable(false),
    ]
}

async fn render_sales_lines(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
) -> Result<Expansion> {
    let (rows, currency) = load_sales_rows(cx, options).await?;
    let root = LineId::root(report.id);

    let mut by_partner: BTreeMap<(String, i64), Vec<&OrderRow>> = BTreeMap::new();
    for row in &rows {
        by_partner
            .entry((row.partner_name.clone(), row.partner_id))
            .or_default()
            .push(row);
    }

    let mut lines = Vec::new();
    let (mut g_qty, mut g_untaxed, mut g_total, mut g_margin) =
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);

    for ((partner_name, partner_id), partner_rows) in by_partner {
        let (mut p_qty, mut p_untaxed, mut p_total, mut p_margin) =
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
        for r in &partner_rows {
            p_qty += r.qty;
            p_untaxed += r.untaxed;
            p_total += r.total;
            p_margin += r.margin;
        }
        g_qty += p_qty;
        g_untaxed += p_untaxed;
        g_total += p_total;
        g_margin += p_margin;

        let pid_str = partner_id.to_string();
        let line_id = root.child(Segment::group("partner_id", "res.partner", &pid_str));
        let unfolded = options.unfold_all || options.unfolded.contains(&line_id);
        let mut group_line = Line::new(
            line_id.clone(),
            partner_name.clone(),
            LineKind::Group,
            sales_cells(
                None, None, p_qty, p_untaxed, p_total, p_margin, currency, true,
            ),
        );
        group_line.unfoldable = true;
        group_line.unfolded = unfolded;
        group_line.caret = if partner_id > 0 {
            vec!["open", "partner"]
        } else {
            vec!["open"]
        };
        lines.push(group_line);

        if unfolded {
            for r in &partner_rows {
                let detail_id = line_id.child(Segment::group(
                    "order_id",
                    "sale.order",
                    &r.order_id.to_string(),
                ));
                let mut detail = Line::new(
                    detail_id,
                    r.name.clone(),
                    LineKind::Detail,
                    sales_cells(
                        Some(r.order_date),
                        Some(state_label(&r.state)),
                        r.qty,
                        r.untaxed,
                        r.total,
                        r.margin,
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
        "Total Sales",
        LineKind::Total,
        sales_cells(
            None, None, g_qty, g_untaxed, g_total, g_margin, currency, false,
        ),
    ));

    Ok(Expansion::new(lines))
}

async fn expand_customer(
    cx: &mut ReportCx<'_>,
    options: &Options,
    line: &LineId,
) -> Result<Expansion> {
    let Some(last) = line.segments().last() else {
        return Ok(Expansion::new(Vec::new()));
    };
    let partner_id: i64 = last.value().parse().unwrap_or(0);
    let (rows, currency) = load_sales_rows(cx, options).await?;
    let mut children = Vec::new();
    for r in rows.into_iter().filter(|r| r.partner_id == partner_id) {
        let detail_id = line.child(Segment::group(
            "order_id",
            "sale.order",
            &r.order_id.to_string(),
        ));
        let mut detail = Line::new(
            detail_id,
            r.name,
            LineKind::Detail,
            sales_cells(
                Some(r.order_date),
                Some(state_label(&r.state)),
                r.qty,
                r.untaxed,
                r.total,
                r.margin,
                currency,
                true,
            ),
        );
        detail.caret = vec!["open"];
        children.push(detail);
    }
    Ok(Expansion::new(children))
}

fn drill_sales(line: &LineId, target: &DrillTarget) -> Result<Json> {
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
            "name": "Customer",
            "res_model": "res.partner",
            "res_id": pid,
            "target": "current",
            "views": [[false, "form"]],
        }));
    }
    if let Some(oid) = order_id {
        return Ok(json!({
            "type": "ir.actions.act_window",
            "name": "Sales Order",
            "res_model": "sale.order",
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
        "name": "Sales Orders",
        "res_model": "sale.order",
        "target": "current",
        "views": [[false, "list"], [false, "form"]],
        "domain": domain,
    }))
}

pub(crate) struct SalesReport;
const SPEC: ProviderSpec = ProviderSpec::new("sale.sales_report", 1);

impl ReportProvider for SalesReport {
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
        Box::pin(async move { render_sales_lines(cx, report, options).await })
    }
    fn expand<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        _page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move { expand_customer(cx, options, line).await })
    }
    fn drill<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        _options: &'a Options,
        line: &'a LineId,
        target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(async move { drill_sales(line, target) })
    }
}

erp_addon_reporting::provider_declarations!(SalesReport);

#[cfg(test)]
mod tests {
    use super::sales_cells;
    use erp_sdk::{Date, Decimal};
    use std::str::FromStr;

    #[test]
    fn sales_report_cells_preserve_date_and_exact_monetary_scale() {
        let cells = sales_cells(
            Some(Date::from_ymd(2026, 11, 25).unwrap()),
            Some("Sales Order"),
            Decimal::from(4),
            Decimal::from_str("200.00").unwrap(),
            Decimal::from_str("220.00").unwrap(),
            Decimal::from_str("50.00").unwrap(),
            Some(1),
            true,
        );
        assert_eq!(cells.len(), 7);
        assert_eq!(cells[0].text_value(), Some("2026-11-25"));
        assert_eq!(cells[1].text_value(), Some("Sales Order"));
        assert_eq!(cells[6].value(), Some(Decimal::from_str("25.00").unwrap()));
    }
}
