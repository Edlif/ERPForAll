// SPDX-License-Identifier: MIT OR Apache-2.0
//! Closed print vocabulary. Adding a token requires an explicit semantic mapping, not a wildcard.
//! Each table is a space-separated list of exact tokens.
/// Semantic classification usable by the later renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenMeaning {
    Structure,
    Typography,
    Spacing,
    Grid,
    Flex,
    Table,
    Border,
    Visibility,
    Pagination,
    Icon,
    NoOp,
}
/// A supported print-profile version; `new` is the only way to obtain one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintProfile(());
impl PrintProfile {
    /// Select a supported profile; any version other than 1 is refused by number.
    pub fn new(version: u32) -> Result<Self, crate::ReportError> {
        if version != 1 {
            return Err(crate::ReportError::Version(version));
        }
        Ok(Self(()))
    }
    /// Canonical profile version.
    pub const fn version(self) -> u32 {
        1
    }
}
/// Exact membership in a space-separated table; an empty token is never a member.
pub(crate) fn listed(table: &str, token: &str) -> bool {
    !token.is_empty() && table.split(' ').any(|entry| entry == token)
}
const ELEMENTS: &str = "t div span p a table thead tbody tfoot tr th td strong b em i u small br \
    hr img address ul ol li section main";
/// An element of the profile: a listed one, or a heading of level 1 to 6.
pub(crate) fn element(tag: &str) -> bool {
    listed(ELEMENTS, tag) || matches!(tag.as_bytes(), [b'h', b'1'..=b'6'])
}
pub(crate) const DIRECTIVES: &str = "t-name t-foreach t-as t-if t-elif t-else t-set t-value t-esc \
    t-out t-field t-options t-call t-att t-lang t-translation t-tag t-ignore";
pub(crate) const ICONS: &str = "location_on phone smartphone public mail";
const CLASSES: &[(TokenMeaning, &str)] = &[
    (
        TokenMeaning::Structure,
        "header footer article page oe_structure",
    ),
    (
        TokenMeaning::Typography,
        "text-start text-end text-center text-justify text-muted text-primary text-secondary \
         text-success text-danger text-warning text-info text-dark text-light text-white \
         text-black fw-bold fw-normal fw-light fst-italic fst-normal text-nowrap text-wrap \
         text-break small lh-1 lh-sm lh-base lh-lg o_force_ltr",
    ),
    (
        TokenMeaning::Grid,
        "row col col-auto w-25 w-50 w-75 w-100 mw-100",
    ),
    (
        TokenMeaning::Flex,
        "d-flex d-inline-flex flex-row flex-column flex-wrap flex-nowrap justify-content-start \
         justify-content-end justify-content-center justify-content-between \
         justify-content-around align-items-start align-items-end align-items-center \
         align-items-baseline align-items-stretch",
    ),
    (
        TokenMeaning::Table,
        "table table-sm table-borderless table-bordered table-striped",
    ),
    (
        TokenMeaning::Border,
        "border border-0 border-top border-bottom border-start border-end rounded rounded-0",
    ),
    (
        TokenMeaning::Visibility,
        "d-none d-block d-inline d-inline-block",
    ),
    (
        TokenMeaning::Pagination,
        "topage avoid-page-break-inside page-break-before",
    ),
    (TokenMeaning::Icon, "oi oi-fw oi-filled"),
    // Responsive variants have no meaning on paper: accepted, mapped to nothing (design §5.1).
    (
        TokenMeaning::NoOp,
        "d-md-none d-md-block d-md-inline d-md-inline-block d-md-table-cell d-lg-none d-lg-block",
    ),
];
const SPACING: &str = "m mt mb ms me mx my p pt pb ps pe px py gap";
/// Every accepted class maps to one declared print meaning, including explicit no-ops.
pub fn class_meaning(token: &str) -> Option<TokenMeaning> {
    if let Some((meaning, _)) = CLASSES.iter().find(|(_, table)| listed(table, token)) {
        return Some(*meaning);
    }
    if token
        .strip_prefix("col-")
        .is_some_and(|n| number_in(n, 1, 12))
    {
        return Some(TokenMeaning::Grid);
    }
    let (prefix, value) = token.rsplit_once('-')?;
    let auto = value == "auto" && prefix.starts_with('m');
    (listed(SPACING, prefix) && (auto || number_in(value, 0, 5))).then_some(TokenMeaning::Spacing)
}
fn number_in(text: &str, min: u8, max: u8) -> bool {
    text.parse::<u8>()
        .is_ok_and(|n| (min..=max).contains(&n) && n.to_string() == text)
}
const ANY_ELEMENT: &str = "class style id title lang dir itemprop itemscope itemtype role";
const ROOT_OVERRIDES: &str = "data-report-margin-top data-report-margin-bottom \
    data-report-margin-left data-report-margin-right data-report-header-spacing data-report-dpi \
    data-report-landscape";
pub(crate) fn attribute(tag: &str, name: &str) -> bool {
    match name {
        "src" | "alt" => tag == "img",
        "href" => tag == "a",
        "colspan" | "rowspan" => matches!(tag, "td" | "th"),
        "width" | "height" => matches!(tag, "img" | "td" | "th"),
        "data-icon" => tag == "i",
        // The declared namespaces, not catch-all HTML attributes. An evaluated
        // mapping key never passed the XML parser, so the rest of the name is checked here.
        _ => {
            let plain = |rest: &str| {
                !rest.is_empty()
                    && rest
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_".contains(&c))
            };
            listed(ANY_ELEMENT, name)
                || listed(ROOT_OVERRIDES, name)
                || ["data-oe-", "aria-"]
                    .iter()
                    .any(|p| name.strip_prefix(p).is_some_and(plain))
        }
    }
}
/// User HTML degradation vocabulary; the converter is a later consumer, never invoked by validation.
pub const HTML_DEGRADATION_RULES: &[(&str, &str)] = &[
    ("unknown element", "HtmlElementUnwrapped"),
    ("unknown attribute", "HtmlAttributeDropped"),
    ("unknown or invalid style", "HtmlStyleDropped"),
    ("unmapped class", "HtmlClassIgnored"),
    ("external image", "ExternalImageNotFetched"),
    ("invalid or oversized image", "ImageDegraded"),
    ("unsafe link", "LinkDropped"),
    ("excess depth", "HtmlFlattened"),
    ("invalid HTML", "HtmlParseFailed"),
];
