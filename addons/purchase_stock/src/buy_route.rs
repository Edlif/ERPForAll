// SPDX-License-Identifier: MIT OR Apache-2.0
//! Provision the direct Buy receiving rule using stock's generated-identity contract.
use erp_addon_stock::{StockWarehouse, set_generated_identity};
use erp_sdk::Context;
use erp_sdk::prelude::*;

#[extend(StockWarehouse, addon = "purchase_stock")]
struct PurchasingWarehouse {}

#[methods(StockWarehouse, addon = "purchase_stock")]
impl PurchasingWarehouse {
    #[method(override)]
    async fn create(
        this: &RecordSet<StockWarehouse>,
        tx: &mut Transaction,
        values: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        tx.savepoint(async move |tx| {
            // Stock owns authorization, locations and receiving operations. Only use its returned warehouses.
            let created = sup.call(tx, this.as_records(), (values,)).await?;
            provision(tx, Some(created.ids()), false).await?;
            Ok(created)
        })
        .await
    }

    /// Called by purchase_stock's data file after the canonical Buy route is installed.
    #[method]
    #[data_callable(model)]
    async fn _purchase_stock_setup_buy_routes(
        _this: &RecordSet<StockWarehouse>,
        tx: &mut Transaction,
    ) -> Result<()> {
        provision(tx, None, true).await
    }
}

fn decode(err: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Reading the receiving warehouse topology failed.").with_source(err)
}

async fn provision(
    tx: &mut Transaction,
    warehouses: Option<Vec<i64>>,
    required: bool,
) -> Result<()> {
    tx.flush().await?;
    let route=tx.query("SELECT d.res_id::int8,r.active,r.company_id FROM ir_model_data d JOIN stock_route r ON r.id=d.res_id WHERE d.module='purchase_stock' AND d.name='route_buy' AND d.model='stock.route'",&[]).await?;
    let Some(row) = route.first() else {
        // Stock can create warehouses while installing its own dependency, before bridge data exists.
        let installed:bool=tx.query("SELECT EXISTS (SELECT 1 FROM ir_module_module WHERE name='purchase_stock' AND state='installed')",&[]).await?[0].try_get(0).map_err(decode)?;
        return if required || installed {
            Err(Error::invariant("The installed Buy route is missing."))
        } else {
            Ok(())
        };
    };
    if !row.try_get::<_, bool>(1).map_err(decode)? {
        return Ok(());
    }
    let route: i64 = row.try_get(0).map_err(decode)?;
    let route_company: Option<i64> = row.try_get(2).map_err(decode)?;
    let rows=tx.query(
        "SELECT w.id,w.company_id,w.code,w.lot_stock_id,t.id,t.default_location_src_id \
         FROM stock_warehouse w JOIN stock_picking_type t ON t.id=w.in_type_id AND t.company_id=w.company_id \
         JOIN stock_location src ON src.id=t.default_location_src_id AND src.usage='supplier' AND (src.company_id=w.company_id OR src.company_id IS NULL) \
         JOIN stock_location dst ON dst.id=w.lot_stock_id AND dst.usage='internal' AND (dst.company_id=w.company_id OR dst.company_id IS NULL) \
         WHERE ($1::int8[] IS NULL OR w.id=ANY($1)) AND w.active AND w.reception_steps='one_step' \
           AND ($3::int8 IS NULL OR $3=w.company_id) AND t.active AND t.code='incoming' AND t.default_location_dest_id=w.lot_stock_id \
           AND NOT EXISTS (SELECT 1 FROM stock_rule r WHERE \
              (r.generated_warehouse_id=w.id AND r.generated_addon='purchase_stock' AND r.generated_role='buy_receipt') \
              OR (r.route_id=$2 AND r.location_dest_id=w.lot_stock_id \
                  AND (r.company_id=w.company_id OR r.company_id IS NULL))) ORDER BY w.id",
        &[&warehouses,&route,&route_company]).await?;
    for row in rows {
        let wh: i64 = row.try_get(0).map_err(decode)?;
        let co: i64 = row.try_get(1).map_err(decode)?;
        let code: String = row.try_get(2).map_err(decode)?;
        let loc: i64 = row.try_get(3).map_err(decode)?;
        let op: i64 = row.try_get(4).map_err(decode)?;
        let src: i64 = row.try_get(5).map_err(decode)?;
        // Mirrors stock's topology write after its authorized warehouse creation; parameters are
        // exclusively the warehouse's own receiving graph. The public buying action never elevates writes.
        let system = tx
            .env()
            .with_context_replaced(Context::new())
            .sudo(true)
            .with_company(co)?;
        let rule = system
            .records("stock.rule")?
            .create(
                tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str(format!("{code}: Buy").into()))
                        .set_cell("company_id", Cell::Id(co))
                        .set_cell("route_id", Cell::Id(route))
                        .set_cell("active", Cell::Bool(true))
                        .set_cell("action", Cell::Str("buy".into()))
                        .set_cell("location_src_id", Cell::Id(src))
                        .set_cell("location_dest_id", Cell::Id(loc))
                        .set_cell("picking_type_id", Cell::Id(op)),
                ],
            )
            .await?;
        set_generated_identity(tx, &rule, wh, "purchase_stock", "buy_receipt").await?;
    }
    Ok(())
}
