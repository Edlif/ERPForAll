// SPDX-License-Identifier: MIT OR Apache-2.0
//! Strict value grammars; no CSS execution, URL loading, custom properties or recovery.
use crate::profile::listed;
const PROPERTIES: &str = "width height max-width max-height font-size font-weight font-family \
    color background-color text-align vertical-align line-height white-space display \
    page-break-before page-break-after page-break-inside margin margin-top margin-right \
    margin-bottom margin-left padding padding-top padding-right padding-bottom padding-left \
    border border-top border-right border-bottom border-left border-width border-style \
    border-color border-collapse object-fit";
const COLORS: &str = "transparent black white red green blue gray grey yellow orange purple navy \
    silver maroon teal aqua fuchsia lime olive";
pub(crate) fn known(name: &str) -> bool {
    listed(PROPERTIES, name)
}
pub(crate) fn valid(name: &str, value: &str) -> bool {
    if value.is_empty() || value.contains(['!', '{', '}', '\\', '(', ')']) {
        return false;
    }
    let one_of = |table| listed(table, value);
    match name {
        "width" | "height" | "max-width" | "max-height" => value == "auto" || length(value, false),
        "font-size" | "border-width" => length(value, false),
        "font-weight" => one_of("normal bold 100 200 300 400 500 600 700 800 900"),
        "font-family" => value.split(',').all(|f| {
            let f = f.trim();
            let f = if f.starts_with(['\'', '"']) {
                if f.len() < 2 || f.as_bytes().first() != f.as_bytes().last() {
                    return false;
                }
                &f[1..f.len() - 1]
            } else {
                f
            };
            !f.is_empty()
                && f.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        }),
        "color" | "background-color" | "border-color" => color(value),
        "text-align" => one_of("left right start end center justify"),
        "vertical-align" => one_of("top middle bottom baseline"),
        "object-fit" => one_of("contain cover fill none scale-down"),
        "line-height" => value == "normal" || number(value, false) || length(value, false),
        "white-space" => one_of("normal nowrap pre pre-wrap pre-line"),
        "display" => one_of("none block inline-block"),
        "page-break-before" | "page-break-after" => one_of("auto always avoid left right"),
        "page-break-inside" => one_of("auto avoid"),
        "border-collapse" => one_of("collapse separate"),
        "border-style" => one_of("none solid dashed dotted double"),
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            if matches!(value, "0" | "none") {
                return true;
            }
            let parts: Vec<_> = value.split_whitespace().collect();
            parts.len() == 3
                && length(parts[0], false)
                && valid("border-style", parts[1])
                && color(parts[2])
        }
        // What remains of the declared properties is the margin and padding family.
        _ if known(name) => {
            let margin = name.starts_with("margin");
            let most = if matches!(name, "margin" | "padding") {
                4
            } else {
                1
            };
            let parts: Vec<_> = value.split_whitespace().collect();
            (1..=most).contains(&parts.len())
                && parts
                    .iter()
                    .all(|v| (margin && *v == "auto") || length(v, margin))
        }
        _ => false,
    }
}
pub(crate) fn length(value: &str, negative: bool) -> bool {
    if value == "0" {
        return true;
    }
    ["px", "pt", "mm", "cm", "in", "em", "rem", "%"]
        .iter()
        .any(|u| value.strip_suffix(u).is_some_and(|s| number(s, negative)))
}
fn number(value: &str, negative: bool) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+'))
        && value
            .parse::<f64>()
            .is_ok_and(|v| v.is_finite() && (negative || v >= 0.0))
}
fn color(value: &str) -> bool {
    listed(COLORS, value)
        || value.strip_prefix('#').is_some_and(|s| {
            matches!(s.len(), 3 | 4 | 6 | 8) && s.bytes().all(|c| c.is_ascii_hexdigit())
        })
}
