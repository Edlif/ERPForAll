// SPDX-License-Identifier: MIT OR Apache-2.0
//! XMP packets are edited as text: removed properties are cut by their parsed byte ranges and
//! new properties are added in a fresh `rdf:Description`, so unrelated properties stay verbatim.
use crate::error::{Result, malformed, owner, refused, unsupported};
use crate::limits::Context;
use crate::{Limit, PdfaId};
use roxmltree::{Document, Node};

pub(crate) const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub(crate) const PDFAID: &str = "http://www.aiim.org/pdfa/ns/id/";
pub(crate) const EXTENSION: &str = "http://www.aiim.org/pdfa/ns/extension/";
/// Skeleton for documents without an XMP packet.
pub(crate) const EMPTY: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n</rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";

pub(crate) fn parse<'a>(text: &'a str, l: &Context<'_>) -> Result<Document<'a>> {
    l.check(Limit::MetadataBytes, text.len())?;
    let doc = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: u32::try_from(l.get(Limit::WorkUnits)).unwrap_or(u32::MAX),
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
fn rdf<'a, 'i>(doc: &'a Document<'i>) -> Result<Node<'a, 'i>> {
    let mut found = doc
        .descendants()
        .filter(|n| n.tag_name().namespace() == Some(RDF) && n.tag_name().name() == "RDF");
    let rdf = found
        .next()
        .ok_or_else(|| malformed("PDF XMP lacks rdf:RDF"))?;
    if found.next().is_some() {
        return Err(unsupported("XMP with several rdf:RDF elements", owner::XMP));
    }
    Ok(rdf)
}
fn is(n: &Node<'_, '_>, ns: &str, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(ns) && n.tag_name().name() == local
}
fn descriptions<'a, 'i>(rdf: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    rdf.children().filter(|n| is(n, RDF, "Description"))
}
/// Value of a simple, `rdf:Alt` (x-default first) or `rdf:Seq`/`rdf:Bag` (joined) property.
pub(crate) fn read(doc: &Document<'_>, ns: &str, local: &str) -> Result<Option<String>> {
    for d in descriptions(rdf(doc)?) {
        if let Some(a) = d
            .attributes()
            .find(|a| a.namespace() == Some(ns) && a.name() == local)
        {
            return Ok(Some(a.value().to_string()));
        }
        let Some(p) = d.children().find(|n| is(n, ns, local)) else {
            continue;
        };
        let container = p.children().find(|n| n.is_element());
        let Some(c) = container else {
            return Ok(Some(p.text().unwrap_or_default().to_string()));
        };
        let items: Vec<_> = c.children().filter(|n| is(n, RDF, "li")).collect();
        let text = |n: &Node<'_, '_>| n.text().unwrap_or_default().to_string();
        return Ok(Some(if is(&c, RDF, "Alt") {
            items
                .iter()
                .find(|n| {
                    n.attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
                        == Some("x-default")
                })
                .or(items.first())
                .map(text)
                .unwrap_or_default()
        } else if is(&c, RDF, "Seq") || is(&c, RDF, "Bag") {
            items.iter().map(text).collect::<Vec<_>>().join(", ")
        } else {
            return Err(unsupported("XMP structured standard property", owner::XMP));
        }));
    }
    Ok(None)
}
pub(crate) fn pdfa(doc: &Document<'_>) -> Result<Option<PdfaId>> {
    let Some(part) = read(doc, PDFAID, "part")? else {
        return Ok(None);
    };
    let part = part
        .trim()
        .parse()
        .map_err(|e| malformed("PDF XMP pdfaid:part must be a number").source(e))?;
    Ok(Some(PdfaId {
        part,
        conformance: read(doc, PDFAID, "conformance")?,
    }))
}
/// Any element or attribute in namespace `ns` anywhere in the packet.
pub(crate) fn uses(doc: &Document<'_>, ns: &str) -> bool {
    doc.descendants().any(|n| {
        n.tag_name().namespace() == Some(ns) || n.attributes().any(|a| a.namespace() == Some(ns))
    })
}
/// A new `rdf:Description` with its namespace declarations and property XML.
pub(crate) struct Description<'a> {
    pub ns: &'a [(&'a str, &'a str)],
    pub body: String,
}
/// Remove top-level properties `remove` and append `add`; unknown content is kept byte for byte.
pub(crate) fn edit(
    text: &str,
    remove: &[(&str, &str)],
    add: &[Description<'_>],
    l: &Context<'_>,
) -> Result<String> {
    let doc = parse(text, l)?;
    let rdf = rdf(&doc)?;
    let matches = |ns: Option<&str>, local: &str| {
        remove
            .iter()
            .any(|(n, name)| ns == Some(*n) && local == *name)
    };
    let mut cuts = Vec::new();
    for n in rdf.descendants().skip(1) {
        l.step()?;
        let top = n
            .parent()
            .is_some_and(|p| is(&p, RDF, "Description") && p.parent() == Some(rdf));
        for a in n.attributes() {
            if matches(a.namespace(), a.name()) {
                if !is(&n, RDF, "Description") || n.parent() != Some(rdf) {
                    return Err(unsupported("XMP nested standard property", owner::XMP));
                }
                cuts.push(a.range());
            }
        }
        if n.is_element() && matches(n.tag_name().namespace(), n.tag_name().name()) {
            if !top {
                return Err(unsupported("XMP nested standard property", owner::XMP));
            }
            cuts.push(n.range());
        }
    }
    let close = text[rdf.range()]
        .rfind("</")
        .filter(|_| !text[rdf.range()].ends_with("/>"))
        .ok_or_else(|| unsupported("XMP empty-element rdf:RDF", owner::XMP))?
        + rdf.range().start;
    let about = descriptions(rdf)
        .find_map(|d| d.attribute((RDF, "about")))
        .unwrap_or("");
    let mut insert = String::new();
    for d in add {
        insert.push_str("<rdf:Description rdf:about=\"");
        insert.push_str(&escape(about));
        insert.push_str("\" xmlns:rdf=\"");
        insert.push_str(RDF);
        insert.push('"');
        for (prefix, uri) in d.ns {
            insert.push_str(&format!(" xmlns:{prefix}=\"{uri}\""));
        }
        insert.push('>');
        insert.push_str(&d.body);
        insert.push_str("</rdf:Description>\n");
    }
    cuts.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(text.len() + insert.len());
    let mut at = 0;
    for r in cuts {
        if r.start < at {
            return Err(malformed("PDF XMP overlapping properties"));
        }
        if r.start > close {
            return Err(malformed("PDF XMP property after rdf:RDF end"));
        }
        out.push_str(&text[at..r.start]);
        at = r.end;
    }
    out.push_str(&text[at..close]);
    out.push_str(&insert);
    out.push_str(&text[close..]);
    parse(&out, l)?; // The spliced packet must still be well-formed.
    Ok(out)
}
pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
