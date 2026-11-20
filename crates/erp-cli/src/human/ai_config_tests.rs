// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exercise the configuration entry used by the real `erp server` command, without a database.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn server_config_entry_validates_ai_key_files() {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in [
        (
            "runtime-url",
            "postgresql://runtime@127.0.0.1:5548/ai_fixture",
        ),
        ("owner-url", "postgresql://owner@127.0.0.1:5548/ai_fixture"),
        ("fixture.key", "synthetic-ai-fixture"),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let path = dir.path().join("server.toml");
    let base = "database='ai_fixture'\ndatabase_url_file='runtime-url'\nowner_database_url_file='owner-url'\nbind='127.0.0.1:18069'\npublic_origin='http://127.0.0.1:18069'\nproxy_mode='direct'\naddons_path=['addons']\nassets_manifest='manifest.json'\n";
    fs::write(
        &path,
        format!("{base}[ai.provider.fixture]\nkey_file='fixture.key'\n"),
    )
    .unwrap();
    let loaded = super::config(&path).unwrap();
    assert_eq!(
        loaded.ai().unwrap()["provider"]["fixture"]["key_file"].as_str(),
        Some("fixture.key")
    );
    assert!(!format!("{loaded:?}").contains("synthetic-ai-fixture"));
    fs::set_permissions(
        dir.path().join("fixture.key"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let error = super::config(&path).unwrap_err();
    assert!(matches!(error, crate::CliError::Execution(_)));
    assert_eq!(
        error.to_string(),
        format!(
            "Execution error: AI provider 'fixture' key file '{}': mode 0644 must be 0600",
            dir.path().join("fixture.key").display()
        )
    );
    // No AI table is optional, not an implicit provider or an implicit credential.
    fs::write(&path, base).unwrap();
    assert!(super::config(&path).unwrap().ai().is_none());
    fs::write(
        &path,
        format!("{base}[ai.provider.fixture]\nkey='synthetic-ai-fixture'\n"),
    )
    .unwrap();
    let error = super::config(&path).unwrap_err();
    assert!(matches!(error, crate::CliError::Execution(_)));
    assert_eq!(
        error.to_string(),
        "Execution error: invalid [ai] configuration: expected max_providers, max_key_bytes and provider.<name>.key_file only"
    );
}
