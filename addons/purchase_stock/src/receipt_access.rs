// SPDX-License-Identifier: MIT OR Apache-2.0
//! Keep purchase-only stock mutations within the associations that authorize them.

use erp_addon_stock::{StockMove, StockPicking, stock_move_line::StockMoveLine};
use erp_sdk::prelude::*;
use erp_sdk::{Env, Operation};

const REFUSED: &str = "Purchase receipt access requires matching incoming stock relations.";

async fn purchase_only(tx: &mut Transaction, env: &Env) -> Result<bool> {
    Ok(!env.su()
        && tx.has_group(env, erp_addon_purchase::USER_GROUP).await?
        && !tx.has_group(env, "stock.group_stock_user").await?
        && !tx.has_group(env, "stock.group_stock_manager").await?)
}

const MOVE_MISMATCH: &str = "SELECT 1 FROM stock_move m \
    LEFT JOIN stock_picking p ON p.id = m.picking_id \
    LEFT JOIN stock_picking_type t ON t.id = p.picking_type_id \
    WHERE m.id = ANY($1) AND (t.code IS DISTINCT FROM 'incoming' \
        OR m.picking_type_id IS DISTINCT FROM p.picking_type_id) LIMIT 1";

const LINE_MISMATCH: &str = "SELECT 1 FROM stock_move_line l \
    LEFT JOIN stock_move m ON m.id = l.move_id \
    LEFT JOIN stock_picking p ON p.id = m.picking_id \
    LEFT JOIN stock_picking_type t ON t.id = p.picking_type_id \
    WHERE l.id = ANY($1) AND (t.code IS DISTINCT FROM 'incoming' \
        OR m.picking_type_id IS DISTINCT FROM p.picking_type_id \
        OR l.picking_id IS DISTINCT FROM p.id) LIMIT 1";

const PICKING_MISMATCH: &str = "SELECT 1 FROM stock_picking p \
    LEFT JOIN stock_picking_type t ON t.id = p.picking_type_id \
    WHERE p.id = ANY($1) AND (t.code IS DISTINCT FROM 'incoming' OR EXISTS ( \
        SELECT 1 FROM stock_move m WHERE m.picking_id = p.id \
        AND m.picking_type_id IS DISTINCT FROM p.picking_type_id)) LIMIT 1";

async fn check_links(tx: &mut Transaction, records: &Records, query: &str) -> Result<()> {
    tx.flush().await?;
    if !tx.query(query, &[&records.ids()]).await?.is_empty() {
        return Err(Error::access(REFUSED));
    }
    Ok(())
}

async fn create_checked(
    tx: &mut Transaction,
    records: &Records,
    values: Vec<RawVals>,
    sup: Super<(Vec<RawVals>,), Records>,
    query: &str,
) -> Result<Records> {
    if !purchase_only(tx, records.env()).await? {
        return sup.call(tx, records, (values,)).await;
    }
    tx.savepoint(async move |tx| {
        let created = sup.call(tx, records, (values,)).await?;
        check_links(tx, &created, query).await?;
        Ok(created)
    })
    .await
}

async fn write_checked(
    tx: &mut Transaction,
    records: &Records,
    values: RawVals,
    sup: Super<(RawVals,), ()>,
    query: &str,
) -> Result<()> {
    if records.is_empty() || !purchase_only(tx, records.env()).await? {
        return sup.call(tx, records, (values,)).await;
    }
    records.check_access(tx, Operation::Write).await?;
    tx.savepoint(async move |tx| {
        check_links(tx, records, query).await?;
        let () = sup.call(tx, records, (values,)).await?;
        check_links(tx, records, query).await?;
        records.check_access(tx, Operation::Write).await
    })
    .await
}

#[extend(StockMove, addon = "purchase_stock")]
struct PurchaseReceiptMove {}

#[methods(StockMove, addon = "purchase_stock")]
impl PurchaseReceiptMove {
    #[method(override)]
    async fn create(
        this: &RecordSet<StockMove>,
        tx: &mut Transaction,
        values: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        create_checked(tx, this.as_records(), values, sup, MOVE_MISMATCH).await
    }

    #[method(override)]
    async fn write(
        this: &RecordSet<StockMove>,
        tx: &mut Transaction,
        values: RawVals,
        sup: Super,
    ) -> Result<()> {
        write_checked(tx, this.as_records(), values, sup, MOVE_MISMATCH).await
    }
}

#[extend(StockMoveLine, addon = "purchase_stock")]
struct PurchaseReceiptMoveLine {}

#[methods(StockMoveLine, addon = "purchase_stock")]
impl PurchaseReceiptMoveLine {
    #[method(override)]
    async fn create(
        this: &RecordSet<StockMoveLine>,
        tx: &mut Transaction,
        values: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        create_checked(tx, this.as_records(), values, sup, LINE_MISMATCH).await
    }

    #[method(override)]
    async fn write(
        this: &RecordSet<StockMoveLine>,
        tx: &mut Transaction,
        values: RawVals,
        sup: Super,
    ) -> Result<()> {
        write_checked(tx, this.as_records(), values, sup, LINE_MISMATCH).await
    }

    #[method(override)]
    async fn unlink(
        this: &RecordSet<StockMoveLine>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        if !records.is_empty() && purchase_only(tx, records.env()).await? {
            records.check_access(tx, Operation::Unlink).await?;
            check_links(tx, records, LINE_MISMATCH).await?;
        }
        sup.call(tx, records, ()).await
    }
}

#[extend(StockPicking, addon = "purchase_stock")]
struct PurchaseReceiptPicking {}

#[methods(StockPicking, addon = "purchase_stock")]
impl PurchaseReceiptPicking {
    #[method(override)]
    async fn write(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        values: RawVals,
        sup: Super,
    ) -> Result<()> {
        if !values.contains("picking_type_id") {
            return sup.call(tx, this.as_records(), (values,)).await;
        }
        write_checked(tx, this.as_records(), values, sup, PICKING_MISMATCH).await
    }
}
