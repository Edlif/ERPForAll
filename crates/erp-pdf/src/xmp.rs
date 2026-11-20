// SPDX-License-Identifier: MIT OR Apache-2.0
//! Namespace-aware, subject-scoped XMP edits. Untouched byte ranges are never serialized again.
use crate::error::{Result, malformed, refused};
use crate::limits::Context;
use crate::{Limit, MetadataField, PdfaId};
use roxmltree::{Attribute, Document, Node};
use std::ops::Range;

pub(crate) const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub(crate) const PDFAID: &str = "http://www.aiim.org/pdfa/ns/id/";
pub(crate) const EXTENSION: &str = "http://www.aiim.org/pdfa/ns/extension/";
pub(crate) const SCHEMA: &str = "http://www.aiim.org/pdfa/ns/schema#";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
pub(crate) const EMPTY: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n</rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";

pub(crate) fn parse<'a>(text: &'a str, l: &Context<'_>) -> Result<Document<'a>> {
    l.check(Limit::MetadataBytes, text.len())?;
    let doc = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: u32::try_from(l.get(Limit::WorkUnits))
                .map_err(|_| malformed("PDF XMP node limit overflow"))?,
            ..Default::default()
        },
    )
    .map_err(|e| match e {
        roxmltree::Error::DtdDetected => refused("XMP DTD/entity").source(e),
        _ => malformed("PDF malformed XMP").source(e),
    })?;
    for n in doc.descendants() {
        l.step()?;
        l.check(Limit::Depth, n.ancestors().count())?;
    }
    Ok(doc)
}
fn is(n: Node<'_, '_>, ns: &str, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(ns) && n.tag_name().name() == local
}
fn qname(text: &str, n: Node<'_, '_>, l: &Context<'_>) -> Result<String> {
    let raw = text
        .get(n.range())
        .and_then(|s| s.strip_prefix('<'))
        .ok_or_else(|| malformed("PDF XMP invalid element range"))?;
    let end = raw
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .ok_or_else(|| malformed("PDF XMP invalid start tag"))?;
    joined(&[&raw[..end]], l)
}
/// End of a validated opening tag, respecting literal `>` inside quoted attributes.
fn start_tag_end(text: &str, start: usize) -> Result<usize> {
    let mut quote = None;
    for (offset, b) in text.as_bytes()[start..].iter().copied().enumerate() {
        match (quote, b) {
            (Some(q), b) if q == b => quote = None,
            (None, b'\'' | b'"') => quote = Some(b),
            (None, b'>') => return Ok(start + offset + 1),
            _ => {}
        }
    }
    Err(malformed("PDF XMP element has no start-tag end"))
}
#[derive(Clone, Copy)]
enum Place<'a, 'i> {
    Attribute(Attribute<'a, 'i>),
    Element(Node<'a, 'i>),
}
impl Place<'_, '_> {
    fn named(self, ns: &str, local: &str) -> bool {
        match self {
            Self::Attribute(a) => a.namespace() == Some(ns) && a.name() == local,
            Self::Element(n) => is(n, ns, local),
        }
    }
}
/// Only direct properties of the selected document subject; nested resources remain opaque.
pub(crate) struct Packet<'a, 'i, 'l> {
    root: Node<'a, 'i>,
    about: String,
    places: Vec<Place<'a, 'i>>,
    l: &'a Context<'l>,
}
impl<'a, 'i, 'l> Packet<'a, 'i, 'l> {
    pub(crate) fn new(doc: &'a Document<'i>, l: &'a Context<'l>) -> Result<Self> {
        let mut roots = Vec::new();
        let mut descriptions = Vec::new();
        for n in doc.descendants() {
            l.step()?;
            if is(n, RDF, "RDF") && !n.ancestors().skip(1).any(|p| is(p, RDF, "RDF")) {
                l.copied(std::mem::size_of_val(&n))?;
                roots.push(n);
                for d in n.children().filter(|c| is(*c, RDF, "Description")) {
                    l.step()?;
                    if d.attribute((RDF, "nodeID")).is_some()
                        || d.attribute((RDF, "ID")).is_some()
                        || d.ancestors().any(|a| a.attribute((XML, "base")).is_some())
                    {
                        return Err(refused("XMP indirect document subject"));
                    }
                    l.copied(std::mem::size_of_val(&d))?;
                    descriptions.push(d);
                }
            }
        }
        let first = *roots
            .first()
            .ok_or_else(|| malformed("PDF XMP lacks rdf:RDF"))?;
        // Absent rdf:about means the document, as does an explicitly empty about.
        let subject = |d: Node<'a, 'i>| d.attribute((RDF, "about")).unwrap_or("");
        let about = if descriptions.iter().any(|d| subject(*d).is_empty()) {
            ""
        } else if let Some(d) = descriptions.first() {
            let candidate = subject(*d);
            if descriptions.iter().any(|d| subject(*d) != candidate) {
                return Err(malformed("PDF XMP document subject is ambiguous"));
            }
            candidate
        } else {
            ""
        };
        let mut places = Vec::new();
        let mut target = None;
        for d in descriptions.into_iter().filter(|d| subject(*d) == about) {
            if target.is_none() {
                target = d.parent();
            }
            for a in d.attributes() {
                l.step()?;
                if a.namespace().is_some_and(|ns| ns != RDF && ns != XML) {
                    l.copied(std::mem::size_of::<Place<'_, '_>>())?;
                    places.push(Place::Attribute(a));
                }
            }
            for n in d.children().filter(Node::is_element) {
                l.step()?;
                l.copied(std::mem::size_of::<Place<'_, '_>>())?;
                places.push(Place::Element(n));
            }
        }
        Ok(Self {
            root: target.unwrap_or(first),
            about: joined(&[about], l)?,
            places,
            l,
        })
    }
    pub(crate) fn read(&self, ns: &str, local: &str) -> Result<Option<String>> {
        let mut result: Option<String> = None;
        for p in &self.places {
            self.l.step()?;
            if !p.named(ns, local) {
                continue;
            }
            let current = match p {
                Place::Attribute(a) => joined(&[a.value()], self.l)?,
                Place::Element(n) => value(*n, self.l)?,
            };
            if result.as_ref().is_some_and(|old| old != &current) {
                return Err(malformed("PDF XMP conflicting document properties"));
            }
            result = Some(current);
        }
        Ok(result)
    }
    pub(crate) fn pdfa(&self) -> Result<Option<PdfaId>> {
        let part = self.read(PDFAID, "part")?;
        let conformance = self.read(PDFAID, "conformance")?;
        let Some(part) = part else {
            return Ok(None);
        };
        let part = part
            .trim()
            .parse()
            .map_err(|e| malformed("PDF XMP pdfaid:part must be a number").source(e))?;
        Ok(Some(PdfaId { part, conformance }))
    }
    pub(crate) fn has_namespace(&self, ns: &str) -> bool {
        self.places.iter().any(|p| match p {
            Place::Attribute(a) => a.namespace() == Some(ns),
            Place::Element(n) => n.tag_name().namespace() == Some(ns),
        })
    }
    fn bag(&self, ns: &str, local: &str) -> Result<Option<Node<'a, 'i>>> {
        let mut found = None;
        for p in &self.places {
            self.l.step()?;
            if !p.named(ns, local) {
                continue;
            }
            if found.is_some() {
                return Err(malformed("PDF XMP ambiguous extension schema containers"));
            }
            let Place::Element(n) = p else {
                return Err(malformed("PDF XMP extension schemas must hold one rdf:Bag"));
            };
            no_resource_reference(*n)?;
            if n.attribute((RDF, "parseType")).is_some() {
                return Err(malformed("PDF XMP extension schemas must hold one rdf:Bag"));
            }
            let children = element_children(*n, self.l)?;
            if children.len() != 1 || !is(children[0], RDF, "Bag") {
                return Err(malformed("PDF XMP extension schemas must hold one rdf:Bag"));
            }
            let bag = children[0];
            no_resource_reference(bag)?;
            if bag.attribute((RDF, "parseType")).is_some() {
                return Err(malformed("PDF XMP extension schemas must hold one rdf:Bag"));
            }
            let _ = items(bag, self.l)?;
            found = Some(bag);
        }
        Ok(found)
    }
    pub(crate) fn declares_schema(&self, uri: &str) -> Result<bool> {
        let Some(bag) = self.bag(EXTENSION, "schemas")? else {
            return Ok(false);
        };
        for item in items(bag, self.l)? {
            let fields = resource_fields(item, self.l)?;
            let attr = fields.attribute((SCHEMA, "namespaceURI"));
            let mut nodes = fields.children().filter(|n| is(*n, SCHEMA, "namespaceURI"));
            let node = nodes.next();
            if nodes.next().is_some() || (attr.is_some() && node.is_some()) {
                return Err(malformed("PDF XMP schema namespaceURI is ambiguous"));
            }
            let namespace = match (attr, node) {
                (Some(s), None) => joined(&[s], self.l)?,
                (None, Some(n)) => value(n, self.l)?,
                _ => return Err(malformed("PDF XMP schema lacks namespaceURI")),
            };
            if namespace.trim().is_empty() {
                return Err(malformed("PDF XMP schema lacks namespaceURI"));
            }
            if namespace.trim() == uri {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
fn element_children<'a, 'i>(n: Node<'a, 'i>, l: &Context<'_>) -> Result<Vec<Node<'a, 'i>>> {
    let mut out = Vec::new();
    for c in n.children() {
        l.step()?;
        if c.is_element() {
            l.copied(std::mem::size_of_val(&c))?;
            out.push(c);
        } else if c.is_text() && c.text().is_some_and(|s| !s.trim().is_empty()) {
            return Err(malformed("PDF XMP mixed structured value"));
        }
    }
    Ok(out)
}
fn items<'a, 'i>(n: Node<'a, 'i>, l: &Context<'_>) -> Result<Vec<Node<'a, 'i>>> {
    let children = element_children(n, l)?;
    if children.iter().any(|c| !is(*c, RDF, "li")) {
        return Err(malformed("PDF XMP array contains a non-item element"));
    }
    if is(n, RDF, "Alt") {
        let mut languages = std::collections::BTreeSet::new();
        for c in &children {
            let lang = c
                .attribute((XML, "lang"))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| malformed("PDF XMP language alternative lacks xml:lang"))?;
            l.copied(lang.len())?;
            if !languages.insert(lang.to_ascii_lowercase()) {
                return Err(malformed("PDF XMP duplicate language alternative"));
            }
        }
    }
    Ok(children)
}
fn no_resource_reference(n: Node<'_, '_>) -> Result<()> {
    if ["resource", "nodeID", "ID", "datatype"]
        .iter()
        .any(|key| n.attribute((RDF, *key)).is_some())
    {
        return Err(malformed(
            "PDF XMP schema resource has conflicting RDF attributes",
        ));
    }
    Ok(())
}
fn resource_fields<'a, 'i>(n: Node<'a, 'i>, l: &Context<'_>) -> Result<Node<'a, 'i>> {
    no_resource_reference(n)?;
    let children = element_children(n, l)?;
    if n.attribute((RDF, "parseType")) == Some("Resource") {
        return Ok(n);
    }
    if n.attribute((RDF, "parseType")).is_some() {
        return Err(malformed("PDF XMP schema item must be a resource"));
    }
    if let [description] = children.as_slice()
        && is(*description, RDF, "Description")
    {
        no_resource_reference(*description)?;
        if description.attribute((RDF, "parseType")).is_some() {
            return Err(malformed("PDF XMP schema item must be a resource"));
        }
        if n.attribute((SCHEMA, "namespaceURI")).is_some() {
            return Err(malformed("PDF XMP schema namespaceURI is ambiguous"));
        }
        let _ = element_children(*description, l)?;
        return Ok(*description);
    }
    Err(malformed("PDF XMP schema item must be a resource"))
}
#[derive(Clone, Copy)]
enum Shape<'a, 'i> {
    Attribute(Attribute<'a, 'i>),
    Text(Node<'a, 'i>),
    Array(Node<'a, 'i>),
}
/// Follow only a unique qualified-value path; other fields of the resource are qualifiers.
fn shape<'a, 'i>(n: Node<'a, 'i>, l: &Context<'_>) -> Result<Shape<'a, 'i>> {
    l.step()?;
    if n.attribute((RDF, "resource")).is_some() {
        return Err(refused("XMP URI standard value"));
    }
    let mut children = Vec::new();
    for child in n.children().filter(Node::is_element) {
        l.step()?;
        l.copied(std::mem::size_of_val(&child))?;
        children.push(child);
    }
    let parse_type = n.attribute((RDF, "parseType"));
    let outer_value = n.attribute((RDF, "value"));
    let resource = if let Some(&description) = children.iter().find(|c| is(**c, RDF, "Description"))
    {
        if children.len() != 1 || parse_type.is_some() || outer_value.is_some() {
            return Err(malformed("PDF XMP qualified value is ambiguous"));
        }
        if ["about", "resource", "nodeID", "ID", "datatype", "parseType"]
            .iter()
            .any(|key| description.attribute((RDF, *key)).is_some())
        {
            return Err(malformed(
                "PDF XMP qualified Description has conflicting RDF attributes",
            ));
        }
        Some(description)
    } else {
        match parse_type {
            Some("Resource") => Some(n),
            Some(_) => return Err(refused("XMP parseType standard value")),
            None if outer_value.is_some() => Some(n),
            None => None,
        }
    };
    if let Some(fields) = resource {
        // Both the outer property and its Description are structured, never mixed text.
        let _ = element_children(n, l)?;
        let children = element_children(fields, l)?;
        let mut values = children.iter().filter(|v| is(**v, RDF, "value"));
        let attr = fields
            .attributes()
            .find(|a| a.namespace() == Some(RDF) && a.name() == "value");
        let node = values.next();
        if values.next().is_some() || (attr.is_some() && node.is_some()) {
            return Err(malformed("PDF XMP qualified value is ambiguous"));
        }
        return match (attr, node) {
            (Some(a), None) => Ok(Shape::Attribute(a)),
            (None, Some(v)) => shape(*v, l),
            _ => Err(malformed("PDF XMP qualified value lacks rdf:value")),
        };
    }
    if children.is_empty() {
        return Ok(Shape::Text(n));
    }
    let children = element_children(n, l)?;
    if children.len() == 1
        && ["Alt", "Seq", "Bag"]
            .iter()
            .any(|name| is(children[0], RDF, name))
    {
        let _ = items(children[0], l)?;
        return Ok(Shape::Array(children[0]));
    }
    Err(refused("XMP structured standard value"))
}
fn value(n: Node<'_, '_>, l: &Context<'_>) -> Result<String> {
    match shape(n, l)? {
        Shape::Attribute(a) => joined(&[a.value()], l),
        Shape::Text(n) => {
            let mut out = String::new();
            for c in n.children().filter(Node::is_text) {
                l.step()?;
                if let Some(s) = c.text() {
                    extend(&mut out, s, l)?;
                }
            }
            Ok(out)
        }
        Shape::Array(a) => {
            let members = items(a, l)?;
            if is(a, RDF, "Alt") {
                let selected = members
                    .iter()
                    .find(|n| {
                        n.attribute((XML, "lang"))
                            .is_some_and(|s| s.eq_ignore_ascii_case("x-default"))
                    })
                    .or(members.first());
                match selected {
                    Some(n) => value(*n, l),
                    None => Ok(String::new()),
                }
            } else {
                let mut out = String::new();
                for (index, member) in members.into_iter().enumerate() {
                    if index != 0 {
                        extend(&mut out, ", ", l)?;
                    }
                    extend(&mut out, &value(member, l)?, l)?;
                }
                Ok(out)
            }
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Form {
    Simple,
    LangAlt,
    Seq,
}
pub(crate) struct Property<'a> {
    pub ns: &'a str,
    pub prefix: &'a str,
    pub local: &'a str,
    pub form: Form,
    pub value: String,
}
pub(crate) struct Description<'a> {
    pub ns: &'a [(&'a str, &'a str)],
    pub body: String,
}
pub(crate) struct BagItem<'a> {
    pub ns: &'a str,
    pub local: &'a str,
    pub item: String,
    pub absent: Description<'a>,
}
/// Patch payloads are admitted before allocation; one final output allocation follows validation.
struct Patches<'a, 'l> {
    text: &'a str,
    edits: Vec<(Range<usize>, String)>,
    l: &'a Context<'l>,
}
impl<'a, 'l> Patches<'a, 'l> {
    fn push(&mut self, range: Range<usize>, value: String) -> Result<()> {
        self.l.step()?;
        if range.start > range.end || self.text.get(range.clone()).is_none() {
            return Err(malformed("PDF XMP invalid patch range"));
        }
        self.l.check(Limit::MetadataBytes, value.len())?;
        self.l.copied(value.len())?;
        self.l
            .copied(std::mem::size_of::<(Range<usize>, String)>())?;
        self.edits.push((range, value));
        Ok(())
    }
    fn append(&mut self, n: Node<'_, '_>, xml: String) -> Result<()> {
        let r = n.range();
        let raw = &self.text[r.clone()];
        if raw.ends_with("/>") {
            let name = qname(self.text, n, self.l)?;
            self.push(
                r.end - 2..r.end,
                joined(&[">", &xml, "</", &name, ">"], self.l)?,
            )
        } else {
            let close = raw
                .rfind("</")
                .ok_or_else(|| malformed("PDF XMP element has no end tag"))?
                + r.start;
            self.push(close..close, xml)
        }
    }
    fn attribute(&mut self, a: Attribute<'_, '_>, value: &str) -> Result<()> {
        let escaped = escape(value, self.l)?;
        self.push(
            a.range(),
            joined(
                &[&self.text[a.range_qname()], "=\"", &escaped, "\""],
                self.l,
            )?,
        )
    }
    fn scalar(&mut self, n: Node<'_, '_>, value: &str) -> Result<()> {
        match shape(n, self.l)? {
            Shape::Attribute(a) => self.attribute(a, value),
            Shape::Array(_) => Err(refused("XMP array in scalar value")),
            Shape::Text(n) => {
                let escaped = escape(value, self.l)?;
                let r = n.range();
                if self.text[r.clone()].ends_with("/>") {
                    return self.append(n, escaped);
                }
                // roxmltree coalesces adjacent text/CDATA but retains only the first range.
                // Edit lexical gaps around comments/PIs, not the coalesced text-node range.
                let start = start_tag_end(self.text, r.start)?;
                let end = self.text[r.clone()]
                    .rfind("</")
                    .ok_or_else(|| malformed("PDF XMP element has no end tag"))?
                    + r.start;
                let mut at = start;
                let mut replacement = Some(escaped);
                for child in n.children().filter(|c| c.is_comment() || c.is_pi()) {
                    let range = child.range();
                    self.push(at..range.start, replacement.take().unwrap_or_default())?;
                    at = range.end;
                }
                self.push(at..end, replacement.unwrap_or_default())
            }
        }
    }
    fn property(&mut self, n: Node<'_, '_>, p: &Property<'_>) -> Result<()> {
        let s = shape(n, self.l)?;
        match (p.form, s) {
            (Form::Simple, _) => self.scalar(n, &p.value),
            (Form::LangAlt, Shape::Array(a)) if is(a, RDF, "Alt") => {
                let members = items(a, self.l)?;
                if let Some(item) = members.iter().find(|n| {
                    n.attribute((XML, "lang"))
                        .is_some_and(|s| s.eq_ignore_ascii_case("x-default"))
                }) {
                    self.scalar(*item, &p.value)
                } else {
                    let xml = joined(
                        &[
                            "<rdf:li xmlns:rdf=\"",
                            RDF,
                            "\" xml:lang=\"x-default\">",
                            &escape(&p.value, self.l)?,
                            "</rdf:li>",
                        ],
                        self.l,
                    )?;
                    if let Some(first) = members.first() {
                        let at = first.range().start;
                        self.push(at..at, xml)
                    } else {
                        self.append(a, xml)
                    }
                }
            }
            (Form::Seq, Shape::Array(a)) if is(a, RDF, "Seq") => {
                let members = items(a, self.l)?;
                if let Some(first) = members.first() {
                    self.scalar(*first, &p.value)?;
                    for item in members.iter().skip(1) {
                        // Removing a list value is authorized, removing its unknown qualifiers is not.
                        if item.attributes().len() != 0 || item.children().any(|c| !c.is_text()) {
                            return Err(refused("XMP qualified author removal"));
                        }
                        self.push(item.range(), String::new())?;
                    }
                    Ok(())
                } else {
                    self.append(
                        a,
                        joined(
                            &[
                                "<rdf:li xmlns:rdf=\"",
                                RDF,
                                "\">",
                                &escape(&p.value, self.l)?,
                                "</rdf:li>",
                            ],
                            self.l,
                        )?,
                    )
                }
            }
            // A legacy simple title/author stays simple; no wrapper/qualifier is discarded.
            (Form::LangAlt | Form::Seq, Shape::Text(_) | Shape::Attribute(_)) => {
                self.scalar(n, &p.value)
            }
            _ => Err(refused("XMP incompatible standard property array")),
        }
    }
    fn finish(mut self) -> Result<String> {
        self.edits.sort_by_key(|(r, _)| (r.start, r.end));
        let mut at = 0;
        let mut size = self.text.len();
        for (r, v) in &self.edits {
            if r.start < at {
                return Err(malformed("PDF XMP overlapping properties"));
            }
            at = r.end;
            size = size
                .checked_sub(r.len())
                .and_then(|s| s.checked_add(v.len()))
                .ok_or_else(|| malformed("PDF XMP output length overflow"))?;
        }
        self.l.check(Limit::MetadataBytes, size)?;
        self.l.copied(size)?;
        let mut out = String::with_capacity(size);
        at = 0;
        for (r, v) in self.edits {
            out.push_str(&self.text[at..r.start]);
            out.push_str(&v);
            at = r.end;
        }
        out.push_str(&self.text[at..]);
        Ok(out)
    }
}
fn joined(parts: &[&str], l: &Context<'_>) -> Result<String> {
    let size = parts
        .iter()
        .try_fold(0usize, |size, p| size.checked_add(p.len()))
        .ok_or_else(|| malformed("PDF XMP output length overflow"))?;
    l.check(Limit::MetadataBytes, size)?;
    l.copied(size)?;
    let mut out = String::with_capacity(size);
    for p in parts {
        out.push_str(p);
    }
    Ok(out)
}
fn extend(out: &mut String, s: &str, l: &Context<'_>) -> Result<()> {
    let size = out
        .len()
        .checked_add(s.len())
        .ok_or_else(|| malformed("PDF XMP output length overflow"))?;
    l.check(Limit::MetadataBytes, size)?;
    l.copied(s.len())?;
    out.push_str(s);
    Ok(())
}
fn description(about: &str, d: &Description<'_>, l: &Context<'_>) -> Result<String> {
    let mut out = joined(
        &[
            "<rdf:Description rdf:about=\"",
            &escape(about, l)?,
            "\" xmlns:rdf=\"",
            RDF,
            "\"",
        ],
        l,
    )?;
    for (prefix, uri) in d.ns {
        extend(
            &mut out,
            &joined(&[" xmlns:", prefix, "=\"", uri, "\""], l)?,
            l,
        )?;
    }
    extend(
        &mut out,
        &joined(&[">", &d.body, "</rdf:Description>\n"], l)?,
        l,
    )?;
    Ok(out)
}
pub(crate) fn edit(
    text: &str,
    set: &[Property<'_>],
    add: &[Description<'_>],
    bag: Option<&BagItem<'_>>,
    l: &Context<'_>,
) -> Result<String> {
    let doc = parse(text, l)?;
    let packet = Packet::new(&doc, l)?;
    // Refuse conflicts on the selected subject even when the edit targets a different property.
    for field in MetadataField::ALL {
        let (ns, _, local) = field.xmp();
        drop(packet.read(ns, local)?);
    }
    drop(packet.pdfa()?);

    let mut patches = Patches {
        text,
        edits: Vec::new(),
        l,
    };
    let mut fresh = String::new();
    for p in set {
        let mut found = false;
        for place in &packet.places {
            l.step()?;
            if !place.named(p.ns, p.local) {
                continue;
            }
            found = true;
            match place {
                Place::Attribute(a) => patches.attribute(*a, &p.value)?,
                Place::Element(n) => patches.property(*n, p)?,
            }
        }
        if !found {
            let v = escape(&p.value, l)?;
            let inner = match p.form {
                Form::Simple => v,
                Form::LangAlt => joined(
                    &[
                        "<rdf:Alt><rdf:li xml:lang=\"x-default\">",
                        &v,
                        "</rdf:li></rdf:Alt>",
                    ],
                    l,
                )?,
                Form::Seq => joined(&["<rdf:Seq><rdf:li>", &v, "</rdf:li></rdf:Seq>"], l)?,
            };
            let body = joined(
                &[
                    "<", p.prefix, ":", p.local, ">", &inner, "</", p.prefix, ":", p.local, ">",
                ],
                l,
            )?;
            extend(
                &mut fresh,
                &description(
                    &packet.about,
                    &Description {
                        ns: &[(p.prefix, p.ns)],
                        body,
                    },
                    l,
                )?,
                l,
            )?;
            l.check(Limit::MetadataBytes, fresh.len())?;
        }
    }
    for d in add {
        extend(&mut fresh, &description(&packet.about, d, l)?, l)?;
        l.check(Limit::MetadataBytes, fresh.len())?;
    }
    if let Some(b) = bag {
        if let Some(holder) = packet.bag(b.ns, b.local)? {
            patches.append(holder, joined(&[&b.item], l)?)?;
        } else {
            extend(&mut fresh, &description(&packet.about, &b.absent, l)?, l)?;
        }
    }
    if !fresh.is_empty() {
        patches.append(packet.root, fresh)?;
    }
    let out = patches.finish()?;
    let check = parse(&out, l)?;
    let after = Packet::new(&check, l)?;
    for p in set {
        if after.read(p.ns, p.local)?.as_deref() != Some(p.value.as_str()) {
            return Err(malformed("PDF XMP value does not read back after edit"));
        }
    }
    Ok(out)
}
/// XML numeric whitespace references keep both attribute and element values exact.
fn escape(s: &str, l: &Context<'_>) -> Result<String> {
    let mut size = 0usize;
    for c in s.chars() {
        l.step()?;
        let n = match c {
            '&' => 5,
            '<' | '>' => 4,
            '"' => 6,
            '\r' | '\n' => 5,
            '\t' => 4,
            _ => c.len_utf8(),
        };
        size = size
            .checked_add(n)
            .ok_or_else(|| malformed("PDF XMP escaped length overflow"))?;
    }
    l.check(Limit::MetadataBytes, size)?;
    l.copied(size)?;
    let mut out = String::with_capacity(size);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#9;"),
            _ => out.push(c),
        }
    }
    Ok(out)
}
