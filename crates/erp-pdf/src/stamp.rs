// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::error::{Result, guard, invalid, malformed, owner, unsupported};
use crate::{Emitter, PageSelection, PdfLimits, StampPlacement};
use crate::{graph, pages, writer};
use lopdf::{Dictionary, Object, Stream, dictionary};
/// Append a one-page PDF overlay as an isolated Form XObject.
/// Annotations of the target stay as they are and are drawn above the overlay by a reader; an
/// overlay page that has annotations is refused, a Form XObject cannot hold them.
/// Placement uses target default page coordinates; source crop coordinates are retained.
/// Non-zero source rotation is refused; rotate its content in the generating renderer instead.
/// Original graphics state must balance. Discard all output on any error.
/// Optional content of the target stays as it is. Marked-content sections of the target page must
/// be closed, so the overlay never lands inside a hidden layer. An overlay with optional content
/// is refused: the target catalog does not list its groups.
pub fn stamp(
    input: &[u8],
    overlay: &[u8],
    selection: &PageSelection,
    placement: &StampPlacement,
    l: &PdfLimits,
    emit: &mut Emitter<'_>,
) -> Result<()> {
    guard(|| {
        let context = crate::limits::Context::new(l);
        let l = &context;
        let mut doc = graph::load(input, l, 1)?;
        pages::mutable(&doc, l)?;
        let original_pages = graph::pages(&doc, l, graph::PageProfile::WholeDocument)?;
        let chosen = pages::selected(&original_pages, selection, l)?;
        let source = graph::load(overlay, l, 2)?;
        if graph::catalog(&source, l)?.has(b"OCProperties") {
            return Err(unsupported("overlay optional content", owner::OVERLAY));
        }
        let source_pages = graph::pages(&source, l, graph::PageProfile::Overlay)?;
        if source_pages.len() != 1 {
            return Err(invalid("PDF stamp requires exactly one overlay page"));
        }
        let page = &source_pages[0].1;
        if let Ok(v) = page.get(b"Rotate")
            && graph::resolve(&source, v, l)?
                .as_i64()
                .map_err(|e| malformed("PDF overlay Rotate must be integer").source(e))?
                .rem_euclid(360)
                != 0
        {
            return Err(unsupported("rotated overlay", owner::OVERLAY));
        }
        let content = graph::page_content(&source, page, l)?;
        balanced(&content, l)?;
        l.copied(content.len())?;
        let bbox = match page.get(b"CropBox") {
            Ok(v) => v,
            Err(_) => page
                .get(b"MediaBox")
                .map_err(|e| malformed("PDF overlay lacks MediaBox").source(e))?,
        }; // PDF CropBox defaults to MediaBox.
        let bbox = graph::import(&source, bbox, &mut doc, l)?;
        let resources = match page.get(b"Resources") {
            Ok(v) => graph::import(&source, v, &mut doc, l)?,
            Err(_) => Object::Dictionary(Dictionary::new()),
        }; // A page without resource references can omit Resources.
        let mut form_dict =
            dictionary! {"Type"=>"XObject","Subtype"=>"Form","BBox"=>bbox,"Resources"=>resources};
        if let Ok(group) = page.get(b"Group") {
            form_dict.set("Group", graph::import(&source, group, &mut doc, l)?);
        }
        if page.has(b"UserUnit") {
            return Err(unsupported("overlay UserUnit", owner::OVERLAY));
        }
        let form = graph::add(&mut doc, Object::Stream(Stream::new(form_dict, content)), l)?;
        for (id, page) in chosen {
            let original = graph::page_content(&doc, page, l)?;
            balanced(&original, l)?;
            let mut resources = match page.get(b"Resources") {
                Ok(v) => {
                    let d = graph::dict(&doc, v, l)?;
                    graph::charge_dictionary(d, l, 0)?;
                    d.clone()
                }
                Err(_) => Dictionary::new(),
            };
            let mut xobjects = match resources.get(b"XObject") {
                Ok(v) => {
                    let d = graph::dict(&doc, v, l)?;
                    graph::charge_dictionary(d, l, 0)?;
                    d.clone()
                }
                Err(_) => Dictionary::new(),
            };
            let mut n = 0usize;
            let name = loop {
                let name = format!("ERPStamp{n}");
                if !xobjects.has(name.as_bytes()) {
                    break name;
                }
                n = n
                    .checked_add(1)
                    .ok_or_else(|| malformed("PDF stamp name overflow"))?;
                l.check(crate::Limit::WorkUnits, n)?;
            };
            xobjects.set(name.as_bytes(), form);
            resources.set("XObject", xobjects);
            let [a, b, c, d, e, f] = placement.0;
            let suffix = format!("\nQ\nq\n{a} {b} {c} {d} {e} {f} cm\n/{name} Do\nQ\n");
            l.copied(original.len())?;
            l.copied(suffix.len())?;
            l.copied(b"q\n".len())?;
            let mut bytes = b"q\n".to_vec();
            bytes.extend_from_slice(&original);
            bytes.extend_from_slice(suffix.as_bytes());
            l.check(crate::Limit::DecodedBytes, bytes.len())?;
            let content = graph::add(
                &mut doc,
                Object::Stream(Stream::new(Dictionary::new(), bytes)),
                l,
            )?;
            let d = doc
                .get_object_mut(*id)
                .and_then(Object::as_dict_mut)
                .map_err(|e| malformed("PDF stamp page mutation failed").source(e))?;
            d.set("Resources", resources);
            d.set("Contents", content);
        }
        writer::save(doc, l, emit)
    })
}
fn balanced(bytes: &[u8], l: &crate::limits::Context<'_>) -> Result<()> {
    let content = lopdf::content::Content::decode(bytes)
        .map_err(|e| malformed("PDF malformed drawing operations").source(e))?;
    let mut depth = 0usize;
    let mut marked = 0usize;
    let mut text = false;
    for operation in content.operations {
        l.step()?;
        match operation.operator.as_str() {
            "q" => {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| malformed("PDF graphics-state depth overflow"))?;
            }
            "BT" if !text => text = true,
            "ET" if text => text = false,
            "BT" | "ET" => return Err(malformed("PDF unbalanced text state")),
            "Q" => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| malformed("PDF unbalanced graphics state"))?;
            }
            // An open section may belong to a hidden layer and would swallow what follows.
            "BMC" | "BDC" => {
                marked = marked
                    .checked_add(1)
                    .ok_or_else(|| malformed("PDF marked-content depth overflow"))?;
            }
            "EMC" => {
                marked = marked
                    .checked_sub(1)
                    .ok_or_else(|| malformed("PDF unbalanced marked content"))?;
            }
            _ => {} // Other drawing operators do not alter these enclosing state scopes.
        }
        l.check(crate::Limit::Depth, depth.max(marked).max(1))?;
    }
    if marked != 0 {
        return Err(malformed("PDF unbalanced marked content"));
    }
    if text {
        return Err(malformed("PDF unbalanced text state"));
    }
    if depth != 0 {
        return Err(malformed("PDF unbalanced graphics state"));
    }
    Ok(())
}
