// SPDX-License-Identifier: MIT OR Apache-2.0
//! The private admission and mutation boundary for a direct sale shipment.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_sale::SaleOrder;
use erp_sdk::prelude::*;
use erp_sdk::{Context, Env, ErrorKind, FieldOp, Operation};

use super::route::{self, Route};

const DEMAND_ACCESS: &str = "The source records for this sales delivery are not accessible.";

#[model(
    name = "sale.stock.delivery.link",
    description = "Direct Sales Delivery Link",
    rec_name = "order_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        ("_order", "UNIQUE (order_id)", "An order has one direct sales delivery."),
        ("_picking", "UNIQUE (picking_id)", "A direct sales delivery belongs to one order.")
    ],
)]
struct SaleDeliveryLink {
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict"
    )]
    company_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "sale.order",
        required = true,
        ondelete = "restrict"
    )]
    order_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "stock.picking",
        required = true,
        ondelete = "restrict"
    )]
    picking_id: Option<RecId>,
}

#[model(
    name = "sale.stock.delivery.move.link",
    description = "Direct Sales Delivery Move Link",
    rec_name = "sale_line_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        ("_move", "UNIQUE (move_id)", "A direct delivery move belongs to one order line."),
        ("_line", "UNIQUE (sale_line_id)", "An order line has one initial direct delivery move.")
    ],
)]
struct SaleDeliveryMoveLink {
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict"
    )]
    company_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "stock.move",
        required = true,
        ondelete = "restrict"
    )]
    move_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "sale.order.line",
        required = true,
        ondelete = "restrict"
    )]
    sale_line_id: Option<RecId>,
}

#[extend(SaleOrder, addon = "sale_stock")]
struct DirectSalesOrder {
    /// Explicit warehouse selection for a quotation. Confirmation can resolve a lone company warehouse.
    #[field(
        many2one,
        comodel = "stock.warehouse",
        ondelete = "restrict",
        check_company = true
    )]
    warehouse_id: Option<RecId>,
}

struct Demand {
    line: i64,
    order: i64,
    company: i64,
    product: i64,
    unit: i64,
    quantity: Decimal,
}

struct Plan {
    order: i64,
    company: i64,
    shipping: i64,
    reference: String,
    scheduled: DateTime,
    route: Route,
    moves: Vec<Demand>,
}

pub(super) fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Reading the saved sales delivery configuration failed.").with_source(error)
}

fn redact(error: Error) -> Error {
    if matches!(error.kind(), ErrorKind::Access | ErrorKind::Missing) {
        Error::access(DEMAND_ACCESS)
    } else {
        error
    }
}

pub(super) async fn authorize(
    tx: &mut Transaction,
    env: &Env,
    model: &str,
    ids: impl IntoIterator<Item = i64>,
    fields: &[&str],
) -> Result<Records> {
    let records = env.records(model)?.browse(ids)?;
    if records.is_empty() {
        return Ok(records);
    }
    records
        .check_access(tx, Operation::Read)
        .await
        .map_err(redact)?;
    for field in fields {
        if !tx
            .has_field_access(env, records.spec().name, field, FieldOp::Read)
            .await?
        {
            return Err(Error::access(DEMAND_ACCESS));
        }
    }
    Ok(records)
}

fn company_scope(env: &Env, company: i64) -> Env {
    // Drop caller-supplied default_* and behavior context keys before invoking Stock.
    env.with_context_replaced(Context::new().with_company_id(company))
}

async fn demand(
    tx: &mut Transaction,
    orders: &Records,
    limit: i64,
) -> Result<BTreeMap<i64, Vec<Demand>>> {
    let ids = orders.ids();
    let memberships = tx
        .query(
            "SELECT id FROM sale_order_line WHERE order_id = ANY($1) ORDER BY id LIMIT $2",
            &[&ids, &(limit + 1)],
        )
        .await?;
    if memberships.len() as i64 > limit {
        return Err(row_limit(limit));
    }
    let lines = memberships
        .iter()
        .map(|row| row.try_get(0).map_err(decode))
        .collect::<Result<Vec<i64>>>()?;
    let env = orders.env();
    let source_lines = authorize(
        tx,
        env,
        "sale.order.line",
        lines.iter().copied(),
        &[
            "order_id",
            "company_id",
            "display_type",
            "is_downpayment",
            "product_id",
            "product_uom_id",
            "product_uom_qty",
        ],
    )
    .await?;
    source_lines.lock_for_update(tx, true).await?;
    let rows = tx
        .query(
            "SELECT l.id, l.order_id, l.company_id, l.product_id, l.product_uom_id, \
                    l.product_uom_qty, l.display_type, l.is_downpayment \
             FROM sale_order_line l WHERE l.id = ANY($1) AND l.order_id = ANY($2) ORDER BY l.id",
            &[&lines, &ids],
        )
        .await?;
    if rows.len() != lines.len() {
        return Err(Error::access(DEMAND_ACCESS));
    }
    let mut items = Vec::new();
    let mut products = BTreeSet::new();
    for row in &rows {
        let display: Option<&str> = row.try_get(6).map_err(decode)?;
        let downpayment: bool = row.try_get(7).map_err(decode)?;
        let product: Option<i64> = row.try_get(3).map_err(decode)?;
        if display.is_none()
            && !downpayment
            && let Some(product) = product
        {
            products.insert(product);
            items.push((product, row));
        }
    }
    let mut result = BTreeMap::<i64, Vec<Demand>>::new();
    if products.is_empty() {
        return Ok(result);
    }
    authorize(
        tx,
        env,
        "product.product",
        products.iter().copied(),
        &["type"],
    )
    .await?;
    let kinds = tx
        .query(
            "SELECT p.id, t.type FROM product_product p JOIN product_template t ON t.id = p.product_tmpl_id \
             WHERE p.id = ANY($1)",
            &[&products.into_iter().collect::<Vec<_>>()],
        )
        .await?
        .iter()
        .map(|row| Ok((row.try_get::<_, i64>(0).map_err(decode)?, row.try_get::<_, String>(1).map_err(decode)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    for (product, row) in items {
        match kinds.get(&product).map(String::as_str) {
            Some("service") => continue,
            Some("consu") => {}
            Some(_) => {
                return Err(Error::user(
                    "Delivery of bundled products requires a later release.",
                ));
            }
            None => return Err(Error::access(DEMAND_ACCESS)),
        }
        let quantity: Decimal = row.try_get(5).map_err(decode)?;
        if quantity == Decimal::ZERO {
            continue;
        }
        if quantity < Decimal::ZERO {
            return Err(Error::user(
                "Negative sales quantities require the returns workflow in a later release.",
            ));
        }
        let unit: Option<i64> = row.try_get(4).map_err(decode)?;
        let demand = Demand {
            line: row.try_get(0).map_err(decode)?,
            order: row.try_get(1).map_err(decode)?,
            company: row.try_get(2).map_err(decode)?,
            product,
            unit: unit.ok_or_else(|| {
                Error::validation("A direct sales delivery needs a unit on each goods line.")
            })?,
            quantity,
        };
        result.entry(demand.order).or_default().push(demand);
    }
    Ok(result)
}

fn row_limit(limit: i64) -> Error {
    Error::user(format!(
        "Sales delivery confirmation exceeds sale_stock.max_confirmation_source_rows ({limit})."
    ))
}

async fn plans(tx: &mut Transaction, orders: &Records) -> Result<Vec<Plan>> {
    let limit = erp_sdk::params::get_int(tx, &super::ADDON, super::ROW_LIMIT).await?;
    let ids = orders.ids();
    if ids.len() as i64 > limit {
        return Err(row_limit(limit));
    }
    let env = orders.env();
    orders.check_access(tx, Operation::Write).await?;
    tx.flush().await?;
    let configured = tx
        .query(
            "SELECT o.id FROM sale_order o WHERE o.id = ANY($1) AND (
             o.warehouse_id IS NOT NULL OR EXISTS (
                 SELECT 1 FROM stock_warehouse w WHERE w.company_id = o.company_id
                   AND w.active AND w.delivery_route_id IS NOT NULL))",
            &[&ids],
        )
        .await?
        .iter()
        .map(|row| row.try_get(0).map_err(decode))
        .collect::<Result<Vec<i64>>>()?;
    if configured.is_empty() {
        return Ok(Vec::new());
    }
    let orders = authorize(
        tx,
        env,
        "sale.order",
        configured,
        &[
            "name",
            "company_id",
            "partner_id",
            "partner_shipping_id",
            "warehouse_id",
            "date_order",
            "commitment_date",
            "state",
        ],
    )
    .await?;
    let mut demand = demand(tx, &orders, limit).await?;
    if demand.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<i64> = demand.keys().copied().collect();
    let rows = tx
        .query(
            "SELECT o.id, o.company_id, o.partner_id, o.partner_shipping_id, o.warehouse_id, \
                    o.name, COALESCE(o.commitment_date, o.date_order), l.picking_id \
             FROM sale_order o LEFT JOIN sale_stock_delivery_link l ON l.order_id = o.id \
             WHERE o.id = ANY($1) AND o.state = 'sale' ORDER BY o.company_id, o.id",
            &[&ids],
        )
        .await?;
    if rows.len() != ids.len() {
        return Err(Error::invariant(
            "Direct sales deliveries require saved confirmed orders.",
        ));
    }
    let mut result = Vec::new();
    for row in rows {
        let existing: Option<i64> = row.try_get(7).map_err(decode)?;
        if existing.is_some() {
            return Err(Error::user(
                "This order already has a direct sales delivery. Replacement requires a later release.",
            ));
        }
        let order: i64 = row.try_get(0).map_err(decode)?;
        let company: i64 = row.try_get(1).map_err(decode)?;
        let customer: i64 = row.try_get(2).map_err(decode)?;
        let shipping: Option<i64> = row.try_get(3).map_err(decode)?;
        let shipping = shipping.ok_or_else(|| {
            Error::validation("A direct sales delivery needs a shipping contact.")
        })?;
        let warehouse: Option<i64> = row.try_get(4).map_err(decode)?;
        let moves = demand
            .remove(&order)
            .ok_or_else(|| Error::invariant("Direct delivery lost its sales lines."))?;
        if moves.iter().any(|line| line.company != company) {
            return Err(Error::access(DEMAND_ACCESS));
        }
        let scope = company_scope(env, company);
        authorize(
            tx,
            &scope,
            "res.partner",
            [customer, shipping],
            &["company_id", "active"],
        )
        .await?;
        authorize(
            tx,
            &scope,
            "res.partner",
            [shipping],
            &["property_stock_customer"],
        )
        .await?;
        let partners = tx
            .query(
                "SELECT 1 FROM res_partner WHERE id = ANY($1) \
                 AND (active IS NOT TRUE OR (company_id IS NOT NULL AND company_id <> $2)) LIMIT 1",
                &[&[customer, shipping].as_slice(), &company],
            )
            .await?;
        if !partners.is_empty() {
            return Err(Error::access(DEMAND_ACCESS));
        }
        let products: BTreeSet<i64> = moves.iter().map(|line| line.product).collect();
        let products = authorize(
            tx,
            &scope,
            "product.product",
            products,
            &["uom_id", "product_tmpl_id", "categ_id"],
        )
        .await?;
        let mut units: BTreeSet<i64> = moves.iter().map(|line| line.unit).collect();
        for unit in products.cells(tx, "uom_id").await.map_err(redact)? {
            let Cell::Id(unit) = unit else {
                return Err(Error::validation(
                    "A direct sales delivery needs an inventory unit for each product.",
                ));
            };
            units.insert(unit);
        }
        // Stock captures both conversions under the private mutation scope. Admit every
        // ancestor and conversion field as the caller before crossing that boundary.
        for unit in units {
            erp_addon_uom::models::load_unit(&scope, tx, unit)
                .await
                .map_err(redact)?;
        }
        let route = route::resolve(tx, &scope, company, warehouse, shipping).await?;
        ensure_warehouse_route(tx, &scope, company, route.warehouse, &products.ids()).await?;
        result.push(Plan {
            order,
            company,
            shipping,
            reference: row.try_get(5).map_err(decode)?,
            scheduled: row.try_get(6).map_err(decode)?,
            route,
            moves,
        });
    }
    Ok(result)
}

async fn ensure_warehouse_route(
    tx: &mut Transaction,
    scope: &Env,
    company: i64,
    warehouse: i64,
    products: &[i64],
) -> Result<()> {
    let rows = tx
        .query(
            "SELECT DISTINCT r.id FROM product_product p JOIN product_template t ON t.id = p.product_tmpl_id \
             JOIN product_category c ON c.id = t.categ_id CROSS JOIN stock_route r \
             WHERE p.id = ANY($1) AND r.active AND (r.company_id IS NULL OR r.company_id = $2) \
               AND (EXISTS (SELECT 1 FROM stock_route_product rp WHERE rp.route_id = r.id AND rp.product_id = t.id) \
                    OR EXISTS (SELECT 1 FROM stock_route_categ rc JOIN product_category ancestor ON ancestor.id = rc.categ_id \
                               WHERE rc.route_id = r.id AND c.parent_path LIKE ancestor.parent_path || '%')) \
               AND NOT EXISTS (SELECT 1 FROM stock_warehouse w WHERE w.id = $3 AND w.delivery_route_id = r.id) \
             ORDER BY r.id LIMIT 1",
            &[&products, &company, &warehouse],
        )
        .await?;
    if let Some(row) = rows.first() {
        let route: i64 = row.try_get(0).map_err(decode)?;
        authorize(tx, scope, "stock.route", [route], &[]).await?;
        return Err(Error::user(
            "An alternate product or category route requires a later release.",
        ));
    }
    Ok(())
}

impl Plan {
    async fn create(self, tx: &mut Transaction, caller: &Env) -> Result<()> {
        // Only this admitted order and its exact moves cross the Stock write boundary.
        // The caller remains the actor for the stock lifecycle and gains no Stock ACL.
        let env = company_scope(caller, self.company)
            .without_default_overrides()
            .sudo(true);
        let common = RawVals::new()
            .set_cell("company_id", Cell::Id(self.company))
            .set_cell("picking_type_id", Cell::Id(self.route.operation))
            .set_cell("location_id", Cell::Id(self.route.source))
            .set_cell("location_dest_id", Cell::Id(self.route.destination))
            .set_cell("origin", Cell::Str(self.reference.into()))
            .set_cell("partner_id", Cell::Id(self.shipping));
        let picking = env
            .records("stock.picking")?
            .create(
                tx,
                vec![
                    common
                        .clone()
                        .set_cell("scheduled_date", Cell::DateTime(self.scheduled)),
                ],
            )
            .await?;
        let picking_ids = picking.ids();
        let [picking_id] = picking_ids.as_slice() else {
            return Err(Error::invariant(
                "Direct delivery did not create exactly one transfer.",
            ));
        };
        let picking_id = *picking_id;
        let lines: Vec<i64> = self.moves.iter().map(|line| line.line).collect();
        let values = self
            .moves
            .into_iter()
            .map(|line| {
                common
                    .clone()
                    .set_cell("picking_id", Cell::Id(picking_id))
                    .set_cell("product_id", Cell::Id(line.product))
                    .set_cell("uom_id", Cell::Id(line.unit))
                    .set_cell("product_uom_qty", Cell::Decimal(line.quantity))
                    .set_cell("date", Cell::DateTime(self.scheduled))
                    .set_cell("warehouse_id", Cell::Id(self.route.warehouse))
                    .set_cell("rule_id", Cell::Id(self.route.rule))
                    .set_cell("procure_method", Cell::Str("make_to_stock".into()))
            })
            .collect();
        let moves = env.records("stock.move")?.create(tx, values).await?;
        let moves = moves.ids();
        if moves.len() != lines.len() {
            return Err(Error::invariant(
                "Direct delivery did not preserve order-line membership.",
            ));
        }
        let () = picking.call(tx, "action_confirm", ()).await?;
        tx.execute(
            "INSERT INTO sale_stock_delivery_link (company_id, order_id, picking_id) VALUES ($1, $2, $3)",
            &[&self.company, &self.order, &picking_id],
        ).await?;
        tx.execute(
            "INSERT INTO sale_stock_delivery_move_link (company_id, move_id, sale_line_id) \
             SELECT $1, move_id, line_id FROM unnest($2::int8[], $3::int8[]) AS link(move_id, line_id)",
            &[&self.company, &moves, &lines],
        ).await?;
        Ok(())
    }
}

#[methods(SaleOrder, addon = "sale_stock")]
impl DirectSalesOrder {
    #[method(override)]
    async fn write(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let orders = this.as_records();
        if vals.contains("warehouse_id") && !orders.is_empty() {
            orders.check_access(tx, Operation::Write).await?;
            orders.lock_for_update(tx, false).await?;
            tx.flush().await?;
            let frozen = tx
                .query(
                    "SELECT 1 FROM sale_order o WHERE o.id = ANY($1) AND (
                         o.state IS NULL OR o.state NOT IN ('draft', 'sent')
                         OR o.locked IS DISTINCT FROM FALSE OR EXISTS (
                             SELECT 1 FROM sale_stock_delivery_link l WHERE l.order_id = o.id
                         )
                     ) LIMIT 1",
                    &[&orders.ids()],
                )
                .await?;
            if !frozen.is_empty() {
                return Err(Error::user(
                    "The sales warehouse can be changed only on an editable quotation without a delivery.",
                ));
            }
        }
        sup.call(tx, orders, (vals,)).await
    }

    #[method(override)]
    async fn _action_confirm(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let orders = this.as_records();
        if orders.is_empty() {
            return sup.call(tx, orders, ()).await;
        }
        for plan in plans(tx, orders).await? {
            plan.create(tx, orders.env()).await?;
        }
        sup.call(tx, orders, ()).await
    }

    #[method(override)]
    async fn _action_cancel(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let orders = this.as_records();
        orders.check_access(tx, Operation::Write).await?;
        tx.flush().await?;
        let open = tx.query(
            "SELECT 1 FROM sale_stock_delivery_link l JOIN stock_picking p ON p.id = l.picking_id
             WHERE l.order_id = ANY($1) AND p.state IS DISTINCT FROM 'cancel' LIMIT 1",
            &[&orders.ids()],
        ).await?;
        if !open.is_empty() {
            return Err(Error::user(
                "Cancel the linked transfer in Inventory before cancelling its sales order. Completed deliveries require the a later release returns workflow.",
            ));
        }
        sup.call(tx, orders, ()).await
    }
}
