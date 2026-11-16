// SPDX-License-Identifier: MIT OR Apache-2.0
//! What an element's tag, classes and inline style mean for layout (design §5.1). The profile
//! validated every token before this runs; a token the document contract cannot express yet
//! (horizontal margins, one-sided padding, side borders, widths) is accepted and has no effect here.
use crate::{Alignment, TableLook};

/// A length that may depend on the font size around it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Size {
    Points(f64),
    /// Of the inherited font size.
    Em(f64),
    /// Of the document's base font size.
    Rem(f64),
}
impl Size {
    pub(crate) fn points(self, current: f64, base: f64) -> f64 {
        match self {
            Self::Points(v) => v,
            Self::Em(v) => v * current,
            Self::Rem(v) => v * base,
        }
    }
}
/// A text colour: fixed, or one of the two theme colours the caller supplies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Tint {
    Rgb([u8; 3]),
    Primary,
    Secondary,
}
/// The layout role a class gives its element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Role {
    #[default]
    Plain,
    /// A 12-column `row`.
    Row,
    /// `d-flex` in row direction: equal columns.
    Flex,
    Header,
    Footer,
    PageNumber,
    PageTotal,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct Look {
    pub(crate) hidden: bool,
    pub(crate) bold: Option<bool>,
    pub(crate) italic: Option<bool>,
    pub(crate) underline: bool,
    pub(crate) size: Option<Size>,
    pub(crate) color: Option<Tint>,
    pub(crate) align: Option<Alignment>,
    pub(crate) padding: Option<Size>,
    pub(crate) keep: bool,
    pub(crate) break_before: bool,
    pub(crate) break_after: bool,
    pub(crate) role: Role,
    /// Width in twelfths; `Some(0)` shares what is left.
    pub(crate) column: Option<u8>,
    /// Space between flex children, in points at the base size.
    pub(crate) gap: Option<Size>,
    /// A `colspan` or `rowspan` above one.
    pub(crate) spans: bool,
    pub(crate) icon: bool,
    /// Space before and after a block.
    pub(crate) above: Option<Size>,
    pub(crate) below: Option<Size>,
    pub(crate) fill: Option<[u8; 3]>,
    pub(crate) rule_above: bool,
    pub(crate) rule_below: bool,
    /// `table-bordered` / `table-borderless` on a `table`.
    pub(crate) table: Option<TableLook>,
    /// `align-items-*` on a `row` or a flex container: where its cells sit in the row.
    pub(crate) cross: Option<crate::CrossAlignment>,
}
/// Bootstrap's spacer scale, in `rem`.
const SPACERS: [f64; 6] = [0.0, 0.25, 0.5, 1.0, 1.5, 3.0];
const COLORS: &[(&str, [u8; 3])] = &[
    ("black", [0, 0, 0]),
    ("white", [255, 255, 255]),
    ("red", [255, 0, 0]),
    ("green", [0, 128, 0]),
    ("blue", [0, 0, 255]),
    ("gray", [128, 128, 128]),
    ("grey", [128, 128, 128]),
    ("yellow", [255, 255, 0]),
    ("orange", [255, 165, 0]),
    ("purple", [128, 0, 128]),
    ("navy", [0, 0, 128]),
    ("silver", [192, 192, 192]),
    ("maroon", [128, 0, 0]),
    ("teal", [0, 128, 128]),
    ("aqua", [0, 255, 255]),
    ("fuchsia", [255, 0, 255]),
    ("lime", [0, 255, 0]),
    ("olive", [128, 128, 0]),
];
impl Look {
    pub(crate) fn of<'a>(tag: &str, attrs: impl Iterator<Item = (&'a str, &'a str)>) -> Self {
        let mut look = Self::default();
        let mut flex = false;
        let mut column_direction = false;
        match tag {
            "strong" | "b" | "th" => look.bold = Some(true),
            "em" | "i" => look.italic = Some(true),
            "u" => look.underline = true,
            "small" => look.size = Some(Size::Em(0.875)),
            _ => {
                if let [b'h', level @ b'1'..=b'6'] = tag.as_bytes() {
                    look.bold = Some(true);
                    look.size = Some(Size::Rem(
                        [2.5, 2.0, 1.75, 1.5, 1.25, 1.0][usize::from(level - b'1')],
                    ));
                }
            }
        }
        for (name, value) in attrs {
            match name {
                "class" => {
                    for token in value.split_whitespace() {
                        match token {
                            "d-flex" | "d-inline-flex" => flex = true,
                            "flex-column" => column_direction = true,
                            _ => look.class(tag, token),
                        }
                    }
                }
                "style" => {
                    for item in value.split(';') {
                        if let Some((property, value)) = item.split_once(':') {
                            look.style(property.trim(), value.trim());
                        }
                    }
                }
                "colspan" | "rowspan" => {
                    look.spans |= value.trim().parse::<u32>().is_ok_and(|n| n > 1);
                }
                "data-icon" => look.icon = true,
                _ => {} // The other declared attributes carry no layout in this contract.
            }
        }
        if flex && !column_direction && look.role == Role::Plain {
            look.role = Role::Flex;
        }
        look
    }
    fn class(&mut self, tag: &str, token: &str) {
        let rgb = |r, g, b| Some(Tint::Rgb([r, g, b]));
        match token {
            "d-none" => self.hidden = true,
            "text-start" | "text-justify" => self.align = Some(Alignment::Start),
            "text-end" => self.align = Some(Alignment::End),
            "text-center" => self.align = Some(Alignment::Center),
            "fw-bold" => self.bold = Some(true),
            "fw-normal" | "fw-light" => self.bold = Some(false),
            "fst-italic" => self.italic = Some(true),
            "fst-normal" => self.italic = Some(false),
            "small" => self.size = Some(Size::Em(0.875)),
            "text-primary" => self.color = Some(Tint::Primary),
            "text-muted" | "text-secondary" => self.color = Some(Tint::Secondary),
            "text-success" => self.color = rgb(25, 135, 84),
            "text-danger" => self.color = rgb(220, 53, 69),
            "text-warning" => self.color = rgb(255, 193, 7),
            "text-info" => self.color = rgb(13, 202, 240),
            "text-dark" => self.color = rgb(33, 37, 41),
            "text-light" => self.color = rgb(248, 249, 250),
            "text-white" => self.color = rgb(255, 255, 255),
            "text-black" => self.color = rgb(0, 0, 0),
            "row" => self.role = Role::Row,
            "align-items-start" => self.cross = Some(crate::CrossAlignment::Start),
            "align-items-center" => self.cross = Some(crate::CrossAlignment::Center),
            "align-items-end" => self.cross = Some(crate::CrossAlignment::End),
            "header" if tag == "div" => self.role = Role::Header,
            "footer" if tag == "div" => self.role = Role::Footer,
            "page" if tag == "span" => self.role = Role::PageNumber,
            "topage" if tag == "span" => self.role = Role::PageTotal,
            "col" | "col-auto" => self.column = Some(0),
            "avoid-page-break-inside" => self.keep = true,
            "table-bordered" if tag == "table" => self.table = Some(TableLook::Bordered),
            "table-borderless" if tag == "table" => self.table = Some(TableLook::Borderless),
            "border-top" => self.rule_above = true,
            "border-bottom" => self.rule_below = true,
            "page-break-before" => self.break_before = true,
            _ => {
                let spacer = |n: &str| n.parse::<usize>().ok().and_then(|n| SPACERS.get(n));
                if let Some(n) = token
                    .strip_prefix("col-")
                    .and_then(|n| n.parse::<u8>().ok())
                {
                    self.column = Some(n);
                } else if let Some(v) = token.strip_prefix("p-").and_then(spacer) {
                    self.padding = Some(Size::Rem(*v));
                } else if let Some(v) = token.strip_prefix("gap-").and_then(spacer) {
                    self.gap = Some(Size::Rem(*v));
                } else if let Some((side, v)) = token
                    .split_once('-')
                    .and_then(|(side, n)| spacer(n).map(|v| (side, Size::Rem(*v))))
                {
                    match side {
                        "mt" => self.above = Some(v),
                        "mb" => self.below = Some(v),
                        "my" | "m" => (self.above, self.below) = (Some(v), Some(v)),
                        _ => {} // Horizontal margins have no place in the document contract.
                    }
                }
            }
        }
    }
    fn style(&mut self, property: &str, value: &str) {
        let breaks = |v: &str| matches!(v, "always" | "left" | "right");
        match property {
            "display" => self.hidden = value == "none",
            "font-size" => self.size = length(value, true).or(self.size),
            "font-weight" => {
                self.bold = Some(value == "bold" || value.parse::<u16>().is_ok_and(|n| n >= 600));
            }
            "color" => self.color = color(value).map(Tint::Rgb).or(self.color),
            "text-align" => {
                self.align = Some(match value {
                    "right" | "end" => Alignment::End,
                    "center" => Alignment::Center,
                    _ => Alignment::Start,
                });
            }
            "padding" => {
                let mut sides = value.split_whitespace();
                let first = sides.next();
                if let Some(first) = first.filter(|first| sides.all(|side| side == *first)) {
                    self.padding = length(first, false).or(self.padding);
                }
            }
            // A colour with transparency has no place yet: accepted, no effect (never an opaque fill).
            "background-color" if !translucent(value) => self.fill = color(value).or(self.fill),
            "margin-top" => self.above = length(value, false).or(self.above),
            "margin-bottom" => self.below = length(value, false).or(self.below),
            "margin" => {
                // CSS order: top, right, bottom, left; one or two values give the bottom the top's.
                let sides: Vec<&str> = value.split_whitespace().collect();
                let top = sides.first().and_then(|v| length(v, false));
                let bottom = sides.get(2).map_or(top, |v| length(v, false));
                (self.above, self.below) = (top.or(self.above), bottom.or(self.below));
            }
            "border-top" => self.rule_above = draws(value),
            "border-bottom" => self.rule_below = draws(value),
            "page-break-before" => self.break_before = breaks(value),
            "page-break-after" => self.break_after = breaks(value),
            "page-break-inside" => self.keep = value == "avoid",
            _ => {} // Declared properties without a place in the document contract yet.
        }
    }
}
/// Whether a `border` shorthand draws a line: not when its width is 0, its style `none` or `hidden`,
/// or its colour `transparent`.
fn draws(value: &str) -> bool {
    !value.split_whitespace().any(|part| {
        matches!(part, "none" | "hidden" | "transparent")
            || length(part, false).is_some_and(|size| size == Size::Points(0.0))
    })
}
/// A `#rgba` or `#rrggbbaa` colour that is not fully opaque, or `transparent`.
fn translucent(value: &str) -> bool {
    match value.strip_prefix('#') {
        Some(hex) if hex.len() == 4 => !hex[3..].eq_ignore_ascii_case("f"),
        Some(hex) if hex.len() == 8 => !hex[6..].eq_ignore_ascii_case("ff"),
        _ => value == "transparent",
    }
}
/// A CSS length as points or as a share of a font size; a percentage only where it means one.
pub(crate) fn length(value: &str, percent_of_font: bool) -> Option<Size> {
    if value == "0" {
        return Some(Size::Points(0.0));
    }
    let number = |suffix: &str| {
        value
            .strip_suffix(suffix)
            .and_then(|n| n.parse::<f64>().ok())
            .filter(|n| n.is_finite() && *n >= 0.0)
    };
    // `rem` before `em`: one is a suffix of the other.
    if let Some(n) = number("rem") {
        return Some(Size::Rem(n));
    }
    if let Some(n) = number("em") {
        return Some(Size::Em(n));
    }
    if let Some(n) = number("%") {
        return percent_of_font.then_some(Size::Em(n / 100.0));
    }
    [
        ("px", 0.75),
        ("pt", 1.0),
        ("mm", 72.0 / 25.4),
        ("cm", 72.0 / 2.54),
        ("in", 72.0),
    ]
    .iter()
    .find_map(|(unit, factor)| number(unit).map(|n| Size::Points(n * factor)))
}
fn color(value: &str) -> Option<[u8; 3]> {
    if let Some((_, rgb)) = COLORS.iter().find(|(name, _)| *name == value) {
        return Some(*rgb);
    }
    let hex = value.strip_prefix('#')?;
    let digit = |i: usize| u8::from_str_radix(hex.get(i..=i)?, 16).ok();
    match hex.len() {
        3 | 4 => Some([digit(0)? * 17, digit(1)? * 17, digit(2)? * 17]),
        6 | 8 => Some([
            digit(0)? * 16 + digit(1)?,
            digit(2)? * 16 + digit(3)?,
            digit(4)? * 16 + digit(5)?,
        ]),
        _ => None,
    }
}
