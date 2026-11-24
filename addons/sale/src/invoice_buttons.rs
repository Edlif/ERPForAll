// SPDX-License-Identifier: MIT OR Apache-2.0
//! The count buttons that join a sales order and its customer invoices: "Invoices" on the order,
//! "Sales Orders" on the invoice. Both show only documents the caller may read.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use erp_addon_account::moves::AccountMove;
use erp_sdk::prelude::*;
use erp_sdk::{Operation, ValueMap};

use crate::SaleOrder;

/// Order id, invoice id.
const ORDER_INVOICES: &str = "SELECT DISTINCT s.order_id::int8, m.id::int8 FROM sale_invoice_item k \
    JOIN sale_order_line s ON s.id = k.sale_line_id \
    JOIN account_move_line l ON l.id = k.invoice_item_id \
    JOIN account_move m ON m.id = l.move_id \
    WHERE s.order_id = ANY($1) AND m.move_type IN ('out_invoice', 'out_refund')";
/// Invoice id, order id.
const INVOICE_ORDERS: &str = "SELECT DISTINCT m.id::int8, s.order_id::int8 FROM account_move m \
    JOIN account_move_line l ON l.move_id = m.id \
    JOIN sale_invoice_item k ON k.invoice_item_id = l.id \
    JOIN sale_order_line s ON s.id = k.sale_line_id \
    WHERE m.id = ANY($1) AND m.move_type IN ('out_invoice', 'out_refund')";

fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Reading the documents linked to a sales order failed.").with_source(error)
}

/// The stored row a record stands for: its own id, its origin for an unsaved copy.
fn row_of(id: &RecId) -> Option<i64> {
    id.as_real().or(id.origin()).map(|id| id.get())
}

/// Per source row, the sorted ids of the linked `target` documents the caller may read.
pub async fn readable_links(
    tx: &mut Transaction,
    sources: &Records,
    sql: &str,
    target: &str,
) -> Result<BTreeMap<i64, Vec<i64>>> {
    let ids: Vec<i64> = sources.rec_ids().iter().filter_map(row_of).collect();
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
    }
    Ok(out)
}

/// The number of linked documents per source record, in record order.
pub fn counts(sources: &Records, found: &BTreeMap<i64, Vec<i64>>) -> Result<Vec<i32>> {
    sources
        .rec_ids()
        .iter()
        .map(|id| {
            let n = row_of(id).and_then(|id| found.get(&id)).map_or(0, Vec::len);
            i32::try_from(n).map_err(|_| Error::programming("document count does not fit an i32"))
        })
        .collect()
}

/// A current-window action over the readable documents; `views` empty means the model's defaults.
/// Exactly one document opens straight in its form, stacked on the page the button is on; more
/// (or none) open the list.
pub fn window(name: &str, model: &str, views: Vec<(i64, &str)>, ids: Vec<i64>) -> Value {
    let context = ValueMap::from([("create".to_owned(), Value::Bool(false))]);
    if let [id] = ids.as_slice() {
        let form = views
            .iter()
            .find(|(_, kind)| *kind == "form")
            .map_or(Value::False, |(view, _)| Value::Int(*view));
        return Value::Map(Arc::new(ValueMap::from([
            ("type".to_owned(), Value::str("ir.actions.act_window")),
            ("name".to_owned(), Value::str(name)),
            ("res_model".to_owned(), Value::str(model)),
            ("target".to_owned(), Value::str("current")),
            ("res_id".to_owned(), Value::Int(*id)),
            (
                "views".to_owned(),
                Value::list([Value::list([form, Value::str("form")])]),
            ),
            ("context".to_owned(), Value::Map(Arc::new(context))),
        ])));
    }
    let views = if views.is_empty() {
        vec![
            Value::list([Value::False, Value::str("list")]),
            Value::list([Value::False, Value::str("form")]),
        ]
    } else {
        views
            .into_iter()
            .map(|(id, kind)| Value::list([Value::Int(id), Value::str(kind)]))
            .collect()
    };
    Value::Map(Arc::new(ValueMap::from([
        ("type".to_owned(), Value::str("ir.actions.act_window")),
        ("name".to_owned(), Value::str(name)),
        ("res_model".to_owned(), Value::str(model)),
        ("target".to_owned(), Value::str("current")),
        ("views".to_owned(), Value::list(views)),
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

async fn invoice_views(tx: &mut Transaction) -> Result<Vec<(i64, &'static str)>> {
    let rows = tx
        .query(
            "SELECT name, res_id::int8 FROM ir_model_data WHERE module = 'account' \
             AND model = 'ir.ui.view' AND name IN ('view_invoice_list', 'view_invoice_form')",
            &[],
        )
        .await?;
    let mut found = Vec::with_capacity(rows.len());
    for row in &rows {
        found.push((
            row.try_get::<_, String>(0).map_err(decode)?,
            row.try_get::<_, i64>(1).map_err(decode)?,
        ));
    }
    let find = |wanted: &str| -> Result<i64> {
        found
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, id)| *id)
            .ok_or_else(|| Error::invariant(format!("The invoice view {wanted} is missing.")))
    };
    Ok(vec![
        (find("view_invoice_list")?, "list"),
        (find("view_invoice_form")?, "form"),
    ])
}

/// Every linked id of all sources, in one list.
pub fn flat(found: BTreeMap<i64, Vec<i64>>) -> Vec<i64> {
    found.into_values().flatten().collect()
}

#[methods(SaleOrder, addon = "sale")]
impl SaleOrder {
    /// How many readable customer invoices and credit notes the order has; it follows the links
    /// of the order's lines.
    #[compute(depends = ["order_line.invoice_item_link_ids"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_invoice_count(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, SaleOrder>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, ORDER_INVOICES, "account.move").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, SaleOrder::INVOICE_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the order's invoices and credit notes in the invoice list and form.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_invoice(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, ORDER_INVOICES, "account.move").await?;
        let views = invoice_views(tx).await?;
        Ok(window("Invoices", "account.move", views, flat(found)))
    }
}

/// `account.move` with the way back to the sales orders it invoices.
#[extend(AccountMove, addon = "sale")]
pub struct SalesInvoiceOrders {
    /// How many readable sales orders the invoice bills.
    #[field(
        integer,
        compute = "_compute_sale_order_count",
        string = "Sales Order Count"
    )]
    pub sale_order_count: i32,
}

#[methods(AccountMove, addon = "sale")]
impl SalesInvoiceOrders {
    /// Follows the sales source of the invoice's items.
    #[compute(depends = ["line_ids.sale_line_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_sale_order_count(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountMove>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, INVOICE_ORDERS, "sale.order").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, SalesInvoiceOrders::SALE_ORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the sales orders this invoice bills.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_sale_orders(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, INVOICE_ORDERS, "sale.order").await?;
        Ok(window(
            "Sales Orders",
            "sale.order",
            Vec::new(),
            flat(found),
        ))
    }
}
