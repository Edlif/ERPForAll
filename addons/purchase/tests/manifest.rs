// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `purchase` manifest: the depends of the manifest and of the addon declaration agree, the
//! security groups load first and the access rows last (1), and every data file exists.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

fn list(manifest: &toml::Table, key: &str) -> Vec<String> {
    manifest[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is a list"))
        .iter()
        .map(|v| v.as_str().expect("entries are strings").to_owned())
        .collect()
}

#[test]
fn manifest_matches_the_declaration_and_keeps_the_data_order() {
    let manifest: toml::Table = include_str!("../manifest.toml").parse().unwrap();
    assert_eq!(list(&manifest, "depends"), ["base", "account"]);
    assert_eq!(erp_addon_purchase::ADDON.depends, ["base", "account"]);
    assert_eq!(manifest["application"].as_bool(), Some(true));
    assert!(
        manifest.get("auto_install").is_none(),
        "purchase is never auto-installed"
    );
    let data = list(&manifest, "data");
    assert_eq!(
        data.first().map(String::as_str),
        Some("security/purchase_security.xml")
    );
    assert_eq!(
        data.last().map(String::as_str),
        Some("security/ir.access.csv")
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in &data {
        assert!(root.join(file).is_file(), "{file} does not exist");
    }
}

/// `purchase_stock` depends on `purchase` and installs by itself once both its
/// dependencies are installed.
#[test]
fn purchase_stock_depends_on_purchase() {
    let manifest: toml::Table = include_str!("../../purchase_stock/manifest.toml")
        .parse()
        .unwrap();
    assert_eq!(list(&manifest, "depends"), ["stock_account", "purchase"]);
    assert_eq!(manifest["auto_install"].as_bool(), Some(true));
}
