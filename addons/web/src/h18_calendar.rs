// SPDX-License-Identifier: MIT OR Apache-2.0
//! Admission and lowering of the `<calendar>` arch.
//!
//! The root names the fields that place an event (start, stop, all-day, colour) and what the grid offers
//! (scales, first scale, create/edit/delete, quick create, hidden date or time, the month
//! overflow and the per-cell event limit). Its `<field>` children are shown in the event popover and, with
//! `filters="1"`, become side-panel filters. Everything the first calendar slices do not carry is refused by
//! name with the task that adds it, never ignored.

use super::{Node, attrs, boolean, field, field_type_name, invalid};
use erp_sdk::views::{DescriptionObjectBuilder, ViewResult};
use erp_sdk::{FieldSpec, FieldType, ModelSpec};

/// The task that admits what this slice refuses.
const LATER: &str = "a later release";
const SCALES: [&str; 4] = ["day", "week", "month", "year"];
const DEFAULT_EVENT_LIMIT: i64 = 5;
/// Safety bound on the per-cell limit an arch may declare.
const MAX_EVENT_LIMIT: i64 = 50;

const ROOT_ATTRS: &[&str] = &[
    "string",
    "date_start",
    "date_stop",
    "all_day",
    "color",
    "mode",
    "scales",
    "create",
    "edit",
    "delete",
    "quick_create",
    "hide_date",
    "hide_time",
    "month_overflow",
    "event_limit",
];
/// Reference root attributes the first slices do not carry yet.
const ROOT_LATER: &[&str] = &[
    "aggregate",
    "event_open_popup",
    "form_view_id",
    "create_name_field",
    "quick_create_view_id",
    "multi_create_view",
    "show_unusual_days",
    "show_date_picker",
    "schedule",
    "js_class",
];
const FIELD_ATTRS: &[&str] = &["name", "string", "invisible", "filters"];
/// Reference field attributes the first slices do not carry yet.
const FIELD_LATER: &[&str] = &[
    "widget",
    "options",
    "color",
    "avatar_field",
    "write_model",
    "write_field",
    "filter_field",
];

fn later(what: impl std::fmt::Display) -> erp_sdk::views::ViewError {
    invalid(format!(
        "Calendar {what} is not available yet; it comes with {LATER}."
    ))
}

/// The admitted root, read once for validation and again for the description.
struct Root<'s> {
    date_start: &'s FieldSpec,
    date_stop: Option<&'s FieldSpec>,
    all_day: Option<&'s FieldSpec>,
    color: Option<&'s FieldSpec>,
    mode: &'static str,
    scales: [Option<&'static str>; 4],
    event_limit: i64,
}

fn root_field<'s, 'a>(
    spec: &'s ModelSpec,
    node: impl Node<'a>,
    key: &str,
) -> ViewResult<Option<&'s FieldSpec>> {
    let Some(name) = node.attr(key) else {
        return Ok(None);
    };
    spec.field(name).map(Some).ok_or_else(|| {
        invalid(format!(
            "Calendar {key} names unknown field '{}.{name}'.",
            spec.name
        ))
    })
}

fn scale(value: &str) -> ViewResult<&'static str> {
    SCALES
        .into_iter()
        .find(|scale| *scale == value)
        .ok_or_else(|| {
            invalid(format!(
                "Calendar scale \"{value}\" must be one of day, week, month or year."
            ))
        })
}

fn root<'a, 's>(node: impl Node<'a>, spec: &'s ModelSpec) -> ViewResult<Root<'s>> {
    for (key, _) in node.attributes() {
        if ROOT_LATER.contains(&key) {
            return Err(later(format_args!("attribute '{key}'")));
        }
    }
    attrs(node, ROOT_ATTRS)?;
    let date_start = root_field(spec, node, "date_start")?
        .ok_or_else(|| invalid("A calendar view needs date_start."))?;
    let stored = |field: &FieldSpec| field.has_column() || field.is_generated();
    let date_kind = |key: &str, field: &FieldSpec| match field.ty {
        FieldType::Date | FieldType::Datetime if stored(field) => Ok(()),
        _ => Err(invalid(format!(
            "Calendar {key} '{}.{}' must be a stored date or datetime field.",
            spec.name, field.name
        ))),
    };
    date_kind("date_start", date_start)?;
    let date_stop = root_field(spec, node, "date_stop")?;
    if let Some(stop) = date_stop {
        date_kind("date_stop", stop)?;
        if std::mem::discriminant(&stop.ty) != std::mem::discriminant(&date_start.ty) {
            return Err(invalid(format!(
                "Calendar date_stop '{}.{}' must have the type of date_start ({}).",
                spec.name,
                stop.name,
                date_start.ty.type_name()
            )));
        }
    }
    let all_day = root_field(spec, node, "all_day")?;
    if let Some(field) = all_day
        && !matches!(field.ty, FieldType::Boolean)
    {
        return Err(invalid(format!(
            "Calendar all_day '{}.{}' must be a boolean field.",
            spec.name, field.name
        )));
    }
    let color = root_field(spec, node, "color")?;
    if let Some(field) = color
        && !matches!(field.ty, FieldType::Many2one { .. } | FieldType::Integer)
    {
        return Err(invalid(format!(
            "Calendar color '{}.{}' must be a many2one or integer field.",
            spec.name, field.name
        )));
    }
    let mut scales = [None; 4];
    match node.attr("scales") {
        None => scales = SCALES.map(Some),
        Some(raw) => {
            for (count, part) in raw.split(',').enumerate() {
                let value = scale(part.trim())?;
                if scales[..count].contains(&Some(value)) {
                    return Err(invalid(format!(
                        "Calendar scale \"{value}\" is listed twice."
                    )));
                }
                scales[count] = Some(value);
            }
        }
    }
    let offered = |value: &str| scales.contains(&Some(value));
    let mode = match node.attr("mode") {
        Some(raw) => {
            let value = scale(raw.trim())?;
            if !offered(value) {
                return Err(invalid(format!(
                    "Calendar mode \"{value}\" is not one of its scales."
                )));
            }
            value
        }
        None if offered("week") => "week",
        None => scales[0].ok_or_else(|| invalid("A calendar view offers no scale."))?,
    };
    let event_limit = match node.attr("event_limit") {
        None => DEFAULT_EVENT_LIMIT,
        Some(raw) => raw
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|limit| (1..=MAX_EVENT_LIMIT).contains(limit))
            .ok_or_else(|| {
                invalid(format!(
                    "Calendar event_limit must be an integer from 1 through {MAX_EVENT_LIMIT}."
                ))
            })?,
    };
    for key in [
        "create",
        "edit",
        "delete",
        "quick_create",
        "hide_date",
        "hide_time",
        "month_overflow",
    ] {
        boolean(node, key, false)?;
    }
    Ok(Root {
        date_start,
        date_stop,
        all_day,
        color,
        mode,
        scales,
        event_limit,
    })
}

/// Admit a `<calendar>` root; called by the shared classifier.
pub(super) fn validate_root<'a>(node: impl Node<'a>, spec: &ModelSpec) -> ViewResult<()> {
    root(node, spec).map(drop)
}

/// Admit one child of `<calendar>`: only `<field>` (the popover template comes later).
pub(super) fn validate_child<'a, N: Node<'a>>(
    node: N,
    parent: N,
    spec: &ModelSpec,
) -> ViewResult<&'static str> {
    match node.tag() {
        "field" => {}
        "popover" => return Err(later("<popover>")),
        other => {
            return Err(invalid(format!(
                "View element <{other}> is not allowed under <calendar>; only <field> is."
            )));
        }
    }
    for (key, _) in node.attributes() {
        if FIELD_LATER.contains(&key) {
            return Err(later(format_args!("field attribute '{key}'")));
        }
    }
    attrs(node, FIELD_ATTRS)?;
    if node.children().next().is_some() {
        return Err(invalid("Calendar field nodes cannot contain children."));
    }
    let field = field(spec, node)?;
    let kind = field_type_name(field)?;
    boolean(node, "invisible", false)?;
    if boolean(node, "filters", false)? && !matches!(kind, "many2one" | "selection") {
        return Err(later(format_args!(
            "filter on '{}.{}' (a {kind} field; filters are many2one or selection fields for now)",
            spec.name, field.name
        )));
    }
    if parent
        .children()
        .filter(|sibling| sibling.tag() == "field" && sibling.attr("name") == Some(field.name))
        .count()
        > 1
    {
        return Err(invalid(format!(
            "Calendar field '{}.{}' is declared twice.",
            spec.name, field.name
        )));
    }
    Ok("calendar_field")
}

/// Fill the `calendar` root description (its children are set by the caller).
pub(super) fn build_root<'a>(
    node: impl Node<'a>,
    spec: &ModelSpec,
    object: &mut DescriptionObjectBuilder<'_>,
) -> ViewResult<()> {
    let root = root(node, spec)?;
    object.set("date_start", |value| {
        value.field_handle(spec.name, root.date_start.name)
    })?;
    for (key, field) in [
        ("date_stop", root.date_stop),
        ("all_day", root.all_day),
        ("color", root.color),
    ] {
        if let Some(field) = field {
            object.set(key, |value| value.field_handle(spec.name, field.name))?;
        }
    }
    object.set("mode", |value| value.text(root.mode))?;
    let scales: Vec<&str> = root.scales.into_iter().flatten().collect();
    object.set("scales", |value| {
        value.array(scales.len(), |index, value| value.text(scales[index]))
    })?;
    for (key, default) in [
        ("create", true),
        ("edit", true),
        ("delete", true),
        ("quick_create", true),
        ("hide_date", false),
        ("hide_time", false),
        ("month_overflow", true),
    ] {
        let flag = boolean(node, key, default)?;
        object.set(key, |value| value.boolean(flag))?;
    }
    object.set("event_limit", |value| value.integer(root.event_limit))
}

/// Fill one `calendar_field` description.
pub(super) fn build_field<'a>(
    node: impl Node<'a>,
    spec: &ModelSpec,
    object: &mut DescriptionObjectBuilder<'_>,
) -> ViewResult<()> {
    let field = field(spec, node)?;
    object.set("field", |value| value.field_handle(spec.name, field.name))?;
    object.set("label", |value| {
        value.text(node.attr("string").unwrap_or(&field.string))
    })?;
    object.set("invisible", |value| {
        value.boolean(boolean(node, "invisible", false)?)
    })?;
    object.set("filters", |value| {
        value.boolean(boolean(node, "filters", false)?)
    })
}
