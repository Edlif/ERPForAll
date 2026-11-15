// SPDX-License-Identifier: MIT OR Apache-2.0
//! Builtin `<graph>` admission and lowering: the chart type, its flags and the
//! fields it groups by and measures. The grouped read is the existing `formatted_read_group`; nothing
//! here reads data.
use super::*;

/// Attributes the `<graph>` root may carry.
pub(super) const ROOT_ATTRS: &[&str] = &[
    "string",
    "type",
    "stacked",
    "cumulated",
    "order",
    "disable_linking",
];
/// The first grouping is the axis (or the slices), the second the series; the chart draws no third.
const MAX_GROUPINGS: usize = 2;

fn mode<'a>(node: impl Node<'a>) -> ViewResult<&'static str> {
    match node.attr("type") {
        None | Some("bar") => Ok("bar"),
        Some("line") => Ok("line"),
        Some("pie") => Ok("pie"),
        Some(other) => Err(invalid(format!(
            "The graph view draws a bar, line or pie chart; type=\"{other}\" is none of them."
        ))),
    }
}
fn order<'a>(node: impl Node<'a>) -> ViewResult<Option<&'static str>> {
    match node.attr("order") {
        None => Ok(None),
        Some("asc" | "ASC") => Ok(Some("asc")),
        Some("desc" | "DESC") => Ok(Some("desc")),
        Some(other) => Err(invalid(format!(
            "The graph view sorts its groups asc or desc; order=\"{other}\" is neither."
        ))),
    }
}
fn flags<'a>(node: impl Node<'a>) -> ViewResult<(bool, bool, bool)> {
    Ok((
        boolean(node, "stacked", true)?,
        boolean(node, "cumulated", false)?,
        boolean(node, "disable_linking", false)?,
    ))
}
/// A column the grouped read can aggregate or group: stored or database-generated
/// (the `store`/`groupable` rule of `erp-orm` `describe.rs`).
fn has_column(field: &FieldSpec) -> bool {
    field.has_column() || field.is_generated()
}

pub(super) fn admit_root<'a>(node: impl Node<'a>) -> ViewResult<()> {
    mode(node)?;
    order(node)?;
    flags(node)?;
    Ok(())
}

pub(super) fn admit_field<'a>(
    node: impl Node<'a>,
    parent: impl Node<'a>,
    spec: &ModelSpec,
) -> ViewResult<&'static str> {
    // `widget`, `invisible` and `options` stay refused by name: the chart formats every measure itself.
    attrs(node, &["name", "string", "type", "interval"])?;
    let field = field(spec, node)?;
    if node.children().next().is_some() {
        return Err(invalid("Field nodes cannot contain children."));
    }
    let kind = field_type_name(field)?;
    let name = node.attr("name");
    match node.attr("type") {
        None | Some("row" | "col") => {
            let groupable = matches!(
                kind,
                "boolean" | "char" | "integer" | "selection" | "many2one" | "date" | "datetime"
            );
            if !groupable || !has_column(field) {
                let stored = if groupable { " without a column" } else { "" };
                return Err(invalid(format!(
                    "Graph field '{}.{}' cannot group the chart: it is a {kind} field{stored}; the graph view groups by stored boolean, char, integer, selection, many2one, date and datetime fields.",
                    spec.name, field.name
                )));
            }
            if let Some(interval) = node.attr("interval") {
                if !matches!(kind, "date" | "datetime") {
                    return Err(invalid(format!(
                        "Graph field '{}.{}' has an interval but is a {kind} field; 'interval' applies to date and datetime fields.",
                        spec.name, field.name
                    )));
                }
                if !PIVOT_INTERVALS.contains(&interval) {
                    return Err(invalid(format!(
                        "Graph field '{}.{}' has interval \"{interval}\"; it must be one of day, week, month, quarter or year.",
                        spec.name, field.name
                    )));
                }
            }
            let groupings = parent.children().filter(|sibling| is_grouping(*sibling));
            let (mut total, mut same) = (0, 0);
            for sibling in groupings {
                total += 1;
                same += usize::from(sibling.attr("name") == name);
            }
            if total > MAX_GROUPINGS {
                return Err(invalid(format!(
                    "A graph view groups by at most {MAX_GROUPINGS} fields (the axis, then the series); this one declares {total}."
                )));
            }
            if same > 1 {
                return Err(invalid(format!(
                    "Graph field '{}.{}' is declared twice as a grouping; a chart groups by a field once.",
                    spec.name, field.name
                )));
            }
        }
        Some("measure") => {
            if node.attr("interval").is_some() {
                return Err(invalid(format!(
                    "Graph measure '{}.{}' has an interval; 'interval' applies to date and datetime groupings.",
                    spec.name, field.name
                )));
            }
            // The aggregator rule of `describe.rs`: stored numbers other than `sequence` add up.
            if !matches!(kind, "integer" | "float" | "monetary")
                || !has_column(field)
                || field.name == "sequence"
            {
                return Err(invalid(format!(
                    "Graph measure '{}.{}' must be a stored integer, float or monetary field that adds up.",
                    spec.name, field.name
                )));
            }
            let declared = parent
                .children()
                .filter(|sibling| {
                    sibling.attr("type") == Some("measure") && sibling.attr("name") == name
                })
                .count();
            if declared > 1 {
                return Err(invalid(format!(
                    "Graph measure '{}.{}' is declared more than once.",
                    spec.name, field.name
                )));
            }
        }
        Some(other) => {
            return Err(invalid(format!(
                "Graph field '{}.{}' has type=\"{other}\"; it must be row, col or measure (or absent for a grouping).",
                spec.name, field.name
            )));
        }
    }
    Ok("graph_field")
}
fn is_grouping<'a>(node: impl Node<'a>) -> bool {
    node.tag() == "field" && node.attr("type") != Some("measure")
}

pub(super) fn build_root<'a>(
    node: impl Node<'a>,
    object: &mut erp_sdk::views::DescriptionObjectBuilder<'_>,
) -> ViewResult<()> {
    let (stacked, cumulated, disable_linking) = flags(node)?;
    let mode = mode(node)?;
    object.set("mode", |value| value.text(mode))?;
    object.set("stacked", |value| value.boolean(stacked))?;
    object.set("cumulated", |value| value.boolean(cumulated))?;
    if let Some(order) = order(node)? {
        object.set("order", |value| value.text(order))?;
    }
    object.set("disable_linking", |value| value.boolean(disable_linking))?;
    Ok(())
}

pub(super) fn build_field<'a>(
    node: impl Node<'a>,
    spec: &ModelSpec,
    object: &mut erp_sdk::views::DescriptionObjectBuilder<'_>,
) -> ViewResult<()> {
    let field = field(spec, node)?;
    object.set("field", |value| value.field_handle(spec.name, field.name))?;
    object.set("label", |value| {
        value.text(node.attr("string").unwrap_or(&field.string))
    })?;
    let role = if is_grouping(node) {
        "group"
    } else {
        "measure"
    };
    object.set("role", |value| value.text(role))?;
    if let Some(interval) = node.attr("interval") {
        object.set("interval", |value| value.text(interval))?;
    }
    Ok(())
}
