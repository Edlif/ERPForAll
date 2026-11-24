// SPDX-License-Identifier: MIT OR Apache-2.0
//! Form open time measured in a real browser against an installed native server (sales, purchase, stock and
//! accounting) and the compiled client. A measurement, not a gate: it is ignored in every ordinary run and asserts
//! only that each step reached its page and every request succeeded. The numbers are in the result file the browser
//! script writes (`web/scripts/browser-form-open.mjs`).
//!
//! Run it on a build host, with an optimized build (timings of a debug server say little):
//!
//! ```text
//! cargo build --locked --profile release-tests -p erp-ui-compiler-cli --bin erp-ui-compile
//! FORM_OPEN_OUTPUT=<dir> cargo test --locked --profile release-tests -p erp-cli --test form_open_browser \
//!     -- --ignored --nocapture
//! ```
//!
//! It needs the test database (`DATABASE_URL`), the web toolchain with `pnpm --dir web install` done, and Chrome
//! (`CHROME_BIN`, default `/usr/bin/google-chrome`). `FORM_OPEN_REPS` sets the samples per scenario (default 7),
//! `FORM_OPEN_HOST_LABEL` the host name written into the result, `FORM_OPEN_REVISION` the revision it names.
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

/// Lines of the two orders with lines.
const FIFTY: usize = 50;
const MANY: usize = 500;

fn call(fixture: &NativeFixture, model: &str, method: &str, company: i64, body: Value) -> Value {
    let mut body = body;
    body["context"] = json!({"allowed_company_ids": [company]});
    let reply = native_http::request_within(
        fixture.port,
        "POST",
        &format!("/erp/json/2/{model}/{method}"),
        &[
            ("cookie", &fixture.cookies),
            ("content-type", "application/json"),
            ("sec-fetch-site", "same-origin"),
        ],
        &body.to_string(),
        std::time::Duration::from_secs(300),
    );
    assert_eq!(reply.status, 200, "{model}.{method}: {}", reply.body);
    serde_json::from_str(&reply.body).unwrap()
}

fn create(fixture: &NativeFixture, model: &str, company: i64, vals: Vec<Value>) -> Vec<i64> {
    let ids = call(
        fixture,
        model,
        "create",
        company,
        json!({"vals_list": vals}),
    );
    ids.as_array()
        .expect("create returns record ids")
        .iter()
        .map(|id| id.as_i64().unwrap())
        .collect()
}

fn order_with_lines(
    fixture: &NativeFixture,
    company: i64,
    partner: i64,
    products: &[i64],
    lines: usize,
) -> i64 {
    let order = create(
        fixture,
        "sale.order",
        company,
        vec![json!({"partner_id": partner, "company_id": company})],
    )[0];
    let vals: Vec<Value> = (0..lines)
        .map(|n| json!({"order_id": order, "product_id": products[n % products.len()], "product_uom_qty": 1 + n % 7, "price_unit": 3 + n % 11}))
        .collect();
    for chunk in vals.chunks(100) {
        create(fixture, "sale.order.line", company, chunk.to_vec());
    }
    order
}

#[ignore = "DEFERRED_PERFORMANCE: a browser measurement for a build host, started by hand (see the module documentation)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn form_open_times_in_the_browser() {
    let output = std::env::var_os("FORM_OPEN_OUTPUT")
        .expect("FORM_OPEN_OUTPUT names the directory for the result file");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let erp = Path::new(env!("CARGO_BIN_EXE_erp"));
    let profile = erp
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap()
        .to_owned();
    let compiler = erp.with_file_name("erp-ui-compile");
    assert!(
        compiler.is_file(),
        "build erp-ui-compiler-cli --bin erp-ui-compile with the same profile ({profile}) first"
    );
    let assets = tempfile::tempdir().unwrap();
    let built = Command::new("bash")
        .current_dir(root)
        .args([
            "-c",
            "set -euo pipefail; scripts/web-toolchain.sh check; eval \"$(scripts/web-toolchain.sh env)\"; exec node \"$1\" --addons web,acceptance_notes,reporting,stock_account",
            "form-open-assets",
        ])
        .arg(root.join("web/scripts/build-human-ui.mjs"))
        .env("ERP_UI_COMPILER", &compiler)
        .env("ERP_UI_OUTPUT_DIR", assets.path())
        .status()
        .unwrap();
    assert!(built.success(), "client asset compilation failed");
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
        r#"{"modules":["sale_management","sale_stock","purchase_stock","stock_account"]}"#,
        std::time::Duration::from_secs(900),
    );
    assert_eq!(installed.status, 200, "install: {}", installed.body);
    let xmlid = async |module: &str, name: &str| -> i64 {
        fixture
            .client
            .query_one(
                "SELECT res_id FROM ir_model_data WHERE module=$1 AND name=$2",
                &[&module, &name],
            )
            .await
            .unwrap()
            .get(0)
    };
    let company = xmlid("base", "main_company").await;
    let action = xmlid("sale", "action_quotations").await;
    let partner = create(
        &fixture,
        "res.partner",
        company,
        vec![json!({"name": "Form Open Customer"})],
    )[0];
    let products = create(
        &fixture,
        "product.product",
        company,
        (1..=20)
            .map(|n| json!({"name": format!("Form Open Product {n:02}"), "type": "consu", "invoice_policy": "order", "list_price": n}))
            .collect(),
    );
    let zero = order_with_lines(&fixture, company, partner, &products, 0);
    let fifty = order_with_lines(&fixture, company, partner, &products, FIFTY);
    let many = order_with_lines(&fixture, company, partner, &products, MANY);
    // the least chart a draft invoice needs: an income and a receivable account and a sales journal
    let accounts = create(
        &fixture,
        "account.account",
        company,
        vec![
            json!({"code": "FO400", "name": "Form Open Income", "account_type": "income"}),
            json!({"code": "FO121", "name": "Form Open Receivable", "account_type": "asset_receivable"}),
        ],
    );
    create(
        &fixture,
        "account.journal",
        company,
        vec![
            json!({"name": "Form Open Invoices", "code": "FOI", "type": "sale", "company_id": company, "default_account_id": accounts[0]}),
        ],
    );
    let invoiced = order_with_lines(&fixture, company, partner, &products, 3);
    call(
        &fixture,
        "sale.order",
        "action_confirm",
        company,
        json!({"ids": [invoiced]}),
    );
    let drafted = call(
        &fixture,
        "sale.order",
        "action_create_draft_invoice",
        company,
        json!({"ids": [invoiced]}),
    );
    let invoice = drafted["res_id"]
        .as_i64()
        .unwrap_or_else(|| panic!("the draft invoice action names its invoice: {drafted}"));

    let mut child = Command::new("bash")
        .current_dir(root)
        .args([
            "-c",
            "set -euo pipefail; eval \"$(scripts/web-toolchain.sh env)\"; exec node \"$1\"",
            "form-open-browser",
        ])
        .arg(root.join("web/scripts/browser-form-open.mjs"))
        .env("FORM_OPEN_OUTPUT", &output)
        .env("FORM_OPEN_PROFILE", &profile)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({
                "origin": format!("http://127.0.0.1:{}", fixture.port), "cookies": fixture.cookies,
                "action": action, "invoice": invoice,
                "orders": {"zero": zero, "fifty": fifty, "many": many, "invoiced": invoiced},
                "lines": {"fifty": FIFTY, "many": MANY},
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let status = child.wait().unwrap();
    fixture.cleanup().await;
    assert!(status.success(), "the form open measurement failed");
}
