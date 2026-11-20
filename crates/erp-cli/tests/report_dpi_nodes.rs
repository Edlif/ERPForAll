// SPDX-License-Identifier: MIT OR Apache-2.0
//! Committed barcode settings reach two native server processes without restarting either.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "native HTTP assertions"
)]
#[path = "support/native_http.rs"]
mod native_http;
use native_http::{NativeFixture, json_post, request};
use serde_json::{Value, json};

const KEY: &str = "base.report_barcode_dpi";

fn rpc(port: u16, cookies: &str, path: &str, body: Value) -> Value {
    let reply = json_post(port, cookies, path, &body.to_string());
    assert_eq!(reply.status, 200, "{path}: {}", reply.body);
    serde_json::from_str(&reply.body).unwrap()
}

fn settings(port: u16, cookies: &str) -> Value {
    rpc(
        port,
        cookies,
        "/erp/json/2/ir.config.setting/erp_read_values",
        json!({"scope":{"kind":"global"},"keys":[KEY]}),
    )
}

fn printed_width(port: u16, cookies: &str, path: &str) -> f64 {
    let reply = request(port, "GET", path, &[("cookie", cookies)], "");
    assert_eq!(reply.status, 200, "HTML preparation: {}", reply.body);
    let (_, code) = reply
        .body
        .split_once("aria-label=\"Barcode WH/OUT/00012\"")
        .expect("the real report must contain the requested barcode");
    let (_, width) = code.split_once("width:").unwrap();
    width.split_once("pt").unwrap().0.parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn barcode_dpi_updates_reach_both_print_nodes() {
    let fixture = NativeFixture::start().await;
    let peer = fixture.peer();
    let report = "test_dpi.nodes";
    let arch = r#"<t t-name="test_dpi.nodes"><t t-foreach="docs" t-as="doc"><div><span t-field="doc.name" t-options="{'widget': 'barcode', 'img_style': 'width:300px;height:50px'}"/></div></t></t>"#;
    rpc(
        fixture.port,
        &fixture.cookies,
        "/erp/json/2/ir.ui.view/create",
        json!({"vals_list":[{
            "name":report,"key":report,"type":"qweb","arch_db":arch
        }]}),
    );
    rpc(
        fixture.port,
        &fixture.cookies,
        "/erp/json/2/ir.actions.report/create",
        json!({"vals_list":[{
            "name":"DPI peer sheet","model":"res.partner","report_type":"qweb-html","report_name":report
        }]}),
    );
    let ids = rpc(
        fixture.port,
        &fixture.cookies,
        "/erp/json/2/res.partner/create",
        json!({"vals_list":[{"name":"WH/OUT/00012"}]}),
    );
    let path = format!("/erp/report/html/{report}/{}", ids[0].as_i64().unwrap());
    let ports = [fixture.port, peer.port];
    let initial: Vec<_> = ports
        .iter()
        .map(|port| {
            assert_eq!(settings(*port, &fixture.cookies)["values"][0]["value"], 300);
            printed_width(*port, &fixture.cookies, &path)
        })
        .collect();
    assert_eq!(initial[0], initial[1]);
    for (writer, dpi) in [(fixture.port, 203), (peer.port, 300)] {
        let current = settings(writer, &fixture.cookies);
        rpc(
            writer,
            &fixture.cookies,
            "/erp/json/2/ir.config.setting/erp_write_values",
            json!({
                "scope":{"kind":"global"},"expected_revision":current["revision"],
                "changes":[{"key":KEY,"value":dpi}]
            }),
        );
        let widths: Vec<_> = ports
            .iter()
            .map(|port| {
                let saved = settings(*port, &fixture.cookies);
                assert_eq!(saved["values"][0]["value"], dpi);
                assert_eq!(saved["values"][0]["source"], "stored");
                printed_width(*port, &fixture.cookies, &path)
            })
            .collect();
        assert_eq!(widths[0], widths[1]);
        if dpi == 203 {
            assert_ne!(
                widths[0], initial[0],
                "both warmed nodes must prepare with the new grid"
            );
        } else {
            assert_eq!(widths, initial);
        }
    }
    drop(peer);
    fixture.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn barcode_dpi_settings_browser_smoke() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let fixture = NativeFixture::start().await;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    // This acceptance test is mandatory, including when selected before the web CI steps.
    // Prepare the declared dependencies with the pinned toolchain; missing tools fail loudly.
    let mut child = Command::new("bash")
        .current_dir(root)
        .args([
            "-c",
            "set -euo pipefail; scripts/web-toolchain.sh check; toolchain_env=$(scripts/web-toolchain.sh env); eval \"$toolchain_env\"; (cd web && pnpm install --frozen-lockfile); exec node web/scripts/browser-report-dpi.mjs",
        ])
        .env("REPORT_DPI_OUTPUT", root.join(format!("logs/report-dpi-browser-{}", fixture.port)))
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({
                "origin":format!("http://127.0.0.1:{}", fixture.port),
                "cookies":fixture.cookies
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "browser smoke failed: {status}");
    assert_eq!(
        settings(fixture.port, &fixture.cookies)["values"][0]["value"],
        203
    );
    fixture.cleanup().await;
}
