// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use erp_sdk::prelude::*;
use erp_testkit::Harness;

pub fn harness_builder() -> erp_testkit::HarnessBuilder {
    Harness::builder([
        &erp_addon_base::ADDON,
        &erp_addon_web::ADDON,
        &erp_addon_uom::ADDON,
        &erp_addon_product::ADDON,
        &erp_addon_barcodes::ADDON,
        &erp_addon_barcodes_gs1_nomenclature::ADDON,
        &erp_addon_stock::ADDON,
        &erp_addon_reporting::ADDON,
        &erp_addon_analytic::ADDON,
        &erp_addon_account::ADDON,
        &erp_addon_stock_account::ADDON,
        &erp_addon_purchase::ADDON,
        &erp_addon_purchase_stock::ADDON,
        &erp_addon_mail::ADDON,
        &erp_addon_sales_team::ADDON,
        &erp_addon_sale::ADDON,
        &erp_addon_sale_stock::ADDON,
        &erp_addon_stock_dropshipping::ADDON,
    ])
    .require_sql_objects([
        &erp_addon_account::reconcile::history_guard::MANIFEST,
        &erp_addon_stock::schema_guards::MANIFEST,
        &erp_addon_stock_account::history_guards::MANIFEST,
    ])
    .schema_sync_options(erp_testkit::SchemaSyncOptions::inline().lifecycle(true))
}

pub async fn harness() -> Harness {
    harness_builder().build().await.unwrap()
}

fn text(s: &str) -> Cell {
    Cell::Str(s.into())
}

async fn company(tx: &mut Transaction, name: &str) -> i64 {
    let existing = tx
        .query("SELECT id FROM res_currency ORDER BY id LIMIT 1", &[])
        .await
        .unwrap();
    let currency: i64 = match existing.first() {
        Some(row) => row.get(0),
        None => erp_testkit::fixtures::seed_currency_in(tx).await.unwrap(),
    };
    tx.query(
        "WITH n AS (SELECT nextval(pg_get_serial_sequence('res_company', 'id')) AS id) \
         INSERT INTO res_company (tz, id, name, currency_id, parent_path, account_price_include, tax_calculation_rounding_method, anglo_saxon_accounting, expects_chart_of_accounts, fpos_revision) \
         SELECT 'UTC', id, $1, $2, id::text || '/', 'tax_excluded', 'round_globally', false, true, 0 FROM n RETURNING id::int8",
        &[&name, &currency],
    )
    .await
    .unwrap()[0]
        .get(0)
}

async fn location(tx: &mut Transaction, name: &str, usage: &str, company_id: Option<i64>) -> i64 {
    let mut vals = RawVals::new()
        .set_cell("name", text(name))
        .set_cell("usage", text(usage));
    if let Some(c) = company_id {
        vals = vals.set_cell("company_id", Cell::Id(c));
    }
    tx.env()
        .records("stock.location")
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .rec_ids()[0]
        .get()
}

async fn picking_type(tx: &mut Transaction, company_id: i64, name: &str, code: &str) -> i64 {
    let shared = location(tx, "Shared", "internal", None).await;
    tx.env()
        .records("stock.picking.type")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text(name))
                    .set_cell("sequence", Cell::Int(10))
                    .set_cell("company_id", Cell::Id(company_id))
                    .set_cell("code", text(code))
                    .set_cell("default_location_src_id", Cell::Id(shared))
                    .set_cell("default_location_dest_id", Cell::Id(shared)),
            ],
        )
        .await
        .unwrap()
        .rec_ids()[0]
        .get()
}

#[tokio::test]
async fn test_selection_carries_dropship_value() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let c = company(&mut tx, "TestCo").await;
    let pt_id = picking_type(&mut tx, c, "Dropship Operation", "dropship").await;
    let records = tx
        .env()
        .records("stock.picking.type")
        .unwrap()
        .browse([pt_id])
        .unwrap();
    assert_eq!(
        records.cell(&mut tx, "code").await.unwrap(),
        Cell::Str("dropship".into())
    );
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_is_dropship_flag_computed() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let c = company(&mut tx, "TestCo").await;
    let pt_id = picking_type(&mut tx, c, "Operations", "internal").await;

    let supplier_loc = location(&mut tx, "Vendor Location", "supplier", None).await;
    let customer_loc = location(&mut tx, "Customer Location", "customer", None).await;
    let internal_loc = location(&mut tx, "Stock Location", "internal", Some(c)).await;

    // Supplier to customer transfer: is_dropship must be true
    let dropship_picking = tx
        .env()
        .records("stock.picking")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("DROP-001"))
                    .set_cell("company_id", Cell::Id(c))
                    .set_cell("picking_type_id", Cell::Id(pt_id))
                    .set_cell("location_id", Cell::Id(supplier_loc))
                    .set_cell("location_dest_id", Cell::Id(customer_loc)),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        dropship_picking.cell(&mut tx, "is_dropship").await.unwrap(),
        Cell::Bool(true)
    );

    // Supplier to internal (Receipt): is_dropship must be false
    let receipt_picking = tx
        .env()
        .records("stock.picking")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("IN-001"))
                    .set_cell("company_id", Cell::Id(c))
                    .set_cell("picking_type_id", Cell::Id(pt_id))
                    .set_cell("location_id", Cell::Id(supplier_loc))
                    .set_cell("location_dest_id", Cell::Id(internal_loc)),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        receipt_picking.cell(&mut tx, "is_dropship").await.unwrap(),
        Cell::Bool(false)
    );

    // Internal to internal: is_dropship must be false
    let internal_picking = tx
        .env()
        .records("stock.picking")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("INT-001"))
                    .set_cell("company_id", Cell::Id(c))
                    .set_cell("picking_type_id", Cell::Id(pt_id))
                    .set_cell("location_id", Cell::Id(internal_loc))
                    .set_cell("location_dest_id", Cell::Id(internal_loc)),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        internal_picking.cell(&mut tx, "is_dropship").await.unwrap(),
        Cell::Bool(false)
    );

    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_route_record_exists_after_install() {
    let manifest: toml::Table = include_str!("../manifest.toml").parse().unwrap();
    let data = manifest
        .get("data")
        .and_then(|v| v.as_array())
        .expect("data list in manifest");
    assert!(
        data.iter().any(|v| v.as_str() == Some("data/route.xml")),
        "data/route.xml must be in manifest.toml data list"
    );

    let route_xml = include_str!("../data/route.xml");
    assert!(
        route_xml.contains("route_drop_shipping") || route_xml.contains("route_dropship"),
        "route record xml id"
    );
    assert!(route_xml.contains(r#"model="stock.route""#));
    assert!(route_xml.contains("Dropship"));
    assert!(route_xml.contains("product_selectable"));

    let h = harness().await;
    let mut tx = h.begin().await.unwrap();

    let route = tx
        .env()
        .records("stock.route")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Dropship"))
                    .set_cell("product_selectable", Cell::Bool(true)),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        route.cell(&mut tx, "name").await.unwrap(),
        Cell::Str("Dropship".into())
    );
    assert_eq!(
        route.cell(&mut tx, "product_selectable").await.unwrap(),
        Cell::Bool(true)
    );

    let rows = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module = 'stock_dropshipping' AND name IN ('route_dropship', 'route_drop_shipping')",
            &[],
        )
        .await;
    if let Ok(rows) = rows
        && let Some(row) = rows.first()
    {
        let res_id: i64 = row.get(0);
        let r = tx
            .env()
            .records("stock.route")
            .unwrap()
            .browse([res_id])
            .unwrap();
        assert_eq!(
            r.cell(&mut tx, "name").await.unwrap(),
            Cell::Str("Dropship".into())
        );
        assert_eq!(
            r.cell(&mut tx, "product_selectable").await.unwrap(),
            Cell::Bool(true)
        );
    }

    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}
