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

// Stored actions carry admitted expression documents; JSON2 search accepts their evaluated values.
fn action_value(document: &serde_json::Value, absent: serde_json::Value) -> serde_json::Value {
    use erp_expr::{EvalContext, ExprValue, eval_ast, wire::ValidatedAst};
    fn json(value: &ExprValue) -> serde_json::Value {
        match value {
            ExprValue::None => serde_json::Value::Null,
            ExprValue::Bool(value) => serde_json::json!(value),
            ExprValue::Int(value) => serde_json::json!(value),
            ExprValue::Str(value) => serde_json::json!(value),
            ExprValue::List(values) | ExprValue::Tuple(values) => values.iter().map(json).collect(),
            ExprValue::Dict(values) => serde_json::Value::Object(
                values
                    .iter()
                    .map(|(key, value)| {
                        let ExprValue::Str(key) = key else {
                            panic!("action context key must be text")
                        };
                        (key.to_string(), json(value))
                    })
                    .collect(),
            ),
            other => panic!("unsupported static action value: {other:?}"),
        }
    }
    if document.is_null() {
        return absent;
    }
    let ast = ValidatedAst::from_json(&serde_json::to_vec(document).unwrap()).unwrap();
    json(&eval_ast(ast.as_expr(), &EvalContext::standard()).unwrap())
}

async fn identity(fixture: &NativeFixture, module: &str, name: &str) -> i64 {
    fixture
        .client
        .query_one(
            "SELECT res_id FROM ir_model_data WHERE module=$1 AND name=$2",
            &[&module, &name],
        )
        .await
        .unwrap()
        .get(0)
}
fn session(fixture: &NativeFixture, login: &str, company: i64, groups: &[i64]) -> (i64, String) {
    let partner = rpc(
        fixture,
        "res.partner",
        "create",
        serde_json::json!({"vals_list":[{"name":login}]}),
    )[0]
    .as_i64()
    .unwrap();
    let uid = rpc(fixture,"res.users","create",serde_json::json!({"vals_list":[{
        "name":login,"login":login,"password":"fixture-ai-reader-password","partner_id":partner,
        "company_id":company,"company_ids":[[6,0,[company]]],"group_ids":[[6,0,groups]],"lang":"en_US","tz":"UTC"
    }]}))[0].as_i64().unwrap();
    let login_page = request(fixture.port, "GET", "/erp/login", &[], "");
    let prefs = cookie(&login_page, "prefs");
    let csrf = embedded(&login_page, "erp-login");
    let logged = request(
        fixture.port,
        "POST",
        "/erp/login",
        &[
            ("cookie", &prefs),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        &format!(
            "db={}&login={login}&password=fixture-ai-reader-password&csrf_token={}",
            fixture.db.name(),
            encoded(csrf["csrf"].as_str().unwrap().as_bytes())
        ),
    );
    assert_eq!(logged.status, 303, "fixture subject login failed");
    (uid, format!("{prefs}; {}", cookie(&logged, "session_id")))
}
fn denied(reply: Reply, message: &str) {
    assert_eq!(reply.status, 403);
    let error: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(error["name"], "erp.exceptions.AccessError");
    assert_eq!(error["message"], message);
}
async fn check_subjects(fixture: &NativeFixture, provider: i64) {
    let company = identity(fixture, "base", "main_company").await;
    let user_group = identity(fixture, "base", "group_user").await;
    let system_group = identity(fixture, "base", "group_system").await;
    let (_, cookies) = session(fixture, "ai-reader", company, &[user_group]);
    let panel = json_post(
        fixture.port,
        &cookies,
        "/erp/json/2/ir.config.setting/erp_describe",
        r#"{"scope":{"kind":"global"}}"#,
    );
    assert_eq!(panel.status, 200);
    let panel: serde_json::Value = serde_json::from_str(&panel.body).unwrap();
    assert!(
        panel["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| !item["key"].as_str().unwrap().starts_with("ai_base."))
    );
    for name in ["provider_action", "route_action"] {
        let action = identity(fixture, "ai_base", name).await;
        denied(
            json_post(
                fixture.port,
                &cookies,
                "/erp/json/2/ir.actions.act_window/erp_load_web_action",
                &serde_json::json!({"ids":[action]}).to_string(),
            ),
            "You may not read this window action.",
        );
    }
    let read = json_post(
        fixture.port,
        &cookies,
        "/erp/json/2/ai.provider/read",
        &serde_json::json!({"ids":[provider],"fields":["name"]}).to_string(),
    );
    assert_eq!(read.status, 200);
    denied(
        json_post(
            fixture.port,
            &cookies,
            "/erp/json/2/ai.provider/write",
            &serde_json::json!({"ids":[provider],"vals":{"region":"refused"}}).to_string(),
        ),
        "Your access rights do not let you modify 'AI Provider' (ai.provider) records.\n\nGroups that may do this:\n\t- Settings Administration\n\nAsk an administrator if you need this access.",
    );
    let currency: i64 = fixture
        .client
        .query_one(
            "SELECT currency_id FROM res_company WHERE id=$1",
            &[&company],
        )
        .await
        .unwrap()
        .get(0);
    let foreign = rpc(
        fixture,
        "res.company",
        "create",
        serde_json::json!({"vals_list":[{"name":"AI foreign company","currency_id":currency}]}),
    )[0]
    .as_i64()
    .unwrap();
    let (uid, cookies) = session(
        fixture,
        "ai-foreign-operator",
        foreign,
        &[user_group, system_group],
    );
    let route = identity(fixture, "ai_base", "route_assistant_main").await;
    denied(
        json_post(
            fixture.port,
            &cookies,
            "/erp/json/2/ai.route/read",
            &serde_json::json!({"ids":[route],"fields":["name"]}).to_string(),
        ),
        &format!(
            "Access rules protect some of these records.\n\nai-foreign-operator (id={uid}) has no 'read' access to:\n- AI Route (ai.route)\n\nAsk an administrator if you need access to these records.\n\nThe record belongs to a company outside your allowed companies."
        ),
    );
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
    let panel = rpc(
        &fixture,
        "ir.config.setting",
        "erp_describe",
        serde_json::json!({"scope":{"kind":"global"}}),
    );
    let links = panel["items"].as_array().unwrap();
    for key in [
        "ai_base.providers",
        "ai_base.routes",
        "ai_base.budgets",
        "ai_base.usage",
    ] {
        let item = links.iter().find(|item| item["key"] == key).unwrap();
        assert_eq!(item["block"], "AI");
        assert_eq!(item["target"]["kind"], "menu");
        assert!(item["target"]["id"].as_i64().unwrap() > 0);
    }
    for (model, action_name) in [
        ("ai.provider", "provider_action"),
        ("ai.route", "route_action"),
    ] {
        let action: i64 = fixture
            .client
            .query_one(
                "SELECT res_id FROM ir_model_data WHERE module='ai_base' AND name=$1",
                &[&action_name],
            )
            .await
            .unwrap()
            .get(0);
        let loaded_action = rpc(
            &fixture,
            "ir.actions.act_window",
            "erp_load_web_action",
            serde_json::json!({"ids":[action]}),
        );
        let listed = rpc(
            &fixture,
            model,
            "web_search_read",
            serde_json::json!({"domain":action_value(&loaded_action["domain"],serde_json::json!([])),"context":action_value(&loaded_action["context"],serde_json::json!({})),
                "specification":{"id":{},"name":{}},"limit":10}),
        );
        assert_eq!(listed["records"].as_array().unwrap().len(), 1, "{listed}");
        let specification = if model == "ai.provider" {
            serde_json::json!({"name":{},"family":{},"base_url":{},"active":{},"allowed_models":{},"model_limit":{},"region":{},"no_training":{},"terms_reference":{}})
        } else {
            serde_json::json!({"name":{},"company_id":{"fields":{"display_name":{}}},"task_kind":{},"provider_id":{"fields":{"display_name":{}}},"model":{}})
        };
        let new_record = rpc(
            &fixture,
            model,
            "onchange",
            serde_json::json!({
                "ids":[],"values":{},"field_names":[],"fields_spec":specification
            }),
        );
        assert!(new_record["value"].is_object(), "{new_record}");
        for mobile in [false, true] {
            let views = rpc(
                &fixture,
                model,
                "get_views",
                serde_json::json!({"wire_major":1,"views":[[null,"list"],[null,"form"]],"options":{"mobile":mobile}}),
            );
            assert!(views["notices"].as_array().unwrap().is_empty(), "{views}");
            assert_eq!(views["views"].as_array().unwrap().len(), 2);
            assert!(!views.to_string().contains(key.to_str().unwrap()));
            if model == "ai.provider" {
                assert!(views.to_string().contains("ai_base.model_list"));
            }
        }
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
    check_subjects(&fixture, id).await;
    let saved = rpc(
        &fixture,
        "ai.provider",
        "web_save",
        serde_json::json!({"ids":[id],"vals":{"region":"operator-selected-region","allowed_models":["local-model","provider/new-model"]},
            "specification":{"name":{},"region":{},"allowed_models":{}}}),
    );
    assert_eq!(
        saved[0]["allowed_models"],
        serde_json::json!(["local-model", "provider/new-model"])
    );
    let refused = json_post(fixture.port, &fixture.cookies, "/erp/json/2/ai.provider/web_save",
        &serde_json::json!({"ids":[id],"vals":{"allowed_models":["two words"]},"specification":{"allowed_models":{}}}).to_string());
    assert_eq!(refused.status, 422, "{}", refused.body);
    let error: serde_json::Value = serde_json::from_str(&refused.body).unwrap();
    assert_eq!(error["name"], "erp.exceptions.ValidationError");
    assert_eq!(
        error["message"],
        "ai.provider.allowed_models must contain 1..=model_limit unique nonempty ASCII model identifiers of at most 1024 bytes"
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
