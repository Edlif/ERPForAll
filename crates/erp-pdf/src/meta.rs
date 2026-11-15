// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::edit::{Update, caller_text, text, text_object};
use crate::error::{Result, guard, invalid, malformed};
use crate::limits::Context;
use crate::xmp::{self, Description, escape};
use crate::{Emitter, PdfDate, PdfLimits, graph};
use lopdf::{Dictionary, Object, StringFormat};
use std::collections::BTreeMap;

const DC: &str = "http://purl.org/dc/elements/1.1/";
const XMP: &str = "http://ns.adobe.com/xap/1.0/";
const PDF: &str = "http://ns.adobe.com/pdf/1.3/";

/// Standard document fields kept equal in the Info dictionary and the XMP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum MetadataField {
    /// Info `Title`, XMP `dc:title`.
    Title,
    /// Info `Author`, XMP `dc:creator` (one entry).
    Author,
    /// Info `Subject`, XMP `dc:description`.
    Subject,
    /// Info `Keywords`, XMP `pdf:Keywords`.
    Keywords,
    /// Info `Creator`, XMP `xmp:CreatorTool`.
    Creator,
    /// Info `Producer`, XMP `pdf:Producer`.
    Producer,
    /// Info `CreationDate`, XMP `xmp:CreateDate`.
    CreationDate,
    /// Info `ModDate`, XMP `xmp:ModifyDate`.
    ModDate,
}
const ALL: [MetadataField; 8] = [
    MetadataField::Title,
    MetadataField::Author,
    MetadataField::Subject,
    MetadataField::Keywords,
    MetadataField::Creator,
    MetadataField::Producer,
    MetadataField::CreationDate,
    MetadataField::ModDate,
];
impl MetadataField {
    fn info(self) -> &'static str {
        [
            "Title",
            "Author",
            "Subject",
            "Keywords",
            "Creator",
            "Producer",
            "CreationDate",
            "ModDate",
        ][self as usize]
    }
    fn xmp(self) -> (&'static str, &'static str, &'static str) {
        [
            (DC, "dc", "title"),
            (DC, "dc", "creator"),
            (DC, "dc", "description"),
            (PDF, "pdf", "Keywords"),
            (XMP, "xmp", "CreatorTool"),
            (PDF, "pdf", "Producer"),
            (XMP, "xmp", "CreateDate"),
            (XMP, "xmp", "ModifyDate"),
        ][self as usize]
    }
    fn is_date(self) -> bool {
        matches!(self, Self::CreationDate | Self::ModDate)
    }
}
#[derive(Debug, Clone)]
enum Value {
    Text(String),
    Date(PdfDate),
}
/// Validated set of standard fields to write; fields not set keep their current values.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MetadataUpdate(BTreeMap<MetadataField, Value>);
impl MetadataUpdate {
    /// Empty update; at least one field must be set before use.
    pub fn new() -> Self {
        Self::default()
    }
    /// Set a text field (not a date field).
    pub fn text(mut self, field: MetadataField, value: &str, limits: &PdfLimits) -> Result<Self> {
        if field.is_date() {
            return Err(invalid(format!(
                "PDF metadata {} takes a date",
                field.info()
            )));
        }
        caller_text(&format!("metadata {}", field.info()), value, limits)?;
        self.0.insert(field, Value::Text(value.into()));
        Ok(self)
    }
    /// Set `CreationDate` or `ModDate`.
    pub fn date(mut self, field: MetadataField, value: PdfDate) -> Result<Self> {
        if !field.is_date() {
            return Err(invalid(format!("PDF metadata {} takes text", field.info())));
        }
        self.0.insert(field, Value::Date(value));
        Ok(self)
    }
}
/// PDF/A identification read from `pdfaid:part` and `pdfaid:conformance`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PdfaId {
    /// PDF/A part (1, 2, 3, 4).
    pub part: u8,
    /// Conformance level letter as written (`A`, `B`, `U`), if any.
    pub conformance: Option<String>,
}
/// Standard fields as found in each representation; dates use the XMP (ISO 8601) form.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Metadata {
    /// Fields of the Info dictionary.
    pub info: BTreeMap<MetadataField, String>,
    /// Fields of the XMP packet.
    pub xmp: BTreeMap<MetadataField, String>,
    /// PDF/A identification, when the packet declares one.
    pub pdfa: Option<PdfaId>,
    /// The XMP packet verbatim.
    pub packet: Option<String>,
}
/// Inspect Info and XMP metadata.
pub fn metadata(input: &[u8], limits: &PdfLimits) -> Result<Metadata> {
    guard(|| {
        let context = Context::new(limits);
        let l = &context;
        let u = Update::load(input, l)?;
        let mut info = BTreeMap::new();
        if let Ok(v) = u.doc.trailer.get(b"Info") {
            let d = graph::dict(&u.doc, v, l)?;
            for f in ALL {
                if let Ok(v) = d.get(f.info().as_bytes()) {
                    let s = graph::resolve(&u.doc, v, l)?.as_str().map_err(|e| {
                        malformed(format!("PDF Info {} must be a string", f.info())).source(e)
                    })?;
                    let s = text(s)?;
                    info.insert(
                        f,
                        if f.is_date() {
                            crate::date::normalize(s)
                        } else {
                            s
                        },
                    );
                }
            }
        }
        let packet = u.xmp(l)?;
        let mut fields = BTreeMap::new();
        let mut pdfa = None;
        if let Some(p) = &packet {
            let doc = xmp::parse(p, l)?;
            for f in ALL {
                let (ns, _, local) = f.xmp();
                if let Some(v) = xmp::read(&doc, ns, local)? {
                    fields.insert(f, v);
                }
            }
            pdfa = xmp::pdfa(&doc)?;
        }
        Ok(Metadata {
            info,
            xmp: fields,
            pdfa,
            packet,
        })
    })
}
/// Write the given standard fields to Info and XMP as an incremental update: the original bytes
/// are emitted unchanged as the prefix and unrelated XMP properties are kept verbatim.
/// Unsafe XMP (DTD, entities, malformed, nested or structured standard properties) is refused.
/// Discard all emitted bytes on error.
pub fn update_metadata(
    input: &[u8],
    update: &MetadataUpdate,
    limits: &PdfLimits,
    emit: &mut Emitter<'_>,
) -> Result<()> {
    guard(|| {
        if update.0.is_empty() {
            return Err(invalid("PDF metadata update sets no field"));
        }
        let context = Context::new(limits);
        let l = &context;
        let mut u = Update::load(input, l)?;
        apply(&mut u, update, &[], l)?;
        u.save(input, l, emit)
    })
}
/// Apply `update` plus any extra XMP descriptions in one packet revision.
pub(crate) fn apply(
    u: &mut Update,
    update: &MetadataUpdate,
    extra: &[Description<'_>],
    l: &Context<'_>,
) -> Result<()> {
    if !update.0.is_empty() {
        let (id, mut info) = match u.doc.trailer.get(b"Info") {
            Ok(Object::Reference(id)) => (
                Some(*id),
                graph::dict(&u.doc, &Object::Reference(*id), l)?.clone(),
            ),
            Ok(v) => (None, graph::dict(&u.doc, v, l)?.clone()),
            Err(_) => (None, Dictionary::new()), // No Info dictionary yet.
        };
        for (f, v) in &update.0 {
            info.set(
                f.info(),
                match v {
                    Value::Text(s) => text_object(s),
                    Value::Date(d) => {
                        Object::String(d.to_pdf_string().into_bytes(), StringFormat::Literal)
                    }
                },
            );
        }
        let id = match id {
            Some(id) => {
                u.set(id, Object::Dictionary(info));
                id
            }
            None => u.add(Object::Dictionary(info), l)?,
        };
        u.doc.trailer.set("Info", id);
    }
    let old = u.xmp(l)?;
    let old = old.as_deref().unwrap_or(xmp::EMPTY);
    let remove: Vec<_> = update
        .0
        .keys()
        .map(|f| {
            let (ns, _, local) = f.xmp();
            (ns, local)
        })
        .collect();
    let mut body = String::new();
    for (f, v) in &update.0 {
        let (_, prefix, local) = f.xmp();
        let value = match v {
            Value::Text(s) => escape(s),
            Value::Date(d) => d.to_xmp_string(),
        };
        let inner = match f {
            MetadataField::Title | MetadataField::Subject => {
                format!("<rdf:Alt><rdf:li xml:lang=\"x-default\">{value}</rdf:li></rdf:Alt>")
            }
            MetadataField::Author => format!("<rdf:Seq><rdf:li>{value}</rdf:li></rdf:Seq>"),
            _ => value,
        };
        body.push_str(&format!("<{prefix}:{local}>{inner}</{prefix}:{local}>"));
    }
    let mut add = Vec::new();
    let ns = [("dc", DC), ("xmp", XMP), ("pdf", PDF)];
    if !body.is_empty() {
        add.push(Description { ns: &ns, body });
    }
    add.extend(extra.iter().map(|d| Description {
        ns: d.ns,
        body: d.body.clone(),
    }));
    let packet = xmp::edit(old, &remove, &add, l)?;
    u.set_xmp(packet, l)
}
