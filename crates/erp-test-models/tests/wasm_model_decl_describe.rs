// SPDX-License-Identifier: MIT OR Apache-2.0
//! The package reading of `describe()` for field attributes, keys and `parent_store` matches what
//! the native macro declared, with the macro's own token spacing (WASM.MODEL-DECL,
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

use erp_fields::serde_json;
use erp_modules::Manifest;
use erp_modules::wasm_lifecycle::models_from_describe;
use erp_registry::{Overlay, Registry, RegistryBuilder};
use erp_test_models::test_wasm_decl;

fn manifest() -> Manifest {
    Manifest::from_toml(
        "test_wasm_decl",
        "name = 'Decl fixture'\ndepends = ['base']\n[wasm]\nsdk_version = '1.0'\n",
    )
    .unwrap()
}

/// The fixture's own `describe()` entries, as a package would carry them.
fn described() -> serde_json::Value {
    let mut described: serde_json::Value = serde_json::from_str(&erp_sdk::describe()).unwrap();
    let entries = described["contributions"].as_array_mut().unwrap();
    entries.retain(|entry| entry["addon"] == "test_wasm_decl");
    for entry in entries {
        if let Some(args) = entry["detail"]["args"].as_object_mut() {
            args.remove("addon");
        }
    }
    described
}

fn native() -> Registry {
    RegistryBuilder::new([&erp_addon_base::ADDON, &test_wasm_decl::ADDON])
        .build()
        .unwrap()
}

fn packaged(described: &serde_json::Value) -> erp_fields::Result<Registry> {
    let defs = models_from_describe(&manifest(), &described.to_string()).unwrap();
    let mut overlay = Overlay::new();
    overlay.add_addon_defs(defs)?;
    let descriptor = erp_modules::model_files::addon_descriptor(&manifest()).unwrap();
    let base_decls = erp_registry::MODEL_DECLS
        .iter()
        .filter(|decl| decl.addon == "base");
    RegistryBuilder::new([&erp_addon_base::ADDON, descriptor])
        .linked(false)
        .with_decls(base_decls)
        .overlay(&overlay)
        .build()
}

/// macro text reach the registry as the native macro declared them)
#[test]
fn field_attributes_match_the_native_declaration() {
    let native = native();
    let packaged = packaged(&described()).unwrap();
    let native_model = native.model("test_wasm_decl.asset").unwrap();
    let packaged_model = packaged.model("test_wasm_decl.asset").unwrap();
    for name in [
        "name",
        "label",
        "site",
        "state",
        "amount",
        "rank",
        "ratio",
        "active_flag",
        "note",
        "secret",
    ] {
        let (n, p) = (
            native_model.field(name).unwrap(),
            packaged_model.field(name).unwrap(),
        );
        assert_eq!(p.ty, n.ty, "{name} type");
        assert_eq!(p.index, n.index, "{name} index");
        assert_eq!(p.groups, n.groups, "{name} groups");
        assert_eq!(
            format!("{:?}", p.default),
            format!("{:?}", n.default),
            "{name} default"
        );
    }
    // the fixture really declares each thing (the comparison above is not between two empties)
    let state = packaged_model.field("state").unwrap();
    assert!(format!("{:?}", state.ty).contains("Done"), "{:?}", state.ty);
    assert!(state.default.is_some());
    assert_eq!(
        packaged_model.field("secret").unwrap().groups,
        Some("base.group_system")
    );
    assert!(format!("{:?}", packaged_model.field("amount").unwrap().ty).contains("16"));
    assert_eq!(
        packaged_model.field("label").unwrap().index,
        Some(erp_fields::IndexKind::Trigram)
    );
    assert_eq!(
        packaged_model.field("site").unwrap().index,
        Some(erp_fields::IndexKind::Btree)
    );
}

#[test]
fn keys_match_the_native_declaration() {
    let native = native();
    let packaged = packaged(&described()).unwrap();
    let native_model = native.model("test_wasm_decl.asset").unwrap();
    let packaged_model = packaged.model("test_wasm_decl.asset").unwrap();
    let names = |m: &erp_registry::ModelSpec| {
        m.business_keys()
            .iter()
            .filter(|k| k.name.starts_with("_serial") || k.name.starts_with("_name_site"))
            .map(|k| {
                (
                    k.name,
                    k.fields
                        .iter()
                        .map(|f| m.field_by_id(*f).name)
                        .collect::<Vec<_>>(),
                    k.message,
                    k.nulls_distinct,
                )
            })
            .collect::<Vec<_>>()
    };
    let expected = names(native_model);
    assert_eq!(expected.len(), 2, "{expected:?}");
    assert_eq!(names(packaged_model), expected);
    assert!(expected.iter().any(|k| k.0 == "_serial_uniq" && k.3));
    assert!(expected.iter().any(|k| k.0 == "_name_site_key" && !k.3));
}

#[test]
fn parent_store_matches_the_native_declaration() {
    let native = native();
    let packaged = packaged(&described()).unwrap();
    let node = packaged.model("test_wasm_decl.node").unwrap();
    let native_node = native.model("test_wasm_decl.node").unwrap();
    assert_eq!(
        node.parent_path.map(|f| node.field_by_id(f).name),
        native_node
            .parent_path
            .map(|f| native_node.field_by_id(f).name)
    );
    assert_eq!(
        node.parent_path.map(|f| node.field_by_id(f).name),
        Some("parent_path")
    );
    assert_eq!(
        node.parent_name.map(|f| node.field_by_id(f).name),
        Some("parent_id")
    );
}

/// registry is built, naming the model)
#[test]
fn parent_store_without_an_indexed_path_is_refused() {
    let mut text = described();
    for entry in text["contributions"].as_array_mut().unwrap() {
        if entry["model"] == "test_wasm_decl.node" && entry["kind"] == "model" {
            for field in entry["detail"]["fields"].as_array_mut().unwrap() {
                if field["name"] == "parent_path" {
                    field["attrs"] = serde_json::json!({});
                }
            }
        }
    }
    let error = packaged(&text).unwrap_err();
    assert_eq!(error.kind(), erp_fields::ErrorKind::Programming);
    assert_eq!(
        error.message(),
        "parent_path field on model 'test_wasm_decl.node' should be indexed: \
         #[field(char, index)]"
    );
}
