// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit compiled-client acceptance against installed Sales and Stock on a native server.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "integration fixture assertions and shared helpers"
)]

#[path = "support/native_http.rs"]
mod native_http;

use native_http::NativeFixture;
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn create(fixture: &NativeFixture, model: &str, company: i64, vals: Value) -> i64 {
    let reply = native_http::json_post(
        fixture.port,
        &fixture.cookies,
        &format!("/erp/json/2/{model}/create"),
        &json!({"vals_list": [vals], "context": {"allowed_company_ids": [company]}}).to_string(),
    );
    assert_eq!(reply.status, 200, "{model} create: {}", reply.body);
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    value[0].as_i64().expect("create returns a record id")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_sales_warehouse_keyboard_save_and_confirm() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    // Full CI's web stage builds the compiler before Rust tests. A focused run builds
    // that same binary once as a prerequisite. Assets stay outside the checkout so
    // parallel lifecycle tests never observe a changing addon source artifact.
    let compiler = Path::new(env!("CARGO_BIN_EXE_erp")).with_file_name("erp-ui-compile");
    assert!(
        compiler.is_file(),
        "build erp-ui-compiler-cli --bin erp-ui-compile in the same target directory first"
    );
    let assets = tempfile::tempdir().unwrap();
    let built = Command::new("bash")
        .current_dir(root)
        .args([
            "-c",
            "set -euo pipefail; scripts/web-toolchain.sh check; eval \"$(scripts/web-toolchain.sh env)\"; exec node \"$1\" --addons web,acceptance_notes,reporting,stock_account",
            "sale-delivery-assets",
        ])
        .arg(root.join("web/scripts/build-human-ui.mjs"))
        .env("ERP_UI_COMPILER", &compiler)
        .env("ERP_UI_OUTPUT_DIR", assets.path())
        .status()
        .unwrap();
    assert!(built.success(), "Sales browser asset compilation failed");
    let output = root
        .join("logs/sale-delivery-browser")
        .join(std::process::id().to_string());
    let fixture = NativeFixture::start_with_assets(|_| {}, Some(assets.path())).await;
    let installed = native_http::request_within(
        fixture.port,
        "POST",
        "/erp/module/install",
        &[
            ("cookie", &fixture.cookies),
            ("content-type", "application/json"),
            ("sec-fetch-site", "same-origin"),
        ],
        r#"{"modules":["sale_management","sale_stock"]}"#,
        std::time::Duration::from_secs(600),
    );
    assert_eq!(installed.status, 200, "install: {}", installed.body);
    let company: i64 = fixture
        .client
        .query_one(
            "SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let action: i64 = fixture
        .client
        .query_one(
            "SELECT res_id FROM ir_model_data WHERE module='sale' AND name='action_quotations'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let warehouse_name = "Browser Shipping Warehouse";
    let warehouse = create(
        &fixture,
        "stock.warehouse",
        company,
        json!({"name": warehouse_name, "code": "BSW", "company_id": company}),
    );
    let partner = create(
        &fixture,
        "res.partner",
        company,
        json!({"name": "Browser Shipping Customer"}),
    );
    let product = create(
        &fixture,
        "product.product",
        company,
        json!({"name": "Browser Shipping Cartons", "type": "consu", "is_storable": true, "invoice_policy": "order"}),
    );
    fixture.client.execute(
        "INSERT INTO stock_quant(company_id,product_id,location_id,quantity,reserved_quantity,in_date)
         SELECT company_id,$1,lot_stock_id,2,0,now() FROM stock_warehouse WHERE id=$2",
        &[&product,&warehouse],
    ).await.unwrap();
    let order = create(
        &fixture,
        "sale.order",
        company,
        json!({"partner_id": partner, "company_id": company}),
    );
    create(
        &fixture,
        "sale.order.line",
        company,
        json!({"order_id": order, "product_id": product, "product_uom_qty": 3, "price_unit": 1}),
    );
    let mut child = Command::new("bash")
        .current_dir(root)
        .args([
            "-c",
            "set -euo pipefail; eval \"$(scripts/web-toolchain.sh env)\"; exec node \"$1\"",
            "sale-delivery-browser",
        ])
        .arg(root.join("web/scripts/browser-sale-delivery.mjs"))
        .env("SALE_DELIVERY_BROWSER_OUTPUT", &output)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(json!({
        "origin": format!("http://127.0.0.1:{}", fixture.port), "cookies": fixture.cookies,
        "action": action, "order": order, "warehouse": warehouse, "warehouseName": warehouse_name
    }).to_string().as_bytes()).unwrap();
    let status = child.wait().unwrap();
    let delivered = fixture.client.query_one(
        "SELECT o.state, o.warehouse_id, (SELECT count(*) FROM sale_stock_delivery_link l WHERE l.order_id = o.id) FROM sale_order o WHERE o.id = $1",
        &[&order]).await.unwrap();
    let shipped = fixture.client.query_one(
        "SELECT m.quantity, (SELECT count(*) FROM stock_picking child WHERE child.backorder_id=m.picking_id)
         FROM stock_move m JOIN sale_stock_delivery_move_link l ON l.move_id=m.id
         JOIN sale_order_line line ON line.id=l.sale_line_id WHERE line.order_id=$1",
        &[&order],
    ).await.unwrap();
    fixture.cleanup().await;
    assert!(
        status.success(),
        "installed Sales browser acceptance failed"
    );
    assert_eq!(
        shipped.get::<_, erp_fields::Decimal>(0),
        erp_fields::Decimal::from(2)
    );
    assert_eq!(shipped.get::<_, i64>(1), 1);
    assert_eq!(delivered.get::<_, String>(0), "sale");
    assert_eq!(delivered.get::<_, Option<i64>>(1), Some(warehouse));
    assert_eq!(
        delivered.get::<_, i64>(2),
        1,
        "confirmation creates exactly one linked transfer"
    );
}
