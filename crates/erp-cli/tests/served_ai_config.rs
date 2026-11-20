// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AI configuration admission and secrecy; all credentials are synthetic fixtures.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration fixture assertions"
)]
#[path = "support/native_http.rs"]
mod native_http;
use native_http::*;
use std::{fs, os::unix::fs::PermissionsExt, process::Command, time::Duration};

const MARKER: &str = "synthetic-ai-secret-never-on-wire";
fn lifecycle(fixture: &NativeFixture, path: &str) {
    let reply = request_within(
        fixture.port,
        "POST",
        path,
        &[
            ("cookie", &fixture.cookies),
            ("content-type", "application/json"),
            ("sec-fetch-site", "same-origin"),
        ],
        r#"{"modules":["ai_base"]}"#,
        Duration::from_secs(120),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(!reply.body.contains(MARKER));
}
fn rpc(
    fixture: &NativeFixture,
    model: &str,
    method: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let reply = json_post(
        fixture.port,
        &fixture.cookies,
        &format!("/erp/json/2/{model}/{method}"),
        &body.to_string(),
    );
    assert_eq!(reply.status, 200, "{model}.{method}: {}", reply.body);
    assert!(!reply.body.contains(MARKER));
    serde_json::from_str(&reply.body).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn served_ai_configuration_is_installable_and_never_returns_credentials() {
    let keys = tempfile::tempdir().unwrap();
    let key = keys.path().join("fixture.key");
    private(&key, MARKER);
    let extra = format!("\n[ai.provider.local]\nkey_file='{}'\n", key.display());
    let fixture = NativeFixture::start_with_config(&extra).await;
    assert_eq!(
        state_of(&catalogue(fixture.port, &fixture.cookies), "ai_base").as_deref(),
        Some("uninstalled")
    );
    lifecycle(&fixture, "/erp/module/install");
    assert_eq!(
        state_of(&catalogue(fixture.port, &fixture.cookies), "ai_base").as_deref(),
        Some("installed")
    );
    for model in ["ai.provider", "ai.route"] {
        let fields = rpc(&fixture, model, "fields_get", serde_json::json!({}));
        for forbidden in ["key", "key_file", "api_key", "credentials"] {
            assert!(
                fields.get(forbidden).is_none(),
                "{model} exposes {forbidden}"
            );
        }
        let rows = rpc(
            &fixture,
            model,
            "search_read",
            serde_json::json!({"domain":[],"limit":10,"context":{"active_test":false}}),
        );
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert!(!rows.to_string().contains(key.to_str().unwrap()));
    }
    let providers = rpc(
        &fixture,
        "ai.provider",
        "search_read",
        serde_json::json!({"domain":[],"fields":["id","name","active"],"context":{"active_test":false}}),
    );
    let id = providers[0]["id"].as_i64().unwrap();
    assert_eq!(providers[0]["active"], false);
    rpc(
        &fixture,
        "ai.provider",
        "write",
        serde_json::json!({"ids":[id],"vals":{"region":"operator-selected-region"}}),
    );
    lifecycle(&fixture, "/erp/module/upgrade");
    let providers = rpc(
        &fixture,
        "ai.provider",
        "search_read",
        serde_json::json!({"domain":[],"fields":["id","region"],"context":{"active_test":false}}),
    );
    assert_eq!(providers.as_array().unwrap().len(), 1);
    assert_eq!(providers[0]["id"], id);
    assert_eq!(providers[0]["region"], "operator-selected-region");
    for table in ["ai_provider", "ai_route"] {
        // Both identifiers are fixed test-owned model table names.
        for row in fixture
            .client
            .query(&format!("SELECT to_jsonb(r)::text FROM {table} r"), &[])
            .await
            .unwrap()
        {
            let value: String = row.get(0);
            assert!(!value.contains(MARKER));
            assert!(!value.contains(key.to_str().unwrap()));
        }
    }
    assert!(
        !fs::read_to_string(&fixture.stderr_path)
            .unwrap()
            .contains(MARKER)
    );
    // A second real process must reject an insecure key before opening its listener.
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_erp"))
        .args(["server", "--config"])
        .arg(fixture.dir.path().join("server.toml"))
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let error = String::from_utf8(refused.stderr).unwrap();
    assert!(
        error.contains(&format!(
            "AI provider 'local' key file '{}': mode 0644 must be 0600",
            key.display()
        )),
        "{error}"
    );
    assert!(!error.contains(MARKER));
    assert!(!String::from_utf8(refused.stdout).unwrap().contains(MARKER));
    fixture.cleanup().await;
}
