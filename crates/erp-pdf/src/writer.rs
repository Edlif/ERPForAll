// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::error::{Result, invalid, malformed, owner, refused, unsupported};
use crate::limits::{Budget, Context};
use crate::{AnnotationPolicy, Limit, PdfError, PdfErrorKind, annot, graph};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::{BTreeMap, BTreeSet};
/// A caller-owned output destination. No resource is opened by this crate.
pub type Emitter<'a> = dyn FnMut(&[u8]) -> std::io::Result<()> + 'a;
pub(crate) struct Writer<'a, 'b> {
    emit: &'a mut Emitter<'b>,
    offset: usize,
    xref: BTreeMap<u32, (u16, usize)>,
    pub next: u32,
    budget: Budget<'a>,
}
impl<'a, 'b> Writer<'a, 'b> {
    pub fn new(l: &'a Context<'a>, emit: &'a mut Emitter<'b>) -> Result<Self> {
        let mut w = Self {
            emit,
            offset: 0,
            xref: BTreeMap::new(),
            next: 1,
            budget: Budget::new(l),
        };
        w.bytes(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n")?;
        Ok(w)
    }
    /// Continue after an unchanged original file: an incremental update keeps it as the prefix.
    pub fn append(l: &'a Context<'a>, emit: &'a mut Emitter<'b>, original: &[u8]) -> Result<Self> {
        let mut w = Self {
            emit,
            offset: 0,
            xref: BTreeMap::new(),
            next: 1,
            budget: Budget::new(l),
        };
        w.bytes(original)?;
        if !original.ends_with(b"\n") {
            w.bytes(b"\n")?;
        }
        Ok(w)
    }
    fn bytes(&mut self, b: &[u8]) -> Result<()> {
        let size = self
            .offset
            .checked_add(b.len())
            .ok_or_else(|| malformed("PDF output offset overflow"))?;
        self.budget.limits.check(Limit::OutputBytes, size)?;
        (self.emit)(b).map_err(|e| {
            PdfError::new(PdfErrorKind::Output, "PDF output consumer failed").source(e)
        })?;
        self.offset = size;
        Ok(())
    }
    pub fn id(&mut self) -> Result<ObjectId> {
        let id = self.next;
        self.next = id
            .checked_add(1)
            .ok_or_else(|| malformed("PDF object identifier overflow"))?;
        self.budget.limits.check(Limit::Objects, id as usize)?;
        Ok((id, 0))
    }
    pub fn object(&mut self, id: ObjectId, o: &Object) -> Result<()> {
        self.budget
            .limits
            .check(Limit::Objects, self.xref.len() + 1)?;
        if self.xref.insert(id.0, (id.1, self.offset)).is_some() {
            return Err(malformed("PDF duplicate output object"));
        }
        self.bytes(format!("{} {} obj\n", id.0, id.1).as_bytes())?;
        self.value(o, 0)?;
        self.bytes(b"\nendobj\n")
    }
    fn value(&mut self, o: &Object, depth: usize) -> Result<()> {
        self.budget.step()?;
        self.budget.limits.check(Limit::Depth, depth + 1)?;
        match o {
            Object::Null => self.bytes(b"null"),
            Object::Boolean(v) => self.bytes(if *v { b"true" } else { b"false" }),
            Object::Integer(v) => self.bytes(v.to_string().as_bytes()),
            Object::Real(v) => {
                if !v.is_finite() {
                    return Err(malformed("PDF non-finite number"));
                }
                self.bytes(v.to_string().as_bytes())
            }
            Object::Name(n) => self.name(n),
            Object::String(s, _) => {
                self.bytes(b"<")?;
                for byte in s {
                    self.bytes(format!("{byte:02X}").as_bytes())?;
                }
                self.bytes(b">")
            }
            Object::Reference(id) => self.bytes(format!("{} {} R", id.0, id.1).as_bytes()),
            Object::Array(a) => {
                self.bytes(b"[")?;
                for v in a {
                    self.value(v, depth + 1)?;
                    self.bytes(b" ")?;
                }
                self.bytes(b"]")
            }
            Object::Dictionary(d) => self.dictionary(d, depth, None),
            Object::Stream(s) => {
                self.dictionary(&s.dict, depth, Some(s.content.len()))?;
                self.bytes(b"\nstream\n")?;
                self.bytes(&s.content)?;
                self.bytes(b"\nendstream")
            }
        }
    }
    fn name(&mut self, n: &[u8]) -> Result<()> {
        self.bytes(b"/")?;
        for byte in n {
            if (33..=126).contains(byte) && !b"()<>[]{}/%#".contains(byte) {
                self.bytes(std::slice::from_ref(byte))?;
            } else {
                self.bytes(format!("#{byte:02X}").as_bytes())?;
            }
        }
        Ok(())
    }
    fn dictionary(&mut self, d: &Dictionary, depth: usize, length: Option<usize>) -> Result<()> {
        self.bytes(b"<<")?;
        // Sorted keys make output independent of source dictionary insertion order.
        let keys: std::collections::BTreeSet<_> = d.iter().map(|(k, _)| k).collect();
        for k in keys {
            if length.is_some() && k == b"Length" {
                continue;
            }
            self.bytes(b" ")?;
            self.name(k)?;
            self.bytes(b" ")?;
            self.value(
                d.get(k)
                    .map_err(|e| malformed("PDF dictionary lookup failed").source(e))?,
                depth + 1,
            )?;
        }
        if let Some(length) = length {
            self.bytes(format!(" /Length {length}").as_bytes())?;
        }
        self.bytes(b" >>")
    }
    pub fn finish(self, trailer: Dictionary) -> Result<()> {
        self.finish_section(trailer, None)
    }
    /// End an incremental-update section chained to the original cross-reference at `prev`.
    pub fn finish_update(self, trailer: Dictionary, prev: usize, size: i64) -> Result<()> {
        self.finish_section(trailer, Some((prev, size)))
    }
    fn finish_section(
        mut self,
        mut trailer: Dictionary,
        update: Option<(usize, i64)>,
    ) -> Result<()> {
        let start = self.offset;
        self.bytes(b"xref\n0 1\n0000000000 65535 f \n")?;
        let entries = std::mem::take(&mut self.xref);
        let max = entries
            .last_key_value()
            .map(|(id, _)| *id)
            .ok_or_else(|| malformed("PDF output has no objects"))?;
        for (id, (generation, offset)) in entries {
            self.bytes(format!("{id} 1\n{offset:010} {generation:05} n \n").as_bytes())?;
        }
        for k in [
            b"Prev".as_slice(),
            b"XRefStm",
            b"Type",
            b"W",
            b"Index",
            b"Length",
            b"Filter",
            b"DecodeParms",
        ] {
            trailer.remove(k);
        }
        let mut size = i64::from(max) + 1;
        if let Some((prev, original)) = update {
            trailer.set("Prev", prev as i64);
            size = size.max(original);
        }
        trailer.set("Size", size);
        self.bytes(b"trailer\n")?;
        self.value(&Object::Dictionary(trailer), 0)?;
        self.bytes(format!("\nstartxref\n{start}\n%%EOF\n").as_bytes())
    }
}
pub(crate) fn save(doc: Document, l: &Context<'_>, emit: &mut Emitter<'_>) -> Result<()> {
    let mut w = Writer::new(l, emit)?;
    for (id, o) in &doc.objects {
        w.object(*id, o)?;
    }
    w.finish(doc.trailer)
}
fn count(n: usize) -> Result<usize> {
    n.checked_add(1)
        .ok_or_else(|| invalid("PDF annotation counter overflow"))
}
/// Import reachable page resources, emitting each source object's body once.
pub(crate) struct Importer<'d, 'b, 'a, 'c> {
    pub doc: &'d Document,
    pub writer: &'b mut Writer<'a, 'c>,
    map: BTreeMap<ObjectId, ObjectId>,
    /// Source pages of this copy; a destination may name one that is written later.
    pages: BTreeSet<ObjectId>,
    budget: Budget<'a>,
    plain: bool,
    policy: AnnotationPolicy,
    /// One-based position of the document among the inputs, for messages.
    ordinal: usize,
    /// Annotations carried into the copy.
    pub kept: usize,
    /// Annotations left out under [`AnnotationPolicy::KeepOrDrop`].
    pub dropped: usize,
}
impl<'d, 'b, 'a, 'c> Importer<'d, 'b, 'a, 'c> {
    pub fn new(
        doc: &'d Document,
        writer: &'b mut Writer<'a, 'c>,
        limits: &'a Context<'a>,
        plain: bool,
        policy: AnnotationPolicy,
        ordinal: usize,
    ) -> Self {
        Self {
            doc,
            writer,
            map: BTreeMap::new(),
            pages: BTreeSet::new(),
            budget: Budget::new(limits),
            plain,
            policy,
            ordinal,
            kept: 0,
            dropped: 0,
        }
    }
    /// Give every page of this copy its output identifier before any is written, so a link can
    /// name a page that comes later.
    pub fn reserve(&mut self, pages: impl IntoIterator<Item = ObjectId>) -> Result<()> {
        for old in pages {
            let id = self.writer.id()?;
            self.map.insert(old, id);
            self.pages.insert(old);
        }
        Ok(())
    }
    /// Write one reserved page; `number` is its one-based position in the source document.
    pub fn page(
        &mut self,
        number: usize,
        old: ObjectId,
        mut page: Dictionary,
        parent: ObjectId,
    ) -> Result<ObjectId> {
        let id = *self
            .map
            .get(&old)
            .ok_or_else(|| malformed("PDF page was not reserved for this copy"))?;
        page.remove(b"Parent");
        let annotations = page.remove(b"Annots");
        let mut copied = self.copy_dict(&page, 0)?;
        if let Some(annotations) = annotations {
            let carried = self.annotations(&annotations, id, number)?;
            if !carried.is_empty() {
                copied.set("Annots", carried);
            }
        }
        copied.set("Parent", parent);
        self.writer.object(id, &Object::Dictionary(copied))?;
        Ok(id)
    }
    /// Copy the annotations of one page that stand on their own; refuse or count the others.
    fn annotations(
        &mut self,
        value: &Object,
        page: ObjectId,
        number: usize,
    ) -> Result<Vec<Object>> {
        let doc = self.doc;
        let l = self.budget.limits;
        let items = graph::resolve(doc, value, l)?
            .as_array()
            .map_err(|e| malformed("PDF Annots must be an array").source(e))?;
        if items.is_empty() {
            return Ok(Vec::new());
        }
        if self.policy == AnnotationPolicy::Refuse {
            return Err(graph::annotations_refused());
        }
        l.check(Limit::Objects, items.len())?;
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            l.step()?;
            let pages = &self.pages;
            entries.push(annot::entry(doc, item, l, &|id| pages.contains(&id))?);
        }
        annot::settle(&mut entries, l)?;
        // Identifiers first: a note and its popup name each other.
        for e in &entries {
            match (e.why, e.id) {
                (Some(_), _) if self.policy == AnnotationPolicy::KeepOrDrop => {
                    self.dropped = count(self.dropped)?;
                }
                (Some(why), _) => {
                    let what = format!(
                        "annotation with {} (input {}, page {number})",
                        why.text, self.ordinal
                    );
                    return Err(match why.owner {
                        Some(owner) => unsupported(what, owner),
                        None => refused(what),
                    });
                }
                (None, Some(old)) => {
                    if self.map.contains_key(&old) {
                        return Err(malformed("PDF annotation is listed more than once"));
                    }
                    let id = self.writer.id()?;
                    self.map.insert(old, id);
                }
                (None, None) => {} // Written inside the page's own array.
            }
        }
        let carried = annot::carried(&entries);
        let mut out = Vec::new();
        for e in entries.iter().filter(|e| e.why.is_none()) {
            let mut d = e.dict.clone();
            // The owning page is set below; a popup that is not carried is not named.
            d.remove(b"P");
            if !matches!(d.get(b"Popup"), Ok(Object::Reference(id)) if carried.contains(id)) {
                d.remove(b"Popup");
            }
            let mut copy = self.copy_dict(&d, 0)?;
            copy.set("P", page);
            self.kept = count(self.kept)?;
            match e.id {
                Some(old) => {
                    let id = *self
                        .map
                        .get(&old)
                        .ok_or_else(|| malformed("PDF annotation identifier vanished"))?;
                    self.writer.object(id, &Object::Dictionary(copy))?;
                    out.push(Object::Reference(id));
                }
                None => out.push(Object::Dictionary(copy)),
            }
        }
        Ok(out)
    }
    fn copy_dict(&mut self, d: &Dictionary, depth: usize) -> Result<Dictionary> {
        let mut out = Dictionary::new();
        for (k, v) in d.iter() {
            // A merged reading copy intentionally contains no archival metadata or associations.
            if self.plain
                && [
                    b"Metadata".as_slice(),
                    b"AF",
                    b"StructParents",
                    b"StructParent",
                ]
                .contains(&k.as_slice())
            {
                continue;
            }
            out.set(k.clone(), self.copy(v, depth + 1)?);
        }
        Ok(out)
    }
    pub fn copy(&mut self, o: &Object, depth: usize) -> Result<Object> {
        self.budget.step()?;
        self.budget.limits.check(Limit::Depth, depth + 1)?;
        Ok(match o {
            Object::Reference(old) => {
                if let Some(new) = self.map.get(old) {
                    return Ok(Object::Reference(*new));
                }
                let source = self
                    .doc
                    .objects
                    .get(old)
                    .ok_or_else(|| malformed("PDF missing imported object"))?;
                if let Ok(d) = source.as_dict()
                    && let Ok(kind) = d.get(b"Type")
                    && matches!(kind.as_name(), Ok(b"Page" | b"Pages" | b"Catalog"))
                {
                    return Err(unsupported("cross-document page reference", owner::DEST));
                }
                let id = self.writer.id()?;
                self.map.insert(*old, id);
                let copied = self.copy(source, depth + 1)?;
                self.writer.object(id, &copied)?;
                Object::Reference(id)
            }
            Object::Array(a) => Object::Array(
                a.iter()
                    .map(|v| self.copy(v, depth + 1))
                    .collect::<Result<_>>()?,
            ),
            Object::Dictionary(d) => Object::Dictionary(self.copy_dict(d, depth)?),
            Object::Stream(s) => {
                let mut copy = s.clone();
                copy.dict = self.copy_dict(&s.dict, depth)?;
                Object::Stream(copy)
            }
            Object::Null
            | Object::Boolean(_)
            | Object::Integer(_)
            | Object::Real(_)
            | Object::Name(_)
            | Object::String(_, _) => o.clone(),
        })
    }
}
