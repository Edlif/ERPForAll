// SPDX-License-Identifier: MIT OR Apache-2.0
//! Resolve only the warehouse's own single-step make-to-stock delivery route.

use erp_sdk::Env;
use erp_sdk::prelude::*;

use super::delivery::{authorize, decode};

pub(super) struct Route {
    pub(super) warehouse: i64,
    pub(super) operation: i64,
    pub(super) rule: i64,
    pub(super) source: i64,
    pub(super) destination: i64,
}

pub(super) async fn resolve(
    tx: &mut Transaction,
    scope: &Env,
    company: i64,
    configured: Option<i64>,
    shipping: i64,
) -> Result<Route> {
    let route_error = || {
        Error::user(
            "This sales order needs an active one-step warehouse with one outgoing make-to-stock delivery rule. Other routes require a later release.",
        )
    };
    let warehouse = match configured {
        Some(id) => id,
        None => {
            let candidates = tx
                .query(
                    "SELECT id FROM stock_warehouse WHERE company_id = $1 AND active \
                       AND delivery_route_id IS NOT NULL \
                     ORDER BY id LIMIT 2",
                    &[&company],
                )
                .await?;
            let [candidate] = candidates.as_slice() else {
                return Err(Error::user(
                    "Choose the sales order's warehouse when its company does not have exactly one configured delivery warehouse.",
                ));
            };
            candidate.try_get(0).map_err(decode)?
        }
    };
    authorize(
        tx,
        scope,
        "stock.warehouse",
        [warehouse],
        &[
            "company_id",
            "active",
            "delivery_steps",
            "lot_stock_id",
            "out_type_id",
            "delivery_route_id",
        ],
    )
    .await?;
    let rows = tx
        .query(
            "SELECT w.delivery_route_id, w.out_type_id, w.lot_stock_id \
             FROM stock_warehouse w WHERE w.id = $1 AND w.company_id = $2 \
               AND w.active AND w.delivery_steps = 'ship_only'",
            &[&warehouse, &company],
        )
        .await?;
    let [row] = rows.as_slice() else {
        return Err(route_error());
    };
    let route: Option<i64> = row.try_get(0).map_err(decode)?;
    let operation: Option<i64> = row.try_get(1).map_err(decode)?;
    let source: i64 = row.try_get(2).map_err(decode)?;
    let (Some(route), Some(operation)) = (route, operation) else {
        return Err(route_error());
    };
    authorize(
        tx,
        scope,
        "stock.route",
        [route],
        &[
            "company_id",
            "active",
            "warehouse_ids",
            "warehouse_selectable",
        ],
    )
    .await?;
    authorize(
        tx,
        scope,
        "stock.picking.type",
        [operation],
        &[
            "company_id",
            "active",
            "code",
            "warehouse_id",
            "default_location_src_id",
            "default_location_dest_id",
        ],
    )
    .await?;
    let rules = tx
        .query(
            "SELECT id FROM stock_rule WHERE route_id = $1 AND active ORDER BY id LIMIT 2",
            &[&route],
        )
        .await?;
    let [rule] = rules.as_slice() else {
        return Err(route_error());
    };
    let rule: i64 = rule.try_get(0).map_err(decode)?;
    authorize(
        tx,
        scope,
        "stock.rule",
        [rule],
        &[
            "company_id",
            "active",
            "route_id",
            "warehouse_id",
            "picking_type_id",
            "action",
            "procure_method",
            "location_src_id",
            "location_dest_id",
            "location_dest_from_rule",
        ],
    )
    .await?;
    let rows = tx
        .query(
            "SELECT r.location_dest_id, r.location_dest_from_rule \
             FROM stock_rule r JOIN stock_route route ON route.id = r.route_id \
             JOIN stock_picking_type t ON t.id = r.picking_type_id \
             WHERE r.id = $1 AND r.route_id = $2 AND r.warehouse_id = $3 \
               AND r.picking_type_id = $4 AND r.company_id = $5 AND r.location_src_id = $6 \
               AND r.active AND r.action = 'pull' AND r.procure_method = 'make_to_stock' \
               AND route.active AND route.company_id = $5 AND route.warehouse_selectable \
               AND EXISTS (SELECT 1 FROM stock_route_warehouse rw \
                           WHERE rw.route_id = route.id AND rw.warehouse_id = $3) \
               AND t.active AND t.code = 'outgoing' AND t.company_id = $5 AND t.warehouse_id = $3 \
               AND t.default_location_src_id = $6 AND t.default_location_dest_id = r.location_dest_id",
            &[&rule, &route, &warehouse, &operation, &company, &source],
        )
        .await?;
    let [row] = rows.as_slice() else {
        return Err(route_error());
    };
    let default_destination: i64 = row.try_get(0).map_err(decode)?;
    let force_destination: bool = row.try_get(1).map_err(decode)?;
    let shipping = scope.records("res.partner")?.browse([shipping])?;
    let destination = match shipping.cell(tx, "property_stock_customer").await? {
        Cell::Id(id) if !force_destination || id == default_destination => id,
        Cell::Id(_) => return Err(route_error()),
        Cell::Null | Cell::Bool(false) => default_destination,
        _ => {
            return Err(Error::invariant(
                "The shipping contact's stock location is not an identity.",
            ));
        }
    };

    authorize(
        tx,
        scope,
        "stock.location",
        [source, default_destination, destination],
        &["company_id", "usage", "active"],
    )
    .await?;
    let valid = tx
        .query(
            "SELECT 1 FROM stock_location source \
             WHERE source.id = $1 AND source.active AND source.usage = 'internal' \
               AND source.company_id = $2 AND ( \
                 SELECT count(*) FROM stock_location d WHERE d.id IN ($3, $4) \
                   AND d.active AND d.usage = 'customer' \
                   AND (d.company_id IS NULL OR d.company_id = $2)) = \
                   CASE WHEN $3 = $4 THEN 1 ELSE 2 END",
            &[&source, &company, &default_destination, &destination],
        )
        .await?;
    if valid.is_empty() {
        return Err(Error::user(
            "A direct sales delivery needs an internal source and a customer destination for its company.",
        ));
    }

    Ok(Route {
        warehouse,
        operation,
        rule,
        source,
        destination,
    })
}
