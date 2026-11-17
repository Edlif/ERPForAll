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
