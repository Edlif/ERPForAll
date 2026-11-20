// SPDX-License-Identifier: MIT OR Apache-2.0
//! Core dependencies must never acquire a path back to the optional AI addon.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

#[test]
fn core_crates_have_no_runtime_or_build_path_to_ai_addon() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked", "--offline"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let names: BTreeMap<&str, &str> = packages
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    let nodes: BTreeMap<&str, &serde_json::Value> = metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| (node["id"].as_str().unwrap(), node))
        .collect();
    for core in ["erp-orm", "erp-rpc", "erp-registry", "erp-server"] {
        let start = packages
            .iter()
            .find(|p| p["name"].as_str() == Some(core))
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let mut pending = vec![start];
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            assert_ne!(
                names[id], "erp-addon-ai-base",
                "{core} reaches AI addon via {id}"
            );
            for dep in nodes[id]["deps"].as_array().unwrap() {
                if dep["dep_kinds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|kind| kind["kind"].is_null() || kind["kind"].as_str() == Some("build"))
                {
                    pending.push(dep["pkg"].as_str().unwrap());
                }
            }
        }
    }
}
