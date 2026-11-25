// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cross-document smart buttons owned by `stock`:
//! - `stock.picking`: Returns (`action_view_returns`), Backorders (`backorder_count` / `action_view_backorders`)
//! - `product.template`: On Hand (`qty_on_hand` / `action_open_quants`), Stock Moves (`stock_move_count` / `action_open_stock_moves`), Reordering Rules (`reordering_rule_count` / `action_open_orderpoints`)
//! - `stock.warehouse`: Operation Types (`picking_type_count` / `action_view_picking_types`), Transfers (`picking_count` / `action_view_pickings`)
//! - `stock.location`: Current Stock (`quant_count` / `action_view_quants`), Transfers (`picking_count` / `action_view_pickings`)
//! - `res.partner`: Transfers (`picking_count` / `action_view_pickings`)

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use erp_addon_base::ResPartner;
use erp_addon_product::ProductTemplate;
use erp_sdk::prelude::*;
use erp_sdk::{Operation, ValueMap};

use crate::location::StockLocation;
use crate::picking::StockPicking;
use crate::warehouse::StockWarehouse;

const PICKING_RETURNS: &str =
    "SELECT return_id::int8, id::int8 FROM stock_picking WHERE return_id = ANY($1)";
const PICKING_BACKORDERS: &str =
    "SELECT backorder_id::int8, id::int8 FROM stock_picking WHERE backorder_id = ANY($1)";

const PRODUCT_INTERNAL_QUANTS: &str = "SELECT p.product_tmpl_id::int8, q.id::int8 \
    FROM stock_quant q \
    JOIN product_product p ON p.id = q.product_id \
    JOIN stock_location l ON l.id = q.location_id \
    WHERE p.product_tmpl_id = ANY($1) AND l.usage = 'internal'";

const PRODUCT_MOVES: &str = "SELECT p.product_tmpl_id::int8, m.id::int8 \
    FROM stock_move m \
    JOIN product_product p ON p.id = m.product_id \
    WHERE p.product_tmpl_id = ANY($1)";

const PRODUCT_ORDERPOINTS: &str = "SELECT p.product_tmpl_id::int8, op.id::int8 \
    FROM stock_warehouse_orderpoint op \
    JOIN product_product p ON p.id = op.product_id \
    WHERE p.product_tmpl_id = ANY($1)";

const WAREHOUSE_PICKING_TYPES: &str =
    "SELECT warehouse_id::int8, id::int8 FROM stock_picking_type WHERE warehouse_id = ANY($1)";

const WAREHOUSE_PICKINGS: &str = "SELECT pt.warehouse_id::int8, sp.id::int8 \
    FROM stock_picking sp \
    JOIN stock_picking_type pt ON pt.id = sp.picking_type_id \
    WHERE pt.warehouse_id = ANY($1)";

const LOCATION_QUANTS: &str =
    "SELECT location_id::int8, id::int8 FROM stock_quant WHERE location_id = ANY($1)";

const LOCATION_PICKINGS: &str = "SELECT loc_id::int8, id::int8 FROM ( \
    SELECT location_id AS loc_id, id FROM stock_picking WHERE location_id = ANY($1) \
    UNION \
    SELECT location_dest_id AS loc_id, id FROM stock_picking WHERE location_dest_id = ANY($1) \
    ) t";

const PARTNER_PICKINGS: &str =
    "SELECT partner_id::int8, id::int8 FROM stock_picking WHERE partner_id = ANY($1)";

fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Reading linked inventory records failed.").with_source(error)
}

fn row_of(id: &RecId) -> Option<i64> {
    id.as_real().or(id.origin()).map(|id| id.get())
}

async fn readable_links(
    tx: &mut Transaction,
    sources: &Records,
    sql: &str,
    target: &str,
) -> Result<BTreeMap<i64, Vec<i64>>> {
    let ids: Vec<i64> = sources.rec_ids().iter().filter_map(row_of).collect();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut links = Vec::new();
    for row in tx.query(sql, &[&ids]).await? {
        let source: i64 = row.try_get(0).map_err(decode)?;
        let linked: i64 = row.try_get(1).map_err(decode)?;
        links.push((source, linked));
    }
    let readable: BTreeSet<i64> = sources
        .env()
        .records(target)?
        .browse(links.iter().map(|(_, linked)| *linked))?
        .filtered_access(tx, Operation::Read)
        .await?
        .ids()
        .into_iter()
        .collect();
    let mut out: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for (source, linked) in links {
        if readable.contains(&linked) {
            out.entry(source).or_default().push(linked);
        }
    }
    for linked in out.values_mut() {
        linked.sort_unstable();
        linked.dedup();
    }
    Ok(out)
}

fn counts(sources: &Records, found: &BTreeMap<i64, Vec<i64>>) -> Result<Vec<i32>> {
    sources
        .rec_ids()
        .iter()
        .map(|id| {
            let n = row_of(id).and_then(|id| found.get(&id)).map_or(0, Vec::len);
            i32::try_from(n).map_err(|_| Error::programming("record count does not fit an i32"))
        })
        .collect()
}

fn flat(found: BTreeMap<i64, Vec<i64>>) -> Vec<i64> {
    let mut all: Vec<i64> = found.into_values().flatten().collect();
    all.sort_unstable();
    all.dedup();
    all
}

fn window(name: &str, model: &str, ids: Vec<i64>) -> Value {
    let context = ValueMap::from([("create".to_owned(), Value::Bool(false))]);
    if let [id] = ids.as_slice() {
        return Value::Map(Arc::new(ValueMap::from([
            ("type".to_owned(), Value::str("ir.actions.act_window")),
            ("name".to_owned(), Value::str(name)),
            ("res_model".to_owned(), Value::str(model)),
            ("target".to_owned(), Value::str("current")),
            ("res_id".to_owned(), Value::Int(*id)),
            (
                "views".to_owned(),
                Value::list([Value::list([Value::False, Value::str("form")])]),
            ),
            ("context".to_owned(), Value::Map(Arc::new(context))),
        ])));
    }
    Value::Map(Arc::new(ValueMap::from([
        ("type".to_owned(), Value::str("ir.actions.act_window")),
        ("name".to_owned(), Value::str(name)),
        ("res_model".to_owned(), Value::str(model)),
        ("target".to_owned(), Value::str("current")),
        (
            "views".to_owned(),
            Value::list([
                Value::list([Value::False, Value::str("list")]),
                Value::list([Value::False, Value::str("form")]),
            ]),
        ),
        (
            "domain".to_owned(),
            Value::list([Value::list([
                Value::str("id"),
                Value::str("in"),
                Value::list(ids.into_iter().map(Value::Int)),
            ])]),
        ),
        ("context".to_owned(), Value::Map(Arc::new(context))),
    ])))
}

/// `stock.picking` smart buttons for returns and backorders.
#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    #[compute(depends = ["state", "company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_backorder_count(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockPicking>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, PICKING_BACKORDERS, "stock.picking").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, StockPicking::BACKORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open readable backorder transfers split from this transfer.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_backorders(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PICKING_BACKORDERS, "stock.picking").await?;
        Ok(window("Backorders", "stock.picking", flat(found)))
    }

    /// Open readable return transfers created from this transfer.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_returns(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PICKING_RETURNS, "stock.picking").await?;
        Ok(window("Returns", "stock.picking", flat(found)))
    }
}

/// `product.template` inventory smart buttons: On Hand, Moves, Reordering Rules.
#[extend(ProductTemplate, addon = "stock")]
pub struct ProductTemplateStockButtons {
    /// On-hand quantity summed across readable internal stock quants of this product's variants.
    #[field(float, compute = "_compute_stock_counters", string = "On Hand")]
    pub qty_on_hand: f64,
    /// Readable stock moves touching this product's variants.
    #[field(integer, compute = "_compute_stock_counters", string = "Stock Moves")]
    pub stock_move_count: i32,
    /// Readable reordering rules defined for this product's variants.
    #[field(
        integer,
        compute = "_compute_stock_counters",
        string = "Reordering Rules"
    )]
    pub reordering_rule_count: i32,
}

#[methods(ProductTemplate, addon = "stock")]
impl ProductTemplateStockButtons {
    #[compute(depends = ["company_id", "is_storable"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_stock_counters(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductTemplate>,
    ) -> Result<()> {
        let records = this.as_records();
        let quants = readable_links(tx, records, PRODUCT_INTERNAL_QUANTS, "stock.quant").await?;
        let moves = readable_links(tx, records, PRODUCT_MOVES, "stock.move").await?;
        let rules = readable_links(
            tx,
            records,
            PRODUCT_ORDERPOINTS,
            "stock.warehouse.orderpoint",
        )
        .await?;

        let all_quant_ids = flat(quants.clone());
        let mut qty_by_quant: BTreeMap<i64, f64> = BTreeMap::new();
        if !all_quant_ids.is_empty() {
            for row in tx
                .query(
                    "SELECT id::int8, quantity::float8 FROM stock_quant WHERE id = ANY($1)",
                    &[&all_quant_ids],
                )
                .await?
            {
                let qid: i64 = row.try_get(0).map_err(decode)?;
                let qty: f64 = row.try_get(1).map_err(decode)?;
                qty_by_quant.insert(qid, qty);
            }
        }

        let move_counts = counts(records, &moves)?;
        let rule_counts = counts(records, &rules)?;
        for (index, rec_id) in records.rec_ids().iter().enumerate() {
            let on_hand: f64 = row_of(rec_id)
                .and_then(|id| quants.get(&id))
                .map_or(0.0, |qids| {
                    qids.iter()
                        .filter_map(|qid| qty_by_quant.get(qid))
                        .copied()
                        .sum()
                });
            out.set(index, ProductTemplateStockButtons::QTY_ON_HAND, on_hand)?;
            out.set(
                index,
                ProductTemplateStockButtons::STOCK_MOVE_COUNT,
                move_counts[index],
            )?;
            out.set(
                index,
                ProductTemplateStockButtons::REORDERING_RULE_COUNT,
                rule_counts[index],
            )?;
        }
        Ok(())
    }

    /// Open internal stock quants on hand for this product.
    #[method]
    #[rpc(effect = read)]
    async fn action_open_quants(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PRODUCT_INTERNAL_QUANTS, "stock.quant").await?;
        Ok(window("Stock On Hand", "stock.quant", flat(found)))
    }

    /// Open stock move history for this product.
    #[method]
    #[rpc(effect = read)]
    async fn action_open_stock_moves(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PRODUCT_MOVES, "stock.move").await?;
        Ok(window("Stock Moves", "stock.move", flat(found)))
    }

    /// Open reordering rules for this product.
    #[method]
    #[rpc(effect = read)]
    async fn action_open_orderpoints(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(
            tx,
            records,
            PRODUCT_ORDERPOINTS,
            "stock.warehouse.orderpoint",
        )
        .await?;
        Ok(window(
            "Reordering Rules",
            "stock.warehouse.orderpoint",
            flat(found),
        ))
    }
}

/// `stock.warehouse` smart buttons: Operation Types and Transfers.
#[methods(StockWarehouse, addon = "stock")]
impl StockWarehouse {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_warehouse_counts(
        this: &RecordSet<StockWarehouse>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockWarehouse>,
    ) -> Result<()> {
        let records = this.as_records();
        let types =
            readable_links(tx, records, WAREHOUSE_PICKING_TYPES, "stock.picking.type").await?;
        let pickings = readable_links(tx, records, WAREHOUSE_PICKINGS, "stock.picking").await?;
        let type_counts = counts(records, &types)?;
        let picking_counts = counts(records, &pickings)?;
        for index in 0..records.len() {
            out.set(
                index,
                StockWarehouse::PICKING_TYPE_COUNT,
                type_counts[index],
            )?;
            out.set(index, StockWarehouse::PICKING_COUNT, picking_counts[index])?;
        }
        Ok(())
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_view_picking_types(
        this: &RecordSet<StockWarehouse>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found =
            readable_links(tx, records, WAREHOUSE_PICKING_TYPES, "stock.picking.type").await?;
        Ok(window("Operation Types", "stock.picking.type", flat(found)))
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_view_pickings(
        this: &RecordSet<StockWarehouse>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, WAREHOUSE_PICKINGS, "stock.picking").await?;
        Ok(window("Transfers", "stock.picking", flat(found)))
    }
}

/// `stock.location` smart buttons: Current Stock and Transfers.
#[methods(StockLocation, addon = "stock")]
impl StockLocation {
    #[compute(depends = ["company_id", "usage"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_location_counts(
        this: &RecordSet<StockLocation>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockLocation>,
    ) -> Result<()> {
        let records = this.as_records();
        let quants = readable_links(tx, records, LOCATION_QUANTS, "stock.quant").await?;
        let pickings = readable_links(tx, records, LOCATION_PICKINGS, "stock.picking").await?;
        let quant_counts = counts(records, &quants)?;
        let picking_counts = counts(records, &pickings)?;
        for index in 0..records.len() {
            out.set(index, StockLocation::QUANT_COUNT, quant_counts[index])?;
            out.set(index, StockLocation::PICKING_COUNT, picking_counts[index])?;
        }
        Ok(())
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_view_quants(
        this: &RecordSet<StockLocation>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, LOCATION_QUANTS, "stock.quant").await?;
        Ok(window("Current Stock", "stock.quant", flat(found)))
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_view_pickings(
        this: &RecordSet<StockLocation>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, LOCATION_PICKINGS, "stock.picking").await?;
        Ok(window("Transfers", "stock.picking", flat(found)))
    }
}

/// `res.partner` smart button for Transfers.
#[extend(ResPartner, addon = "stock")]
pub struct PartnerStockPickings {
    /// Readable transfers linked to this contact.
    #[field(integer, compute = "_compute_picking_count", string = "Transfers")]
    pub picking_count: i32,
}

#[methods(ResPartner, addon = "stock")]
impl PartnerStockPickings {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_picking_count(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ResPartner>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, PARTNER_PICKINGS, "stock.picking").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, PartnerStockPickings::PICKING_COUNT, count)?;
        }
        Ok(())
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_view_pickings(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PARTNER_PICKINGS, "stock.picking").await?;
        Ok(window("Transfers", "stock.picking", flat(found)))
    }
}
