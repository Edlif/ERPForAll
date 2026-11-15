// SPDX-License-Identifier: MIT OR Apache-2.0
//! `company_default_for = "<model>.<field>"` (supplement of 2026-11-15):
//! a `res.company` field that mirrors the per-company `ir.default` of a company-dependent field.
//!
//! [`check_declaration`] refuses what one merged declaration already shows (owner model, attribute
//! combinations, kind); [`finish`] resolves the target once every model exists and refuses a wrong
//! one. Each refusal has its own message naming model, field and target. Pure: no database.

use erp_fields::{Error, FieldType, Result};

use crate::decl::FieldDecl;
use crate::spec::{FieldId, ModelId, ModelSpec};

/// The only model whose fields may declare `company_default_for`.
const COMPANY_MODEL: &str = "res.company";

/// The kinds a `company_default_for` field may have, as `FieldType::type_name` spells them, in
/// the order the refusal lists them. A kind is listed only when the ORM ships its round-trip
/// test (`crates/erp-test-models/tests/company_default_for.rs`).
const ADMITTED_KINDS: &str = "boolean, char, float, integer, many2one and selection";

/// The resolved target of a `company_default_for` field: the company-dependent field whose
/// per-company `ir.default` the company field shows and edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompanyDefaultFor {
    /// The declared path, `"<model>.<field>"`.
    pub path: &'static str,
    /// The target model.
    pub model: ModelId,
    /// The company-dependent field of that model.
    pub field: FieldId,
}

fn admitted(ty: &FieldType) -> bool {
    matches!(
        ty,
        FieldType::Boolean
            | FieldType::Char { .. }
            | FieldType::Float { .. }
            | FieldType::Integer
            | FieldType::Many2one { .. }
            | FieldType::Selection { .. }
    )
}

/// The checks one merged declaration allows: the owner model, the attribute combinations and
/// the kind. Called for every field; a field without the attribute passes.
pub(crate) fn check_declaration(model: &str, p: &FieldDecl) -> Result<()> {
    let Some(target) = p.company_default_for else {
        return Ok(());
    };
    let name = p.name;
    if model != COMPANY_MODEL {
        return Err(Error::programming(format!(
            "field '{model}.{name}': company_default_for = \"{target}\" is only accepted on \
             {COMPANY_MODEL} (the field mirrors a company's default of the target)"
        )));
    }
    let conflict = if p.compute.is_some() {
        Some("compute")
    } else if p.inverse.is_some() {
        Some("inverse")
    } else if p.related.is_some() {
        Some("related")
    } else if p.store == Some(true) {
        Some("store = true")
    } else if p.copy == Some(true) {
        Some("copy = true")
    } else if p.translate == Some(true) {
        Some("translate")
    } else if p.company_dependent == Some(true) {
        Some("company_dependent")
    } else if matches!(p.generated, Some(Some(_))) {
        Some("a generated expression")
    } else {
        None
    };
    if let Some(attribute) = conflict {
        return Err(Error::programming(format!(
            "field '{model}.{name}': company_default_for cannot be combined with {attribute} \
             (the field holds no data of its own: the per-company ir.default of its target is \
             the only source)"
        )));
    }
    if target
        .rsplit_once('.')
        .is_none_or(|(m, f)| m.is_empty() || f.is_empty())
    {
        return Err(Error::programming(format!(
            "field '{model}.{name}': company_default_for = \"{target}\" must name its target as \
             \"<model>.<field>\""
        )));
    }
    Ok(())
}

/// The static selection keys of a field type, in declaration order.
fn selection_keys(ty: &FieldType) -> Option<Vec<&str>> {
    match ty {
        FieldType::Selection {
            values: Some(values),
            ..
        } => Some(values.iter().map(|(key, _)| key.as_str()).collect()),
        _ => None,
    }
}

/// Resolve every declared target and refuse a wrong one (see the module documentation).
pub(crate) fn finish(models: &mut [ModelSpec]) -> Result<()> {
    let mut resolved: Vec<(usize, usize, Option<CompanyDefaultFor>)> = Vec::new();
    for (mi, model) in models.iter().enumerate() {
        let mut seen: Vec<(&'static str, &'static str)> = Vec::new();
        for (fi, field) in model.fields.iter().enumerate() {
            let Some(path) = field.company_default_for_declared else {
                continue;
            };
            if field.inherited.is_some() {
                // a copy of a delegated parent's field: read and written on the parent
                resolved.push((mi, fi, None));
                continue;
            }
            if !admitted(&field.ty) {
                return Err(Error::programming(format!(
                    "field '{}.{}': company_default_for supports {ADMITTED_KINDS} fields; this \
                     field is {}",
                    model.name,
                    field.name,
                    field.ty.type_name()
                )));
            }
            let here = format!(
                "field '{}.{}': company_default_for = \"{path}\"",
                model.name, field.name
            );
            let (target_model, target_field) = path.rsplit_once('.').ok_or_else(|| {
                Error::programming(format!(
                    "{here} must name its target as \"<model>.<field>\""
                ))
            })?;
            let target = models
                .iter()
                .find(|m| m.name == target_model)
                .ok_or_else(|| {
                    Error::programming(format!(
                        "{here} names model '{target_model}', which is not in the registry"
                    ))
                })?;
            let tf = target.field(target_field).ok_or_else(|| {
                Error::programming(format!(
                    "{here} names field '{target_field}', which model '{target_model}' does not \
                     have"
                ))
            })?;
            if !tf.company_dependent || tf.inherited.is_some() {
                return Err(Error::programming(format!(
                    "{here} names a field that is not company-dependent"
                )));
            }
            if field.ty.type_name() != tf.ty.type_name() {
                return Err(Error::programming(format!(
                    "{here} needs a {} field like its target; this field is {}",
                    tf.ty.type_name(),
                    field.ty.type_name()
                )));
            }
            // Reads apply the target's precision to the stored default: a value written under other
            // digits would make every later read of the target fail (review cycle 21, M1).
            if let (FieldType::Float { digits: own }, FieldType::Float { digits: theirs }) =
                (&field.ty, &tf.ty)
                && own != theirs
            {
                return Err(Error::programming(format!(
                    "{here} needs the digits of its target ({theirs:?}); this field has {own:?}"
                )));
            }
            if let (Some(own), Some(theirs)) = (field.ty.comodel(), tf.ty.comodel())
                && own != theirs
            {
                return Err(Error::programming(format!(
                    "{here} needs a many2one to '{theirs}' like its target; this field points \
                     to '{own}'"
                )));
            }
            if matches!(field.ty, FieldType::Selection { .. }) {
                let (Some(own), Some(theirs)) = (selection_keys(&field.ty), selection_keys(&tf.ty))
                else {
                    return Err(Error::programming(format!(
                        "{here} needs a static selection on both fields (a method-backed \
                         selection has no keys to compare)"
                    )));
                };
                let mut a = own.clone();
                let mut b = theirs.clone();
                a.sort_unstable();
                b.sort_unstable();
                if a != b {
                    return Err(Error::programming(format!(
                        "{here} needs the selection keys of its target [{}]; this field has [{}]",
                        theirs.join(", "),
                        own.join(", ")
                    )));
                }
            }
            if let Some((_, first)) = seen.iter().find(|(p, _)| *p == path) {
                return Err(Error::programming(format!(
                    "{here} is already declared by '{}.{first}'; one company field per target",
                    model.name
                )));
            }
            seen.push((path, field.name));
            resolved.push((
                mi,
                fi,
                Some(CompanyDefaultFor {
                    path,
                    model: target.id,
                    field: tf.id,
                }),
            ));
        }
    }
    for (mi, fi, target) in resolved {
        let field = &mut models[mi].fields[fi];
        field.company_default_for = target;
        if target.is_none() {
            field.company_default_for_declared = None;
        }
    }
    Ok(())
}
