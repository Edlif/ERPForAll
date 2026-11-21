// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit compiled client acceptance against a disposable native server.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture assertions"
)]
#[path = "support/native_http.rs"]
mod native_http;
use native_http::*;
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compiled_ai_settings_edit_save_discard_and_new_forms() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    // Full CI builds the compiler in its web stage; a focused run prepares that binary first.
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let compiler = root.join(target).join("debug/erp-ui-compile");
    assert!(
        compiler.is_file(),
        "build erp-ui-compiler-cli --bin erp-ui-compile on the build host before this focused test"
    );
    // Do not overwrite another browser fixture's assets in a concurrent full test run.
    let assets = tempfile::tempdir().unwrap();
    let built = Command::new("bash")
        .current_dir(root)
        .env("ERP_UI_COMPILER", &compiler)
        .env("ERP_UI_OUTPUT_DIR", assets.path())
        .args(["-c", "set -e; scripts/web-toolchain.sh check; toolchain_env=$(scripts/web-toolchain.sh env); eval \"$toolchain_env\"; exec node web/scripts/build-human-ui.mjs --addons web,ai_base"])
        .status().unwrap();
    assert!(
        built.success(),
        "compiled AI browser assets could not be built"
    );
    let fixture = NativeFixture::start_with_assets(|_| {}, Some(assets.path())).await;
    // The compiled shell includes the activity menu; install its real backing addon too.
    // Base-only AI installation remains covered by served_ai_config.
    let installed = json_post(
        fixture.port,
        &fixture.cookies,
        "/erp/module/install",
        r#"{"modules":["mail","ai_base"]}"#,
    );
    assert_eq!(installed.status, 200, "{}", installed.body);
    let rows = fixture.client.query("SELECT module,name,res_id FROM ir_model_data WHERE (module='base' AND name IN ('settings_action','settings_menu')) OR (module='ai_base' AND name IN ('provider_action','provider_menu','route_action','route_menu','budget_action','budget_menu','usage_action','usage_menu'))", &[]).await.unwrap();
    let ids: serde_json::Map<String, serde_json::Value> = rows
        .iter()
        .map(|row| {
            (
                format!("{}.{}", row.get::<_, String>(0), row.get::<_, String>(1)),
                serde_json::json!(row.get::<_, i64>(2)),
            )
        })
        .collect();
    fixture.client.execute(
        "INSERT INTO ai_budget(name,company_id,provider_id,model,period,token_limit,cost_limit,input_rate,output_rate)
         SELECT 'Retail assistant',(SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'),
                id,'local-model','month',10000,5000000000,1000,2000 FROM ai_provider WHERE name='local'",
        &[],
    ).await.unwrap();
    let mut child = Command::new("bash")
        .current_dir(root)
        .args([
            "-c",
            "set -e; scripts/web-toolchain.sh check; toolchain_env=$(scripts/web-toolchain.sh env); eval \"$toolchain_env\"; exec node \"$@\"",
            "ai-browser",
        ])
        .arg(root.join("web/scripts/browser-ai-configuration.mjs"))
        .env(
            "AI_BROWSER_OUTPUT",
            std::env::var_os("AI_BROWSER_OUTPUT")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| root.join("logs/ai-configuration-browser")),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(serde_json::json!({"origin":format!("http://127.0.0.1:{}",fixture.port),"cookies":fixture.cookies,"ids":ids}).to_string().as_bytes()).unwrap();
    let result = child.wait().unwrap();
    fixture.cleanup().await;
    assert!(
        result.success(),
        "compiled AI Settings browser acceptance failed"
    );
}
