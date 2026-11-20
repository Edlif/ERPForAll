// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Sales application on a served node: the real `erp` binary installs `sale_management`
//! through the route the Addons page calls, with `sale` and its dependencies and their default
//! data, the Sales menu shows, and every Sales screen opens through the web client's metadata
//! path (the order form with its line sub-list included).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "integration fixture assertions and shared helpers"
)]
#[path = "support/actions_open.rs"]
mod actions_open;
#[path = "support/install_defaults.rs"]
mod install_defaults;
#[path = "support/native_http.rs"]
mod native_http;

use actions_open::assert_actions_open;
use native_http::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn served_node_installs_the_sales_application_and_opens_its_screens() {
    let fixture = NativeFixture::start().await;
    let start = catalogue(fixture.port, &fixture.cookies);
    assert_eq!(
        state_of(&start, "sale_management").as_deref(),
        Some("uninstalled")
    );
    let reply = request_within(
        fixture.port,
        "POST",
        "/erp/module/install",
        &[
            ("cookie", &fixture.cookies),
            ("content-type", "application/json"),
            ("sec-fetch-site", "same-origin"),
        ],
        r#"{"modules":["sale_management"]}"#,
        std::time::Duration::from_secs(600),
    );
    assert_eq!(reply.status, 200, "install: {}", reply.body);
    let after = catalogue(fixture.port, &fixture.cookies);
    for name in ["sales_team", "account", "sale", "sale_management"] {
        assert_eq!(
            state_of(&after, name).as_deref(),
            Some("installed"),
            "{name}"
        );
    }
    for (sql, expected) in install_defaults::SALE {
        let rows: Vec<String> = fixture
            .client
            .query(*sql, &[])
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect();
        assert_eq!(rows, *expected, "{sql}");
    }
    let root: bool = fixture
        .client
        .query_one(
            "SELECT m.active FROM ir_ui_menu m JOIN ir_model_data d \
               ON d.model = 'ir.ui.menu' AND d.res_id = m.id \
             WHERE d.module = 'sale' AND d.name = 'sale_menu_root'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        root,
        "the Sales menu shows once the application is installed"
    );
    assert_actions_open(&fixture, &["sale", "sales_team"]).await;
    fixture.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sales_bridge_adds_the_warehouse_picker_to_the_served_quotation_form() {
    let fixture = NativeFixture::start().await;
    for (modules, has_picker) in [
        (r#"{"modules":["sale_management"]}"#, false),
        (r#"{"modules":["sale_stock"]}"#, true),
    ] {
        let reply = request_within(
            fixture.port,
            "POST",
            "/erp/module/install",
            &[
                ("cookie", &fixture.cookies),
                ("content-type", "application/json"),
                ("sec-fetch-site", "same-origin"),
            ],
            modules,
            std::time::Duration::from_secs(600),
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        let action: i64 = fixture
            .client
            .query_one(
                "SELECT res_id::int8 FROM ir_model_data WHERE module='sale' AND name='action_quotations'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        for mobile in [false, true] {
            let body = serde_json::json!({
                "ids": [action], "wire_major": 1, "options": {"mobile": mobile}
            });
            let reply = json_post(
                fixture.port,
                &fixture.cookies,
                "/erp/json/2/ir.actions.act_window/erp_load_web_action_views",
                &body.to_string(),
            );
            assert_eq!(reply.status, 200, "{}", reply.body);
            let bundle: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
            let form = bundle["views"]["views"]
                .as_array()
                .unwrap()
                .iter()
                .find(|view| view["type"] == "form")
                .unwrap();
            let picker = form["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["field"]["name"] == "warehouse_id");
            assert_eq!(picker.is_some(), has_picker, "mobile={mobile}");
            if let Some(picker) = picker {
                assert_eq!(picker["label"], "Warehouse");
                assert!(picker["options"]["domain"].is_object());
                assert!(picker["modifiers"]["readonly"].is_object());
            }
        }
    }
    fixture.cleanup().await;
}
