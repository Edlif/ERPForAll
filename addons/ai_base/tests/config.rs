// SPDX-License-Identifier: MIT OR Apache-2.0
//! Protected AI credential admission; all key bytes below are synthetic fixture data.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
use erp_addon_ai_base::config::{CredentialError, Credentials};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

fn value(text: &str) -> toml::Value {
    toml::from_str(text).unwrap()
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("fixture.key"), "synthetic-fixture-only\n").unwrap();
    fs::set_permissions(
        dir.path().join("fixture.key"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    dir
}
fn config() -> toml::Value {
    value("[provider.fixture]\nkey_file = 'fixture.key'\n")
}

#[test]
fn protected_file_loads_and_public_file_is_refused() {
    let dir = fixture();
    let loaded = Credentials::load(Some(&config()), dir.path()).unwrap();
    loaded.require("fixture").unwrap();
    fs::set_permissions(
        dir.path().join("fixture.key"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let error = Credentials::load(Some(&config()), dir.path()).unwrap_err();
    assert!(matches!(error, CredentialError::File { .. }));
    assert_eq!(
        error.to_string(),
        format!(
            "AI provider 'fixture' key file '{}': mode 0644 must be 0600",
            dir.path().join("fixture.key").display()
        )
    );
}

#[test]
fn debug_and_errors_never_include_file_contents() {
    let dir = fixture();
    let loaded = Credentials::load(Some(&config()), dir.path()).unwrap();
    assert_eq!(format!("{loaded:?}"), "Credentials { providers: 1, .. }");
    fs::write(
        dir.path().join("fixture.key"),
        "synthetic-fixture-only\nextra",
    )
    .unwrap();
    let error = Credentials::load(Some(&config()), dir.path()).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "AI provider 'fixture' key file '{}': expected a nonempty ASCII token within max_key_bytes",
            dir.path().join("fixture.key").display()
        )
    );
}

#[test]
fn absent_credentials_never_fall_back() {
    let dir = fixture();
    let loaded = Credentials::load(Some(&config()), dir.path()).unwrap();
    let error = loaded.require("other").unwrap_err();
    assert!(matches!(error, CredentialError::Unconfigured { .. }));
    assert_eq!(
        error.to_string(),
        "erp.ai.provider_unconfigured: provider 'other'"
    );
    assert_eq!(Credentials::load(None, dir.path()).unwrap().len(), 0);
}

#[test]
fn symlink_directory_and_empty_or_oversized_files_are_refused() {
    let dir = fixture();
    symlink(dir.path().join("fixture.key"), dir.path().join("link.key")).unwrap();
    for name in ["link.key", "missing.key", "."] {
        let raw = value(&format!("[provider.fixture]\nkey_file = '{name}'\n"));
        assert!(matches!(
            Credentials::load(Some(&raw), dir.path()).unwrap_err(),
            CredentialError::File { .. }
        ));
    }
    for bytes in [Vec::new(), vec![b'x'; 17]] {
        fs::write(dir.path().join("fixture.key"), bytes).unwrap();
        let raw = value("max_key_bytes=16\n[provider.fixture]\nkey_file='fixture.key'\n");
        let error = Credentials::load(Some(&raw), dir.path()).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "AI provider 'fixture' key file '{}': expected a nonempty ASCII token within max_key_bytes",
                dir.path().join("fixture.key").display()
            )
        );
    }
}

#[test]
fn malformed_config_is_sanitized_and_bounded() {
    let dir = fixture();
    for text in [
        "unknown='synthetic-fixture-only'",
        "provider='synthetic-fixture-only'",
        "[provider.fixture]\nkey_file=4",
        "[provider.fixture]\nkey_file='fixture.key'\nkey='synthetic-fixture-only'",
    ] {
        let error = Credentials::load(Some(&value(text)), dir.path()).unwrap_err();
        assert!(matches!(error, CredentialError::Configuration));
        assert_eq!(
            error.to_string(),
            "invalid [ai] configuration: expected max_providers, max_key_bytes, max_request_bytes and provider.<name>.key_file only"
        );
    }
    for (text, setting) in [
        ("max_key_bytes=0", "ai.max_key_bytes"),
        ("max_key_bytes=65537", "ai.max_key_bytes"),
        ("max_providers=0", "ai.max_providers"),
        ("max_providers=129", "ai.max_providers"),
    ] {
        let error = Credentials::load(Some(&value(text)), dir.path()).unwrap_err();
        assert!(matches!(error, CredentialError::Limit { .. }));
        let range = if setting.ends_with("max_key_bytes") {
            "1..=65536"
        } else {
            "1..=128"
        };
        assert_eq!(error.to_string(), format!("{setting} must be in {range}"));
    }
    let error = Credentials::load(Some(&value("max_providers=1\n[provider.a]\nkey_file='fixture.key'\n[provider.b]\nkey_file='fixture.key'")), dir.path()).unwrap_err();
    assert_eq!(error.to_string(), "ai.provider exceeds ai.max_providers");
}

#[test]
fn fifo_and_special_permission_bits_are_refused() {
    let dir = fixture();
    let fifo = dir.path().join("pipe.key");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let raw = value("[provider.fixture]\nkey_file='pipe.key'\n");
    let error = Credentials::load(Some(&raw), dir.path()).unwrap_err();
    assert!(matches!(error, CredentialError::File { .. }));
    assert_eq!(
        error.to_string(),
        format!(
            "AI provider 'fixture' key file '{}': must be a regular file",
            fifo.display()
        )
    );
    for mode in [0o400, 0o1600, 0o2600, 0o4600] {
        fs::set_permissions(
            dir.path().join("fixture.key"),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        let error = Credentials::load(Some(&config()), dir.path()).unwrap_err();
        assert!(matches!(error, CredentialError::File { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "AI provider 'fixture' key file '{}': mode {mode:04o} must be 0600",
                dir.path().join("fixture.key").display()
            )
        );
    }
}

#[test]
fn exact_token_and_provider_boundaries_are_enforced() {
    let dir = fixture();
    let raw =
        value("max_key_bytes=1\nmax_providers=1\n[provider.fixture]\nkey_file='fixture.key'\n");
    for bytes in [b"x".as_slice(), b"x\n"] {
        fs::write(dir.path().join("fixture.key"), bytes).unwrap();
        let loaded = Credentials::load(Some(&raw), dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        loaded.require("fixture").unwrap();
    }
    for bytes in [
        b"xx".as_slice(),
        b"\n",
        b" ",
        b"\x7f",
        b"\xff",
        b"x\r\n",
        b"x\n\n",
    ] {
        fs::write(dir.path().join("fixture.key"), bytes).unwrap();
        let error = Credentials::load(Some(&raw), dir.path()).unwrap_err();
        assert!(matches!(error, CredentialError::File { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "AI provider 'fixture' key file '{}': expected a nonempty ASCII token within max_key_bytes",
                dir.path().join("fixture.key").display()
            )
        );
    }
    fs::write(dir.path().join("fixture.key"), "x").unwrap();
    for name in [
        String::new(),
        "a".repeat(64),
        "with space".into(),
        "é".into(),
        "slash/name".into(),
    ] {
        let raw = value(&format!("[provider.'{name}']\nkey_file='fixture.key'\n"));
        let error = Credentials::load(Some(&raw), dir.path()).unwrap_err();
        assert!(matches!(error, CredentialError::ProviderName));
        assert_eq!(
            error.to_string(),
            "ai.provider names must be 1..=63 ASCII letters, digits, underscores or hyphens"
        );
    }
    let name = "a".repeat(63);
    let raw = value(&format!("[provider.'{name}']\nkey_file='fixture.key'\n"));
    Credentials::load(Some(&raw), dir.path())
        .unwrap()
        .require(&name)
        .unwrap();
}

#[test]
fn failed_candidate_preserves_old_registry_and_success_removes_omitted_keys() {
    let dir = fixture();
    let mut current = Credentials::load(Some(&config()), dir.path()).unwrap();
    let raw = value("[provider.a]\nkey_file='fixture.key'\n[provider.z]\nkey_file='missing.key'\n");
    let error = Credentials::load(Some(&raw), dir.path()).unwrap_err();
    assert!(matches!(error, CredentialError::File { .. }));
    assert_eq!(
        error.to_string(),
        format!(
            "AI provider 'z' key file '{}': cannot open protected regular file",
            dir.path().join("missing.key").display()
        )
    );
    current.require("fixture").unwrap();
    assert_eq!(current.len(), 1);
    let raw = value("[provider.replacement]\nkey_file='fixture.key'\n");
    current = Credentials::load(Some(&raw), dir.path()).unwrap();
    current.require("replacement").unwrap();
    let error = current.require("fixture").unwrap_err();
    assert!(matches!(error, CredentialError::Unconfigured { .. }));
    assert_eq!(
        error.to_string(),
        "erp.ai.provider_unconfigured: provider 'fixture'"
    );
}
