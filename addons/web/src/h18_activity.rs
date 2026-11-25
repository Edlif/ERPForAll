// SPDX-License-Identifier: MIT OR Apache-2.0
//! Activity matrix view admission and lowering.
//!
//! An `<activity>` root displays records as rows against `mail.activity.type` columns. Its direct
//! children are `<field>` nodes rendered in the leading record summary column, and an optional
//! `avatar_field` (`binary`/`image`) supplies each row's avatar thumbnail.

use super::{Node, ViewResult, attrs, field_type_name, invalid};
use erp_sdk::views::DescriptionObjectBuilder;
use erp_sdk::{FieldSpec, ModelSpec};

pub(super) const ROOT_ATTRS: &[&str] = &["string", "avatar_field"];

fn optional_avatar<'a, 's>(
    spec: &'s ModelSpec,
    node: impl Node<'a>,
) -> ViewResult<Option<&'s FieldSpec>> {
    let Some(name) = node.attr("avatar_field") else {
        return Ok(None);
    };
    let field = spec.field(name).ok_or_else(|| {
        invalid(format!(
            "Activity avatar_field names unknown field '{}.{name}'.",
            spec.name
        ))
    })?;
    if field_type_name(field)? != "binary" {
        return Err(invalid(format!(
            "Activity avatar_field '{}.{}' must be a binary or image field.",
            spec.name, field.name
        )));
    }
    Ok(Some(field))
}

pub(super) fn admit_root<'a, N: Node<'a>>(node: N, spec: &ModelSpec) -> ViewResult<()> {
    attrs(node, ROOT_ATTRS)?;
    optional_avatar(spec, node)?;
    Ok(())
}

pub(super) fn build_root<'a, N: Node<'a>>(
    node: N,
    spec: &ModelSpec,
    object: &mut DescriptionObjectBuilder<'_>,
) -> ViewResult<()> {
    if let Some(avatar) = optional_avatar(spec, node)? {
        object.set("avatar_field", |value| {
            value.field_handle(spec.name, avatar.name)
        })?;
    }
    Ok(())
}
