// SPDX-License-Identifier: MIT OR Apache-2.0
//! Owned model slots a WebAssembly package fills: business keys, `parent_store` and field groups
//! (WASM.MODEL-DECL). Pure: no database.
#![allow(clippy::unwrap_used, reason = "tests")]

use erp_fields::ErrorKind;
use erp_registry::{
    AddonDescriptor, FieldDef, KeyDef, KindDef, MAX_NAME_BYTES, MAX_TABLE_OBJECTS_PER_MODEL,
    ModelDef, Registry, RegistryBuilder, TableObjectKind,
};

static OWNER: AddonDescriptor = AddonDescriptor {
    name: "decl_owner",
    depends: &["base"],
    exact_only: false,
};
static EXT: AddonDescriptor = AddonDescriptor {
    name: "decl_ext",
    depends: &["decl_owner"],
    exact_only: false,
};

fn char_field(name: &str) -> FieldDef {
    FieldDef::new(name, KindDef::Char { size: None })
}

fn build(defs: &[ModelDef]) -> erp_fields::Result<Registry> {
    let decls = defs
        .iter()
        .map(|def| def.to_decl())
        .collect::<erp_fields::Result<Vec<_>>>()?;
    RegistryBuilder::new([&OWNER, &EXT])
        .linked(false)
        .with_decls(decls)
        .build()
}

fn asset(key: KeyDef) -> ModelDef {
    ModelDef::new("decl_owner", "decl.asset")
        .field(char_field("serial_no"))
        .field(char_field("site"))
        .key(key)
}

#[test]
fn owned_key_composes_into_a_business_key() {
    let registry = build(&[asset(
        KeyDef::new("_serial_uniq", "The serial number must be unique.")
            .fields(["serial_no", "site"])
            .nulls_distinct(true),
    )])
    .unwrap();
    let model = registry.model("decl.asset").unwrap();
    let key = model.business_key("_serial_uniq").unwrap();
    assert_eq!(
        key.fields,
        vec![
            model.field_id("serial_no").unwrap(),
            model.field_id("site").unwrap()
        ]
    );
    assert!(key.nulls_distinct);
    assert_eq!(key.message, "The serial number must be unique.");
    assert!(
        model
            .table_objects
            .iter()
            .any(|o| { o.name == "_serial_uniq" && matches!(o.kind, TableObjectKind::Key(_)) })
    );
    // without the flag the key keeps the NULLS NOT DISTINCT default
    let strict = build(&[asset(
        KeyDef::new("_serial_uniq", "Unique.").fields(["serial_no"]),
    )])
    .unwrap();
    assert!(
        !strict
            .model("decl.asset")
            .unwrap()
            .business_key("_serial_uniq")
            .unwrap()
            .nulls_distinct
    );
}

#[test]
fn owned_key_over_an_unknown_field_is_refused() {
    let err = build(&[asset(KeyDef::new("_k", "m").fields(["nope"]))]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        "key '_k' of model 'decl.asset': unknown field 'nope'"
    );
}

#[test]
fn owned_key_limits_are_checked() {
    let long = "x".repeat(MAX_NAME_BYTES + 1);
    let err = asset(KeyDef::new(long.clone(), "m").fields(["serial_no"]))
        .check_limits()
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        format!(
            "model 'decl.asset': key name is {} bytes long, more than the limit of {MAX_NAME_BYTES}",
            long.len()
        )
    );
    let mut many = ModelDef::new("decl_owner", "decl.many");
    for i in 0..=MAX_TABLE_OBJECTS_PER_MODEL {
        many = many.key(KeyDef::new(format!("_k{i}"), "m").fields(["a"]));
    }
    let err = many.check_limits().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        format!(
            "model 'decl.many': {} exclusions and keys, more than the limit of \
             {MAX_TABLE_OBJECTS_PER_MODEL}",
            MAX_TABLE_OBJECTS_PER_MODEL + 1
        )
    );
}

#[test]
fn keys_distinguish_interned_definitions() {
    let a = asset(KeyDef::new("_serial_uniq", "one").fields(["serial_no"]));
    let b = asset(KeyDef::new("_serial_uniq", "two").fields(["serial_no"]));
    assert!(std::ptr::eq(
        a.to_decl().unwrap(),
        a.clone().to_decl().unwrap()
    ));
    assert!(!std::ptr::eq(a.to_decl().unwrap(), b.to_decl().unwrap()));
}

fn tree(parent_store: bool, indexed_path: bool) -> ModelDef {
    let path = if indexed_path {
        char_field("parent_path").index(Some(erp_fields::IndexKind::Btree))
    } else {
        char_field("parent_path")
    };
    let mut def = ModelDef::new("decl_owner", "decl.node")
        .field(char_field("name"))
        .field(FieldDef::new(
            "parent_id",
            KindDef::Many2one {
                comodel: "decl.node".into(),
                ondelete: None,
            },
        ))
        .field(path);
    def = def.parent_store(parent_store);
    def
}

/// `parent_path` applies)
#[test]
fn owned_parent_store_is_maintained_and_checked() {
    let registry = build(&[tree(true, true)]).unwrap();
    let node = registry.model("decl.node").unwrap();
    assert_eq!(node.parent_path, node.field_id("parent_path"));
    assert!(node.parent_path.is_some());
    let err = build(&[tree(true, false)]).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        "parent_path field on model 'decl.node' should be indexed: #[field(char, index)]"
    );
    let registry = build(&[tree(false, false)]).unwrap();
    assert!(registry.model("decl.node").unwrap().parent_path.is_none());
}

#[test]
fn owned_parent_name_selects_the_parent_field() {
    let def = ModelDef::new("decl_owner", "decl.node")
        .field(char_field("parent_path").index(Some(erp_fields::IndexKind::Btree)))
        .field(FieldDef::new(
            "up_id",
            KindDef::Many2one {
                comodel: "decl.node".into(),
                ondelete: None,
            },
        ))
        .parent_store(true)
        .parent_name("up_id");
    let registry = build(&[def]).unwrap();
    let node = registry.model("decl.node").unwrap();
    assert_eq!(node.parent_name, node.field_id("up_id"));
}

fn owner_with_groups() -> ModelDef {
    ModelDef::new("decl_owner", "decl.item")
        .field(char_field("plain"))
        .field(char_field("hidden").groups("decl_owner.group_x"))
}

#[test]
fn runtime_groups_on_an_own_field_are_kept() {
    let ext = ModelDef::extension("decl_ext", "decl.item")
        .field(char_field("added").groups("decl_ext.group_y"));
    let registry = build(&[owner_with_groups(), ext]).unwrap();
    let item = registry.model("decl.item").unwrap();
    assert_eq!(
        item.field("hidden").unwrap().groups,
        Some("decl_owner.group_x")
    );
    assert_eq!(
        item.field("added").unwrap().groups,
        Some("decl_ext.group_y")
    );
    // the same addon may state it again on its own field in a later declaration
    let again = ModelDef::extension("decl_owner", "decl.item")
        .field(char_field("hidden").groups("decl_owner.group_z"));
    let registry = build(&[owner_with_groups(), again]).unwrap();
    assert_eq!(
        registry
            .model("decl.item")
            .unwrap()
            .field("hidden")
            .unwrap()
            .groups,
        Some("decl_owner.group_z")
    );
}

#[test]
fn runtime_groups_on_a_foreign_field_are_refused() {
    for field in ["plain", "hidden"] {
        let ext = ModelDef::extension("decl_ext", "decl.item")
            .field(char_field(field).groups("decl_ext.group_y"));
        let err = build(&[owner_with_groups(), ext]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Programming);
        assert_eq!(
            err.message(),
            format!(
                "Invalid field groups on 'decl.item.{field}': addon 'decl_ext' sets `groups` on a \
                 field first declared by addon 'decl_owner'; a runtime declaration may set \
                 `groups` only on the fields it introduces"
            )
        );
    }
    // a runtime redeclaration that does not mention groups changes nothing
    let ext = ModelDef::extension("decl_ext", "decl.item").field(char_field("hidden").string("H"));
    let registry = build(&[owner_with_groups(), ext]).unwrap();
    assert_eq!(
        registry
            .model("decl.item")
            .unwrap()
            .field("hidden")
            .unwrap()
            .groups,
        Some("decl_owner.group_x")
    );
}

fn owner_for_redeclare() -> ModelDef {
    ModelDef::new("decl_owner", "decl.item")
        .field(char_field("plain"))
        .field(char_field("hidden").groups("decl_owner.group_x"))
        .field(char_field("tag"))
        .field(FieldDef::new("price", KindDef::Float { digits: None }))
        .field(FieldDef::new(
            "state",
            KindDef::Selection {
                values: Some(vec![("a".into(), "A".into())]),
                add: vec![],
                ondelete: vec![],
            },
        ))
}

fn refusal(def: ModelDef) -> erp_fields::Error {
    build(&[owner_for_redeclare(), def]).unwrap_err()
}

/// under another type, which would have lifted its group)
#[test]
fn runtime_kind_change_of_a_foreign_field_is_refused() {
    for field in ["hidden", "plain"] {
        let err = refusal(
            ModelDef::extension("decl_ext", "decl.item").field(FieldDef::new(field, KindDef::Text)),
        );
        assert_eq!(err.kind(), ErrorKind::Programming);
        assert_eq!(
            err.message(),
            format!(
                "Invalid field type change on 'decl.item.{field}': addon 'decl_ext' redeclares a \
                 field first declared by addon 'decl_owner' with another type; a runtime \
                 declaration may not change the type of a field it did not introduce"
            )
        );
    }
}

#[test]
fn keys_cannot_take_generated_names() {
    for (name, full) in [
        ("__parent_path_c_index", "decl_node__parent_path_c_index"),
        ("_id_seq", "decl_node_id_seq"),
    ] {
        let mut def = tree(true, true).key(KeyDef::new(name, "m").fields(["name"]));
        def.file = "wasm/decl_owner.wasm".into();
        let err = build(&[def]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Programming, "{name}");
        assert_eq!(
            err.message(),
            format!(
                "SQL object '{full}' of model 'decl.node' (addon 'decl_owner', \
                 wasm/decl_owner.wasm:0) has a name the ORM or PostgreSQL generates for a primary \
                 key, foreign key, not-null constraint, id sequence, parent path index or field index; choose another name"
            )
        );
    }
}
