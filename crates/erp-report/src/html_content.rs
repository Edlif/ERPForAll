// SPDX-License-Identifier: MIT OR Apache-2.0
//! User HTML mapping. No XML compiler, network client, filesystem or database capability.
use super::*;
use crate::{HtmlSource, ReportWarning, ReportWarningKind as W, RichText};
use erp_html::{Fragment, FragmentKind, FragmentNode, HtmlSanitizeError};
use std::collections::BTreeSet;

struct Frame<'a> {
    node: FragmentNode<'a>,
    children: std::vec::IntoIter<FragmentNode<'a>>,
    tag: String,
    own: Own,
    look: Look,
    kids: Vec<Item>,
    flatten: bool,
    transform: u8,
}
impl Run<'_> {
    fn warning(
        &mut self,
        kind: W,
        source: &HtmlSource,
        at: &At,
        node: usize,
    ) -> Result<(), ReportError> {
        self.charge(std::mem::size_of::<ReportWarning>() + source.bytes() + at.template.len())?;
        let warning = ReportWarning {
            kind,
            source: source.clone(),
            template: at.template.to_string(),
            line: at.line,
            node,
        };
        self.charge(
            warning.bytes()
                - std::mem::size_of::<ReportWarning>()
                - source.bytes()
                - at.template.len(),
        )?;
        self.warnings.push(warning);
        Ok(())
    }
    pub(super) fn html_content(
        &mut self,
        input: &str,
        source: &HtmlSource,
        inherit: &Inherit,
        at: &At,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        self.charge(input.len())?;
        let limits = erp_html::Limits::ceiling()
            .with_input_bytes(self.program.limits.html_bytes as u64)
            .and_then(|l| l.with_nodes(self.program.limits.html_nodes as u64))
            .map_err(|_| at.evaluate("invalid HTML parser configuration"))?;
        let parsed = match Fragment::parse(input, &limits) {
            Ok(parsed) => parsed,
            Err(HtmlSanitizeError::ParseFailed(_) | HtmlSanitizeError::NulByte { .. }) => {
                self.warning(W::HtmlParseFailed, source, at, 0)?;
                return self.text(out, input);
            }
            Err(error) => {
                return Err(match error {
                    HtmlSanitizeError::InputTooLarge { limit, .. } => {
                        html_limit("html_bytes", limit)
                    }
                    HtmlSanitizeError::NodeLimit { limit } => html_limit("html_nodes", limit),
                    HtmlSanitizeError::DepthExceeded { limit } => {
                        html_limit("html_parser_depth", limit)
                    }
                    HtmlSanitizeError::WorkLimit { limit } => html_limit("html_work", limit),
                    HtmlSanitizeError::MemoryLimit { limit } => html_limit("html_memory", limit),
                    other => at.evaluate(format!("HTML parser: {other}")),
                });
            }
        };
        // Template wrappers and document root still have to be added after this fragment. Reserve
        // two levels per active template operation (including cells/regions) and two for the root.
        let room = self
            .options
            .limits
            .get(crate::LimitKind::Depth)
            .saturating_sub(self.nesting.saturating_mul(2) + 4);
        let mut ignored = BTreeSet::new();
        let mut stack = vec![self.html_frame(
            parsed.root(),
            inherit,
            RichText::default(),
            0,
            0,
            room,
            source,
            at,
            &mut ignored,
        )?];
        loop {
            self.step()?;
            let Some(frame) = stack.last_mut() else {
                break;
            };
            if let Some(child) = frame.children.next() {
                let inherit = frame.own.inherit;
                let mut rich = frame.own.rich.clone();
                rich.link.clear(); // A link wraps its children once; it is not an inherited annotation.
                let transform = frame.transform;
                let depth = stack.len();
                let next = self.html_frame(
                    child,
                    &inherit,
                    rich,
                    transform,
                    depth,
                    room,
                    source,
                    at,
                    &mut ignored,
                )?;
                stack.push(next);
                continue;
            }
            let frame = stack
                .pop()
                .ok_or_else(|| at.evaluate("HTML traversal lost its frame"))?;
            let mut items = Vec::new();
            match frame.node.kind() {
                FragmentKind::Text(text) => {
                    let transformed = match frame.transform {
                        1 => text.to_uppercase(),
                        2 => text.to_lowercase(),
                        _ => text.into(),
                    };
                    self.charge(transformed.len())?;
                    if frame.own.rich.pre {
                        self.node(Node::Text(transformed), false, &mut items)?;
                    } else {
                        self.text(&mut items, &transformed)?;
                    }
                }
                FragmentKind::Ignored => {}
                _ if frame.tag == "img" && !frame.flatten => {
                    self.html_image(frame.node, source, at, &mut items)?
                }
                _ if frame.flatten => extend(&mut items, loosen(frame.kids)),
                _ => {
                    if frame.tag == "table" && frame.kids.iter().any(|item| matches!(item,
                        Item::Row { cells, .. } if cells.iter().map(|(_, span)| span).sum::<usize>() > MAX_COLSPAN)) {
                        return Err(html_limit("html_table_columns", MAX_COLSPAN as u64));
                    }
                    if !valid_children(&frame.tag, &frame.kids) {
                        self.warning(
                            W::HtmlElementUnwrapped { element: frame.tag },
                            source,
                            at,
                            frame.node.index(),
                        )?;
                        items = loosen(frame.kids);
                    } else {
                        let kids = normalize_children(&frame.tag, frame.kids);
                        self.place(
                            &frame.tag,
                            &frame.look,
                            &frame.own,
                            &frame.own.inherit,
                            kids,
                            at,
                            &mut items,
                        )?;
                    }
                }
            }
            if let Some(parent) = stack.last_mut() {
                extend(&mut parent.kids, items);
            } else {
                extend(out, loosen(items));
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn html_frame<'a>(
        &mut self,
        node: FragmentNode<'a>,
        inherit: &Inherit,
        mut rich: RichText,
        mut transform: u8,
        depth: usize,
        room: usize,
        source: &HtmlSource,
        at: &At,
        ignored: &mut BTreeSet<String>,
    ) -> Result<Frame<'a>, ReportError> {
        self.charge(
            std::mem::size_of::<Frame<'a>>()
                + node.children().count() * std::mem::size_of::<FragmentNode<'a>>(),
        )?;
        let mut tag = "t";
        let mut flatten = depth >= room;
        let mut attrs = Vec::<(String, String)>::new();
        let mut italic = None;
        let mut underline = false;
        if let FragmentKind::Element { name, namespace } = node.kind() {
            let generated = depth == 1 && name == "html";
            let known = namespace == "http://www.w3.org/1999/xhtml" && supported(name);
            if known {
                tag = name;
            } else if !generated {
                self.warning(
                    W::HtmlElementUnwrapped {
                        element: name.into(),
                    },
                    source,
                    at,
                    node.index(),
                )?;
            }
            if flatten && known {
                self.warning(W::HtmlFlattened { depth }, source, at, node.index())?;
            }
            for attr in node.attributes() {
                let (key, value) = (attr.name, attr.value);
                self.charge(key.len() + value.len())?;
                if !attr.namespace.is_empty() {
                    self.warning(
                        W::HtmlAttributeDropped {
                            element: name.into(),
                            attribute: format!("{{{}}}{key}", attr.namespace),
                        },
                        source,
                        at,
                        node.index(),
                    )?;
                    continue;
                }
                match key {
                    "class" => {
                        let mut kept = String::new();
                        for token in value.split_whitespace() {
                            if mapped_class(token, name) {
                                kept.push_str(token);
                                kept.push(' ');
                            } else if !ignored.contains(token) {
                                self.charge(
                                    token.len()
                                        + std::mem::size_of::<String>()
                                        + 6 * std::mem::size_of::<usize>(),
                                )?;
                                ignored.insert(token.into());
                                self.warning(
                                    W::HtmlClassIgnored {
                                        class: token.into(),
                                    },
                                    source,
                                    at,
                                    node.index(),
                                )?;
                            }
                        }
                        attrs.push((key.into(), kept));
                    }
                    "style" => {
                        let mut kept = String::new();
                        for part in value.split(';').filter(|p| !p.trim().is_empty()) {
                            let (property, value) = part.split_once(':').unwrap_or((part, ""));
                            let property = property.trim().to_ascii_lowercase();
                            let value = value.trim();
                            if property == "font-style"
                                && matches!(value, "italic" | "normal" | "oblique")
                            {
                                italic = Some(value != "normal");
                                continue;
                            }
                            if property == "text-decoration" && value == "underline" {
                                underline = true;
                                continue;
                            }
                            if extra_style(&property, value, &mut rich, &mut transform) {
                                continue;
                            }
                            if mapped_style(&property)
                                && crate::style::valid(&property, value)
                                && representable(&property, value)
                                && (is_block(tag)
                                    || matches!(tag, "td" | "th" | "li" | "blockquote" | "pre")
                                    || !matches!(
                                        property.as_str(),
                                        "background-color"
                                            | "padding"
                                            | "margin-top"
                                            | "margin-bottom"
                                    ))
                                && (property != "font-size"
                                    || safe_length(&property, value, inherit.text.size, self.base))
                            {
                                kept.push_str(&property);
                                kept.push(':');
                                kept.push_str(value);
                                kept.push(';');
                            } else {
                                self.warning(
                                    W::HtmlStyleDropped {
                                        property,
                                        value: value.into(),
                                    },
                                    source,
                                    at,
                                    node.index(),
                                )?;
                            }
                        }
                        attrs.push((key.into(), kept));
                    }
                    "href" if name == "a" => match rich.clone().with_link(value) {
                        Ok(next) if !value.is_empty() => rich = next,
                        _ => self.warning(
                            W::LinkDropped {
                                scheme: value
                                    .split_once(':')
                                    .map_or("relative", |(s, _)| s)
                                    .to_ascii_lowercase(),
                            },
                            source,
                            at,
                            node.index(),
                        )?,
                    },
                    "color" | "size" | "face" if name == "font" => {
                        let mapped = match key {
                            "face" => match rich.clone().with_font(value.trim_matches(['\'', '"']))
                            {
                                Ok(next) => {
                                    rich = next;
                                    None
                                }
                                Err(_) => Some(("font-family", value.to_owned())),
                            },
                            "color" => Some(("color", value.to_owned())),
                            _ => Some(("font-size", font_size(value))),
                        };
                        if let Some((property, v)) = mapped {
                            if property != "font-family"
                                && crate::style::valid(property, &v)
                                && representable(property, &v)
                                && safe_length(property, &v, inherit.text.size, self.base)
                            {
                                attrs.push(("style".into(), format!("{property}:{v}")));
                            } else {
                                self.warning(
                                    W::HtmlAttributeDropped {
                                        element: name.into(),
                                        attribute: key.into(),
                                    },
                                    source,
                                    at,
                                    node.index(),
                                )?;
                            }
                        }
                    }
                    "src" | "alt" | "width" | "height" if name == "img" => {}
                    "colspan"
                        if matches!(name, "td" | "th")
                            && value
                                .parse::<usize>()
                                .is_ok_and(|n| (1..=MAX_COLSPAN).contains(&n)) =>
                    {
                        attrs.push((key.into(), value.into()))
                    }
                    "dir" if matches!(value, "ltr" | "rtl") => rich.rtl = Some(value == "rtl"),
                    _ => self.warning(
                        W::HtmlAttributeDropped {
                            element: name.into(),
                            attribute: key.into(),
                        },
                        source,
                        at,
                        node.index(),
                    )?,
                }
            }
            match name {
                "s" => rich.strike = true,
                "sub" => rich.baseline = -1,
                "sup" => rich.baseline = 1,
                "pre" => {
                    rich.pre = true;
                    rich.font = "Noto Sans Mono".into();
                    tag = "div";
                }
                "blockquote" => {
                    tag = "div";
                    attrs.push(("style".into(), "padding:12pt".into()));
                }
                _ => {}
            }
            flatten |= !known && !generated;
        }
        let preliminary = Look::of(tag, attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let computed_font = self.own(tag, &preliminary, inherit).inherit.text.size;
        let font = if (1.0..=200.0).contains(&computed_font) {
            computed_font
        } else {
            inherit.text.size
        };
        for (key, style) in &mut attrs {
            if key != "style" {
                continue;
            }
            let mut kept = String::new();
            for part in style.split(';').filter(|p| !p.is_empty()) {
                let Some((property, value)) = part.split_once(':') else {
                    continue;
                };
                if matches!(property, "padding" | "margin-top" | "margin-bottom")
                    && !value
                        .split_whitespace()
                        .all(|v| safe_length(property, v, font, self.base))
                {
                    self.warning(
                        W::HtmlStyleDropped {
                            property: property.into(),
                            value: value.into(),
                        },
                        source,
                        at,
                        node.index(),
                    )?;
                } else {
                    kept.push_str(part);
                    kept.push(';');
                }
            }
            *style = kept;
        }
        let mut look = Look::of(tag, attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        if !(1.0..=200.0).contains(&computed_font) {
            self.warning(
                W::HtmlStyleDropped {
                    property: "font-size".into(),
                    value: format!("{computed_font}pt"),
                },
                source,
                at,
                node.index(),
            )?;
            look.size = None;
        }
        if italic.is_some() {
            look.italic = italic;
        }
        look.underline |= underline;
        let mut own = self.own(tag, &look, inherit);
        // No accepted user style may produce a later document validation error.
        own.rich = rich;
        own.text_changed |= own.rich != RichText::default();
        Ok(Frame {
            node,
            children: node.children().collect::<Vec<_>>().into_iter(),
            tag: tag.into(),
            own,
            look,
            kids: Vec::new(),
            flatten,
            transform,
        })
    }
    fn html_image(
        &mut self,
        node: FragmentNode<'_>,
        source: &HtmlSource,
        at: &At,
        out: &mut Vec<Item>,
    ) -> Result<(), ReportError> {
        let attr = |name| {
            node.attributes()
                .find(|a| a.namespace.is_empty() && a.name == name)
                .map(|a| a.value)
        };
        let src = attr("src").unwrap_or("");
        use crate::html_images::{ImageOutcome, admit, classify, degraded};
        let prepared = self.records.html_images.get(source, node.index());
        let outcome = match prepared.unwrap_or_else(|| classify(src, &self.options.limits)) {
            // Recheck with the evaluation's limits: a caller may lower them after loading.
            ImageOutcome::Ready(bytes) => admit(bytes, &self.options.limits),
            ImageOutcome::Local(_) => degraded("local HTML image was not loaded by the data stage"),
            other => other,
        };
        let side = |name| {
            attr(name)
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|n| n.is_finite() && *n > 0.0 && *n <= 19_200.0)
                .map_or(PLACEHOLDER_POINTS, |n| n * POINTS_PER_PIXEL)
        };
        let alt = attr("alt").unwrap_or("").to_owned();
        match outcome {
            ImageOutcome::Ready(data) => {
                let dimension = |name| attr(name).map(|_| side(name));
                let warning = ReportWarning {
                    kind: W::ImageDegraded {
                        reason: String::new(),
                    },
                    source: source.clone(),
                    template: at.template.to_string(),
                    line: at.line,
                    node: node.index(),
                };
                self.charge(warning.bytes())?;
                let id = self.builder.add(Node::Image {
                    data,
                    width: dimension("width"),
                    height: dimension("height"),
                    fit: ImageFit::Contain,
                    alt,
                })?;
                self.image_origins.push((id.index(), warning));
                out.push(Item::Node {
                    id,
                    block: false,
                    column: None,
                });
                Ok(())
            }
            ImageOutcome::Warning(kind) => {
                self.warning(kind, source, at, node.index())?;
                self.node(
                    Node::Placeholder {
                        width: side("width"),
                        height: side("height"),
                        alt,
                        reason: "HTML image placeholder".into(),
                    },
                    false,
                    out,
                )
            }
            ImageOutcome::Local(_) => Err(at.evaluate("HTML image resolution is incomplete")),
        }
    }
}
fn html_limit(name: &'static str, limit: u64) -> ReportError {
    ReportError::Limit {
        name,
        maximum: usize::try_from(limit).unwrap_or(usize::MAX),
    }
}
fn supported(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "span"
            | "br"
            | "strong"
            | "b"
            | "em"
            | "i"
            | "u"
            | "s"
            | "small"
            | "sub"
            | "sup"
            | "ul"
            | "ol"
            | "li"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "th"
            | "td"
            | "hr"
            | "blockquote"
            | "pre"
            | "a"
            | "img"
            | "font"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}
fn mapped_class(token: &str, tag: &str) -> bool {
    (matches!(
        token,
        "text-start"
            | "text-end"
            | "text-center"
            | "text-primary"
            | "text-muted"
            | "text-secondary"
            | "text-success"
            | "text-danger"
            | "text-warning"
            | "text-info"
            | "text-dark"
            | "text-light"
            | "text-white"
            | "text-black"
    ) || matches!(token, "fw-bold" | "fw-normal")
        || (tag == "table" && matches!(token, "table" | "table-bordered" | "table-borderless")))
        && crate::class_meaning(token).is_some()
}
fn mapped_style(property: &str) -> bool {
    matches!(
        property,
        "font-size"
            | "font-weight"
            | "color"
            | "background-color"
            | "text-align"
            | "padding"
            | "margin-top"
            | "margin-bottom"
            | "border-top"
            | "border-bottom"
    )
}
fn representable(property: &str, value: &str) -> bool {
    match property {
        "padding" => {
            let mut sides = value.split_whitespace();
            sides.next().is_some_and(|first| {
                look::length(first, false).is_some() && sides.all(|v| v == first)
            })
        }
        "font-size" | "margin-top" | "margin-bottom" => {
            look::length(value, property == "font-size").is_some()
        }
        "color" | "background-color" => match value.strip_prefix('#') {
            Some(v) if v.len() == 4 => v[3..].eq_ignore_ascii_case("f"),
            Some(v) if v.len() == 8 => v[6..].eq_ignore_ascii_case("ff"),
            _ => value != "transparent",
        },
        "text-align" => value != "justify",
        "font-weight" => matches!(value, "normal" | "bold" | "400" | "700"),
        "border-top" | "border-bottom" => matches!(value, "none" | "0"),
        _ => true,
    }
}
fn safe_length(property: &str, value: &str, current: f64, base: f64) -> bool {
    if let Some(length) = look::length(value, property == "font-size") {
        let n = length.points(current, base);
        n.is_finite()
            && if property == "font-size" {
                (1.0..=200.0).contains(&n)
            } else {
                (0.0..=1440.0).contains(&n)
            }
    } else {
        true
    }
}
fn font_size(value: &str) -> String {
    let size = value.parse::<usize>().ok().filter(|n| (1..=7).contains(n));
    size.map_or_else(
        || value.into(),
        |n| format!("{}pt", [8, 10, 12, 14, 18, 24, 36][n - 1]),
    )
}
fn extra_style(property: &str, value: &str, rich: &mut RichText, transform: &mut u8) -> bool {
    match (property, value) {
        ("text-decoration", "none") => {
            rich.strike = false;
            true
        }
        ("text-decoration", "line-through") => {
            rich.strike = true;
            true
        }
        ("font-family", value) => match rich.clone().with_font(value.trim_matches(['\'', '"'])) {
            Ok(next) => {
                *rich = next;
                true
            }
            Err(_) => false,
        },
        ("direction", "rtl" | "ltr") => {
            rich.rtl = Some(value == "rtl");
            true
        }
        ("white-space", "pre" | "pre-wrap" | "normal") => {
            rich.pre = value != "normal";
            true
        }
        ("text-transform", "uppercase" | "lowercase" | "none") => {
            *transform = match value {
                "uppercase" => 1,
                "lowercase" => 2,
                _ => 0,
            };
            true
        }
        ("letter-spacing", value) => {
            let n = value
                .strip_suffix("pt")
                .and_then(|v| v.parse::<f64>().ok())
                .or_else(|| {
                    value
                        .strip_suffix("px")
                        .and_then(|v| v.parse::<f64>().ok())
                        .map(|v| v * 0.75)
                });
            match n.and_then(|n| rich.clone().with_tracking(n).ok()) {
                Some(next) => {
                    *rich = next;
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
}
fn loosen(items: Vec<Item>) -> Vec<Item> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Cell(id, _) | Item::Entry(id) => out.push(Item::Node {
                id,
                block: true,
                column: None,
            }),
            Item::Row { cells, .. } => out.extend(cells.into_iter().map(|(id, _)| Item::Node {
                id,
                block: true,
                column: None,
            })),
            other => out.push(other),
        }
    }
    out
}
fn normalize_children(tag: &str, items: Vec<Item>) -> Vec<Item> {
    if matches!(
        tag,
        "table" | "thead" | "tbody" | "tfoot" | "tr" | "ul" | "ol"
    ) {
        items
    } else {
        loosen(items)
    }
}

fn valid_children(tag: &str, items: &[Item]) -> bool {
    items.iter().all(|item| {
        if matches!(item, Item::Text(text) if blank(text)) {
            return true;
        }
        match tag {
            "ul" | "ol" => matches!(item, Item::Entry(_)),
            "tr" => matches!(item, Item::Cell(..)),
            "table" | "thead" | "tbody" | "tfoot" => matches!(item, Item::Row { .. }),
            _ => true,
        }
    })
}
