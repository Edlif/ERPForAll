// SPDX-License-Identifier: MIT OR Apache-2.0
//! `company_default_for = "<model>.<field>"` at registry composition (supplement of
//! 2026-11-15, design): what a
//! valid declaration resolves to and one refusal per wrong declaration. Pure: no database.
#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]

use erp_fields::ErrorKind;
use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl, Registry, RegistryBuilder};

static CO: AddonDescriptor = AddonDescriptor {
    name: "co",
    depends: &["base"],
    exact_only: false,
};

fn leak<T>(items: Vec<T>) -> &'static [T] {
    Vec::leak(items)
}

fn build(decls: Vec<ModelDecl>) -> erp_fields::Result<Registry> {
    let decls: Vec<&'static ModelDecl> = decls
        .into_iter()
        .map(|d| &*Box::leak(Box::new(d)))
        .collect();
    RegistryBuilder::new([&CO]).with_decls(decls).build()
}

fn m(name: &'static str, fields: Vec<FieldDecl>) -> ModelDecl {
    ModelDecl::new("co", name).fields(leak(fields))
}

const STATES: &[(&str, &str)] = &[("a", "A"), ("b", "B")];

/// The target model: one company-dependent field per admitted kind, and fields that must refuse.
fn target() -> ModelDecl {
    m(
        "co.target",
        vec![
            FieldDecl::char("name"),
            FieldDecl::float("credit_limit").company_dependent(true),
            FieldDecl::many2one("partner_id", "co.partner").company_dependent(true),
            FieldDecl::selection("state", STATES).company_dependent(true),
            FieldDecl::boolean("flag").company_dependent(true),
            FieldDecl::integer("count").company_dependent(true),
            FieldDecl::char("label").company_dependent(true),
            FieldDecl::date("day").company_dependent(true),
            FieldDecl::float("plain"),
        ],
    )
}

fn partner() -> ModelDecl {
    m("co.partner", vec![FieldDecl::char("name")])
}

fn other() -> ModelDecl {
    m("co.other", vec![FieldDecl::char("name")])
}

fn company(extra: Vec<FieldDecl>) -> ModelDecl {
    let mut fields = vec![
        FieldDecl::char("name"),
        FieldDecl::many2one("parent_id", "res.company"),
    ];
    fields.extend(extra);
    m("res.company", fields)
}

/// `(kind, message)` of the registry error of a `res.company` with `extra` fields.
fn refusal(extra: Vec<FieldDecl>) -> (ErrorKind, String) {
    refusal_of(vec![company(extra), target(), partner(), other()])
}

fn refusal_of(decls: Vec<ModelDecl>) -> (ErrorKind, String) {
    let err = build(decls).unwrap_err();
    (err.kind(), err.message().to_owned())
}

#[test]
fn a_valid_declaration_resolves_its_target() {
    let r = build(vec![
        company(vec![
            FieldDecl::float("credit_limit").company_default_for("co.target.credit_limit"),
            FieldDecl::many2one("default_partner_id", "co.partner")
                .company_default_for("co.target.partner_id"),
            FieldDecl::selection("default_state", STATES).company_default_for("co.target.state"),
            FieldDecl::boolean("default_flag").company_default_for("co.target.flag"),
            FieldDecl::integer("default_count").company_default_for("co.target.count"),
            FieldDecl::char("default_label").company_default_for("co.target.label"),
        ]),
        target(),
        partner(),
        other(),
    ])
    .unwrap();
    let company = r.model("res.company").unwrap();
    let target = r.model("co.target").unwrap();
    for (name, target_name) in [
        ("credit_limit", "credit_limit"),
        ("default_partner_id", "partner_id"),
        ("default_state", "state"),
        ("default_flag", "flag"),
        ("default_count", "count"),
        ("default_label", "label"),
    ] {
        let field = company.field(name).unwrap();
        let resolved = field.company_default_for.unwrap();
        assert_eq!(resolved.model, target.id, "{name}");
        assert_eq!(
            resolved.field,
            target.field(target_name).unwrap().id,
            "{name}"
        );
        assert!(!field.store, "{name} is not stored");
        assert!(!field.has_column(), "{name} has no column");
        assert!(!field.readonly, "{name} is writable");
        assert!(!field.copy, "{name} is not copied");
        assert!(!field.company_dependent, "{name}");
    }
    // a field without the attribute carries no target
    assert!(company.field("name").unwrap().company_default_for.is_none());
}

#[test]
fn only_res_company_accepts_the_attribute() {
    let (kind, message) = refusal_of(vec![
        company(vec![]),
        target(),
        partner(),
        m(
            "co.other",
            vec![FieldDecl::float("limit").company_default_for("co.target.credit_limit")],
        ),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'co.other.limit': company_default_for = \"co.target.credit_limit\" is only \
         accepted on res.company (the field mirrors a company's default of the target)"
    );
}

#[test]
fn a_target_without_a_field_part_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::float("limit").company_default_for("credit"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.limit': company_default_for = \"credit\" must name its target as \
         \"<model>.<field>\""
    );
}

#[test]
fn an_unknown_target_model_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::float("limit").company_default_for("no.such.credit_limit"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.limit': company_default_for = \"no.such.credit_limit\" names model \
         'no.such', which is not in the registry"
    );
}

#[test]
fn an_unknown_target_field_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::float("limit").company_default_for("co.target.nope"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.limit': company_default_for = \"co.target.nope\" names field 'nope', \
         which model 'co.target' does not have"
    );
}

#[test]
fn a_target_that_is_not_company_dependent_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::float("limit").company_default_for("co.target.plain"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.limit': company_default_for = \"co.target.plain\" names a field that \
         is not company-dependent"
    );
}

#[test]
fn a_kind_other_than_the_targets_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::integer("limit").company_default_for("co.target.credit_limit"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.limit': company_default_for = \"co.target.credit_limit\" needs a \
         float field like its target; this field is integer"
    );
}

#[test]
fn a_float_with_other_digits_than_the_targets_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::float_digits("limit", erp_fields::Digits::Named("Product Price"))
            .company_default_for("co.target.credit_limit"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.limit': company_default_for = \"co.target.credit_limit\" needs the \
         digits of its target (None); this field has Some(Named(\"Product Price\"))"
    );
}

#[test]
fn a_many2one_to_another_comodel_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::many2one("default_partner_id", "co.other")
            .company_default_for("co.target.partner_id"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.default_partner_id': company_default_for = \
         \"co.target.partner_id\" needs a many2one to 'co.partner' like its target; this field \
         points to 'co.other'"
    );
}

#[test]
fn a_selection_with_other_keys_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::selection("default_state", &[("a", "A"), ("c", "C")])
            .company_default_for("co.target.state"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.default_state': company_default_for = \"co.target.state\" needs the \
         selection keys of its target [a, b]; this field has [a, c]"
    );
}

#[test]
fn conflicting_attributes_are_refused() {
    let base = || FieldDecl::float("limit").company_default_for("co.target.credit_limit");
    let label = || FieldDecl::char("limit").company_default_for("co.target.label");
    for (decl, attribute) in [
        (base().compute("_compute_limit"), "compute"),
        (base().inverse("_inverse_limit"), "inverse"),
        (base().related("parent_id.limit"), "related"),
        (base().store(true), "store = true"),
        (base().copy(true), "copy = true"),
        (label().translate(true), "translate"),
        (base().company_dependent(true), "company_dependent"),
    ] {
        let (kind, message) = refusal(vec![decl]);
        assert_eq!(kind, ErrorKind::Programming, "{attribute}");
        assert_eq!(
            message,
            format!(
                "field 'res.company.limit': company_default_for cannot be combined with \
                 {attribute} (the field holds no data of its own: the per-company ir.default of \
                 its target is the only source)"
            ),
            "{attribute}"
        );
    }
}

#[test]
fn two_fields_for_one_target_are_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::float("limit").company_default_for("co.target.credit_limit"),
        FieldDecl::float("limit_again").company_default_for("co.target.credit_limit"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.limit_again': company_default_for = \"co.target.credit_limit\" is \
         already declared by 'res.company.limit'; one company field per target"
    );
}

#[test]
fn a_kind_that_is_not_admitted_is_refused() {
    let (kind, message) = refusal(vec![
        FieldDecl::date("default_day").company_default_for("co.target.day"),
    ]);
    assert_eq!(kind, ErrorKind::Programming);
    assert_eq!(
        message,
        "field 'res.company.default_day': company_default_for supports boolean, char, float, \
         integer, many2one and selection fields; this field is date"
    );
}
