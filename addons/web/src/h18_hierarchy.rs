// SPDX-License-Identifier: MIT OR Apache-2.0
//! Hierarchy (org chart / tree) view admission and lowering.
//!
//! A `<hierarchy>` root declares its self-referential `parent_field` (required, stored `many2one`
//! targeting the root model) and optional `child_field` (`one2many` targeting the root model),
//! `avatar_field` (`binary`/`image`), `color_field` (`integer`), `count_field` (`integer`),
//! `draggable` (`boolean`, default false), and `label` (plural noun for subordinate counters).
//! Its direct children are ordinary `<field>` nodes rendered on each hierarchy card.

use super::{Node, ViewResult, attrs, boolean, field_type_name, invalid};
use erp_sdk::views::DescriptionObjectBuilder;
use erp_sdk::{FieldSpec, FieldType, ModelSpec};

pub(super) const ROOT_ATTRS: &[&str] = &[
    "string",
    "parent_field",
    "child_field",
    "avatar_field",
    "color_field",
    "count_field",
    "draggable",
    "label",
];

fn parent_field<'a, 's>(spec: &'s ModelSpec, node: impl Node<'a>) -> ViewResult<&'s FieldSpec> {
    let name = node
        .attr("parent_field")
        .ok_or_else(|| invalid("A <hierarchy> view requires a parent_field attribute."))?;
    let field = spec.field(name).ok_or_else(|| {
        invalid(format!(
            "Hierarchy parent_field names unknown field '{}.{name}'.",
            spec.name
        ))
    })?;
    if !field.store {
        return Err(invalid(format!(
            "Hierarchy parent_field '{}.{}' must be stored.",
            spec.name, field.name
        )));
    }
    match &field.ty {
        FieldType::Many2one { comodel, .. } if comodel.as_str() == spec.name => Ok(field),
        _ => Err(invalid(format!(
            "Hierarchy parent_field '{}.{}' must be a many2one field referencing '{}'.",
            spec.name, field.name, spec.name
        ))),
    }
}

fn optional_child_field<'a, 's>(
    spec: &'s ModelSpec,
    node: impl Node<'a>,
) -> ViewResult<Option<&'s FieldSpec>> {
    let Some(name) = node.attr("child_field") else {
        return Ok(None);
    };
    let field = spec.field(name).ok_or_else(|| {
        invalid(format!(
            "Hierarchy child_field names unknown field '{}.{name}'.",
            spec.name
        ))
    })?;
    match &field.ty {
        FieldType::One2many { comodel, .. } if comodel.as_str() == spec.name => Ok(Some(field)),
        _ => Err(invalid(format!(
            "Hierarchy child_field '{}.{}' must be a one2many field referencing '{}'.",
            spec.name, field.name, spec.name
        ))),
    }
}

fn optional_typed_field<'a, 's>(
    spec: &'s ModelSpec,
    node: impl Node<'a>,
    attr: &str,
    allowed_kind: &str,
) -> ViewResult<Option<&'s FieldSpec>> {
    let Some(name) = node.attr(attr) else {
        return Ok(None);
    };
    let field = spec.field(name).ok_or_else(|| {
        invalid(format!(
            "Hierarchy {attr} names unknown field '{}.{name}'.",
            spec.name
        ))
    })?;
    if field_type_name(field)? != allowed_kind {
        return Err(invalid(format!(
            "Hierarchy {attr} '{}.{}' must be a {allowed_kind} field.",
            spec.name, field.name
        )));
    }
    Ok(Some(field))
}

pub(super) fn admit_root<'a, N: Node<'a>>(node: N, spec: &ModelSpec) -> ViewResult<()> {
    attrs(node, ROOT_ATTRS)?;
    parent_field(spec, node)?;
    optional_child_field(spec, node)?;
    optional_typed_field(spec, node, "avatar_field", "binary")?;
    optional_typed_field(spec, node, "color_field", "integer")?;
    optional_typed_field(spec, node, "count_field", "integer")?;
    boolean(node, "draggable", false)?;
    if let Some(label) = node.attr("label")
        && (label.trim().is_empty() || label.len() > 64)
    {
        return Err(invalid(
            "Hierarchy label must be non-empty and at most 64 bytes.",
        ));
    }
    Ok(())
}

pub(super) fn build_root<'a, N: Node<'a>>(
    node: N,
    spec: &ModelSpec,
    object: &mut DescriptionObjectBuilder<'_>,
) -> ViewResult<()> {
    let parent = parent_field(spec, node)?;
    object.set("parent_field", |value| {
        value.field_handle(spec.name, parent.name)
    })?;
    if let Some(child) = optional_child_field(spec, node)? {
        object.set("child_field", |value| {
            value.field_handle(spec.name, child.name)
        })?;
    }
    if let Some(avatar) = optional_typed_field(spec, node, "avatar_field", "binary")? {
        object.set("avatar_field", |value| {
            value.field_handle(spec.name, avatar.name)
        })?;
    }
    if let Some(color) = optional_typed_field(spec, node, "color_field", "integer")? {
        object.set("color_field", |value| {
            value.field_handle(spec.name, color.name)
        })?;
    }
    if let Some(count) = optional_typed_field(spec, node, "count_field", "integer")? {
        object.set("count_field", |value| {
            value.field_handle(spec.name, count.name)
        })?;
    }
    object.set("draggable", |value| {
        value.boolean(boolean(node, "draggable", false)?)
    })?;
    if let Some(label) = node.attr("label") {
        object.set("label", |value| value.text(label.trim()))?;
    }
    Ok(())
}
