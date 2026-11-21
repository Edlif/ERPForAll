// SPDX-License-Identifier: MIT OR Apache-2.0
//! The purchase line's received quantity is a projection of its own completed incoming moves.

use erp_addon_purchase::line::PurchaseOrderLine;
use erp_addon_stock::stock_move_line::StockMoveLine;
use erp_addon_uom::stock::product_quantity_sql;
use erp_sdk::migrations::SemverVersion;
use erp_sdk::prelude::*;

#[extend(PurchaseOrderLine, addon = "purchase_stock")]
struct ReceiptQuantity {
    #[field(selection, selection_add = [("stock_moves", "Stock Moves")], ondelete = [("stock_moves", "set default")])]
    qty_received_method: Option<Arc<str>>,
}

fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Could not read the lines of a completed purchase receipt.").with_source(error)
}

// A direct receipt's origin is fixed by the private server-owned link tables. A similarly named
// transfer, another line for the same SKU, or a later unlinked exchange is a different document.
// m.quantity is already the actual sum of its stock lines expressed in its own captured unit.
// The same-unit path preserves that historical measure across changes to the unit catalogue.
fn update_sql() -> Result<String> {
    let converted = product_quantity_sql("actual", "source_factor", "target_factor", "digits")
        .map_err(|error| {
            Error::invariant("Could not form the purchase quantity conversion.").with_source(error)
        })?;
    Ok(format!(
        "WITH amounts AS (
             SELECT l.id, m.quantity AS actual, m.uom_factor AS source_factor,
                    unit.factor AS target_factor, m.product_unit_digits AS digits,
                    l.uom_id IS NULL OR l.uom_id=m.uom_id AS same_unit,
                    m.state='done' AND m.is_scrap IS NOT TRUE AND m.product_id=l.product_id
                        AND src.usage='supplier'
                        AND dst.usage='internal' AS received
             FROM purchase_stock_receipt_move_link ml
             JOIN purchase_order_line l ON l.id=ml.purchase_line_id AND l.company_id=ml.company_id
             JOIN purchase_stock_receipt_link r ON r.order_id=l.order_id AND r.company_id=l.company_id
             JOIN stock_move root ON root.id=ml.move_id AND root.company_id=ml.company_id AND root.picking_id=r.picking_id
             JOIN stock_move m ON (m.id=root.id OR m.backorder_root_move_id=root.id) AND m.company_id=ml.company_id
             JOIN stock_location src ON src.id=m.location_id
             JOIN stock_location dst ON dst.id=m.location_dest_id
             LEFT JOIN uom_uom unit ON unit.id=l.uom_id
             WHERE ($1::bigint[] IS NULL OR l.id=ANY($1))
         ), totals AS (
             SELECT id, SUM(CASE WHEN NOT received THEN 0 WHEN same_unit THEN actual
                                 ELSE {converted} END) AS received FROM amounts GROUP BY id
         )
         UPDATE purchase_order_line l SET qty_received=t.received, qty_received_method='stock_moves'
         FROM totals t WHERE l.id=t.id AND
             (l.qty_received, l.qty_received_method) IS DISTINCT FROM (t.received, 'stock_moves')"
    ))
}

pub(crate) async fn refresh(tx: &mut Transaction, lines: &Records) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    // Native purchase writes may still hold the newly chosen unit in the ORM cache.
    tx.flush().await?;
    let ids = lines.ids();
    // A receipt and a purchase-unit edit may commit together. Take ordered line locks in a
    // separate statement: at READ COMMITTED the recalculation sees the winning writer.
    tx.query(
        "SELECT id FROM purchase_order_line WHERE id=ANY($1) ORDER BY id FOR UPDATE",
        &[&ids],
    )
    .await?;
    let changed = tx
        .query(&format!("{} RETURNING l.id", update_sql()?), &[&ids])
        .await?;
    let ids: Vec<i64> = changed
        .iter()
        .map(|row| row.try_get(0))
        .collect::<std::result::Result<_, _>>()
        .map_err(decode)?;
    if !ids.is_empty() {
        lines
            .env()
            .records("purchase.order.line")?
            .browse(ids)?
            .modified_external(tx, &["qty_received", "qty_received_method"])
            .await?;
        erp_addon_purchase::line::invalidate_bill_comparisons(tx).await?;
    }
    Ok(())
}

#[methods(PurchaseOrderLine, addon = "purchase_stock")]
impl ReceiptQuantity {
    #[method(override)]
    async fn write(
        this: &RecordSet<PurchaseOrderLine>,
        tx: &mut Transaction,
        values: RawVals,
        sup: Super,
    ) -> Result<()> {
        let lines = this.as_records();
        if !values.contains("uom_id") && !values.contains("product_id") {
            return sup.call(tx, lines, (values,)).await;
        }
        tx.savepoint(async |tx| {
            let () = sup.call(tx, lines, (values,)).await?;
            refresh(tx, lines).await
        })
        .await
    }
}

#[extend(StockMoveLine, addon = "purchase_stock")]
struct CompletedReceiptLine {}

#[methods(StockMoveLine, addon = "purchase_stock")]
impl CompletedReceiptLine {
    #[method(override)]
    async fn _after_stock_completion(
        this: &RecordSet<StockMoveLine>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        let () = sup.call(tx, records, ()).await?;
        let rows = tx.query(
            "SELECT DISTINCT ml.purchase_line_id FROM stock_move_line s
             JOIN stock_move m ON m.id=s.move_id AND m.company_id=s.company_id
             JOIN purchase_stock_receipt_move_link ml ON ml.move_id=COALESCE(m.backorder_root_move_id,m.id) AND ml.company_id=s.company_id
             WHERE s.id=ANY($1) AND s.membership='done' ORDER BY ml.purchase_line_id",
            &[&records.ids()],
        ).await?;
        let ids: Vec<i64> = rows
            .iter()
            .map(|row| row.try_get(0))
            .collect::<std::result::Result<_, _>>()
            .map_err(decode)?;
        refresh(
            tx,
            &records.env().records("purchase.order.line")?.browse(ids)?,
        )
        .await
    }
}

#[erp_sdk::migration(addon="purchase_stock",version="1.2",stage=post)]
async fn backfill_received(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
    tx.execute(&update_sql()?, &[&Option::<Vec<i64>>::None])
        .await?;
    Ok(())
}
