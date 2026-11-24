// SPDX-License-Identifier: MIT OR Apache-2.0
//! The count buttons that join a purchase order and its vendor bills: "Vendor Bills" on the order,
//! "Purchase Orders" on the bill. Both show only documents the caller may read.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use erp_addon_account::moves::AccountMove;
use erp_sdk::prelude::*;
use erp_sdk::{Operation, ValueMap};

use crate::order::PurchaseOrder;

/// Order id, bill id.
const ORDER_BILLS: &str = "SELECT DISTINCT k.order_id::int8, m.id::int8 FROM purchase_direct_bill_line k \
    JOIN account_move m ON m.id = k.bill_id \
    WHERE k.order_id = ANY($1) AND m.move_type IN ('in_invoice', 'in_refund')";
/// Bill id, order id.
const BILL_ORDERS: &str = "SELECT DISTINCT m.id::int8, k.order_id::int8 FROM account_move m \
    JOIN purchase_direct_bill_line k ON k.bill_id = m.id \
    WHERE m.id = ANY($1) AND m.move_type IN ('in_invoice', 'in_refund')";

fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Reading the documents linked to a purchase order failed.").with_source(error)
}

/// The stored row a record stands for: its own id, its origin for an unsaved copy.
fn row_of(id: &RecId) -> Option<i64> {
    id.as_real().or(id.origin()).map(|id| id.get())
}

/// Per source row, the sorted ids of the linked `target` documents the caller may read.
async fn readable_links(
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

fn counts(sources: &Records, found: &BTreeMap<i64, Vec<i64>>) -> Result<Vec<i32>> {
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
fn window(name: &str, model: &str, views: Vec<(i64, &str)>, ids: Vec<i64>) -> Value {
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
    let context = ValueMap::from([("create".to_owned(), Value::Bool(false))]);
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
    let find = |wanted: &str| -> Result<i64> {
        rows.iter()
            .find(|row| row.get::<_, String>(0) == wanted)
            .map(|row| row.get::<_, i64>(1))
            .ok_or_else(|| Error::invariant(format!("The invoice view {wanted} is missing.")))
    };
    Ok(vec![
        (find("view_invoice_list")?, "list"),
        (find("view_invoice_form")?, "form"),
    ])
}

fn flat(found: BTreeMap<i64, Vec<i64>>) -> Vec<i64> {
    found.into_values().flatten().collect()
}

/// `purchase.order` with the bills created for it.
#[extend(PurchaseOrder, addon = "purchase")]
pub struct PurchaseOrderBills {
    /// How many readable vendor bills and refunds the order has.
    #[field(integer, compute = "_compute_invoice_count", string = "Bill Count")]
    pub invoice_count: i32,
}

#[methods(PurchaseOrder, addon = "purchase")]
impl PurchaseOrderBills {
    #[compute(depends = [])]
    async fn _compute_invoice_count(
        this: &RecordSet<PurchaseOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, PurchaseOrder>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, ORDER_BILLS, "account.move").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, PurchaseOrderBills::INVOICE_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the order's vendor bills in the invoice list and form.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_invoice(
        this: &RecordSet<PurchaseOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, ORDER_BILLS, "account.move").await?;
        let views = invoice_views(tx).await?;
        Ok(window("Vendor Bills", "account.move", views, flat(found)))
    }
}

/// `account.move` with the way back to the purchase orders it bills.
#[extend(AccountMove, addon = "purchase")]
pub struct PurchaseBillOrders {
    /// How many readable purchase orders the bill belongs to.
    #[field(
        integer,
        compute = "_compute_purchase_order_count",
        string = "Purchase Order Count"
    )]
    pub purchase_order_count: i32,
}

#[methods(AccountMove, addon = "purchase")]
impl PurchaseBillOrders {
    #[compute(depends = [])]
    async fn _compute_purchase_order_count(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountMove>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, BILL_ORDERS, "purchase.order").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, PurchaseBillOrders::PURCHASE_ORDER_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the purchase orders this bill belongs to.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_purchase_orders(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, BILL_ORDERS, "purchase.order").await?;
        Ok(window(
            "Purchase Orders",
            "purchase.order",
            Vec::new(),
            flat(found),
        ))
    }
}
