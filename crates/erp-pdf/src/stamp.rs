// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::error::{Result, guard, invalid, malformed, owner, unsupported};
use crate::limits::Context;
use crate::{Emitter, PageSelection, PdfLimits, StampPlacement};
use crate::{graph, pages, writer};
use lopdf::{Dictionary, Object, Stream, dictionary};
/// Append a one-page PDF overlay as an isolated Form XObject.
/// Annotations of the target stay as they are and are drawn above the overlay by a reader; an
/// overlay page that has annotations is refused, a Form XObject cannot hold them.
/// An unsigned form of the target stays a form; a signed target or overlay is refused.
///
/// Placement uses target default page coordinates. The overlay is placed the way a viewer shows
/// its page:
///
/// - Without `Rotate` the coordinates of its crop box are retained.
/// - With a quarter-turn `Rotate` the overlay is turned clockwise about its crop box: the turned
///   box keeps the lower-left corner of the crop box and swaps width and height for 90 and 270.
/// - `UserUnit` keeps the physical size: overlay coordinates are multiplied by the overlay's unit
///   and divided by the unit of each target page, so the lower-left corner of the crop box scales
///   with them.
///
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
        let context = Context::new(l);
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
        let turn = match page.get(b"Rotate") {
            Ok(v) => graph::resolve(&source, v, l)?
                .as_i64()
                .map_err(|e| malformed("PDF overlay Rotate must be integer").source(e))?
                .rem_euclid(360),
            Err(_) => 0, // PDF absent Rotate means zero.
        };
        let content = graph::page_content(&source, page, l)?;
        balanced(&content, l)?;
        l.copied(content.len())?;
        let bbox = match page.get(b"CropBox") {
            Ok(v) => v,
            Err(_) => page
                .get(b"MediaBox")
                .map_err(|e| malformed("PDF overlay lacks MediaBox").source(e))?,
        }; // PDF CropBox defaults to MediaBox.
        let matrix = form_matrix(
            turn,
            corners(&source, bbox, l)?,
            user_unit(&source, page, l)?,
        )?;
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
        if let Some(matrix) = matrix {
            form_dict.set("Matrix", matrix.map(Object::Real).to_vec());
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
            let suffix = format!(
                "\nQ\nq\n{} cm\n/{name} Do\nQ\n",
                placed(placement, user_unit(&doc, page, l)?)?
            );
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
fn number(doc: &lopdf::Document, o: &Object, l: &Context<'_>) -> Result<Option<f64>> {
    Ok(match graph::resolve(doc, o, l)? {
        Object::Integer(v) => Some(*v as f64),
        Object::Real(v) => Some(f64::from(*v)),
        _ => None, // The caller names what had to be a number.
    })
}
/// Size of one user space unit in 1/72 inch.
fn user_unit(doc: &lopdf::Document, page: &Dictionary, l: &Context<'_>) -> Result<f64> {
    let Ok(v) = page.get(b"UserUnit") else {
        return Ok(1.0); // PDF absent UserUnit means 1/72 inch.
    };
    match number(doc, v, l)? {
        Some(unit) if unit.is_finite() && unit > 0.0 => Ok(unit),
        _ => Err(malformed("PDF UserUnit must be a positive number")),
    }
}
/// Lower-left and upper-right corner of a page box, whichever two corners the file gives.
fn corners(doc: &lopdf::Document, bbox: &Object, l: &Context<'_>) -> Result<[f64; 4]> {
    let a = graph::resolve(doc, bbox, l)?
        .as_array()
        .map_err(|e| malformed("PDF overlay box must be an array").source(e))?;
    let mut v = [0.0; 4];
    if a.len() != v.len() {
        return Err(malformed("PDF overlay box must have four coordinates"));
    }
    for (slot, o) in v.iter_mut().zip(a) {
        *slot = number(doc, o, l)?
            .ok_or_else(|| malformed("PDF overlay box coordinates must be numbers"))?;
    }
    Ok([
        v[0].min(v[2]),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    ])
}
/// Form matrix from overlay page coordinates to the overlay as a viewer shows it.
///
/// `turn` is the clockwise page rotation in degrees. A quarter turn maps the crop box
/// `[x0 y0 x1 y1]` onto the box with the same lower-left corner and, for 90 and 270, swapped
/// width and height. `unit` then scales every coordinate to 1/72 inch. `None` when nothing changes.
fn form_matrix(turn: i64, [x0, y0, x1, y1]: [f64; 4], unit: f64) -> Result<Option<[f32; 6]>> {
    let turned = match turn {
        0 => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        90 => [0.0, -1.0, 1.0, 0.0, x0 - y0, y0 + x1],
        180 => [-1.0, 0.0, 0.0, -1.0, x0 + x1, y0 + y1],
        270 => [0.0, 1.0, -1.0, 0.0, x0 + y1, y0 - x0],
        _ => return Err(malformed("PDF Rotate must be a multiple of 90")),
    };
    if turn == 0 && unit == 1.0 {
        return Ok(None);
    }
    let matrix = turned.map(|v| (v * unit) as f32);
    if matrix.iter().any(|v| !v.is_finite()) || matrix[..4].iter().all(|v| *v == 0.0) {
        return Err(malformed("PDF overlay geometry is out of range"));
    }
    Ok(Some(matrix))
}
/// Placement operands for a target page whose user space unit is `unit` times 1/72 inch.
fn placed(placement: &StampPlacement, unit: f64) -> Result<String> {
    let [a, b, c, d, e, f] = placement.0;
    if unit == 1.0 {
        return Ok(format!("{a} {b} {c} {d} {e} {f}"));
    }
    // The form is in 1/72 inch; the translation is already in target units.
    // Content operands use the same finite f32 representation as the PDF object model.
    // Validate after narrowing: otherwise a finite f64 can become infinity or a singular
    // matrix when a reader parses the emitted drawing operation.
    let [a, b, c, d] = [a, b, c, d].map(|v| (f64::from(v) / unit) as f32);
    let determinant = f64::from(a) * f64::from(d) - f64::from(b) * f64::from(c);
    if [a, b, c, d].iter().any(|v| !v.is_finite()) || determinant == 0.0 {
        return Err(malformed(
            "PDF stamp placement is out of range for the page UserUnit",
        ));
    }
    Ok(format!("{a} {b} {c} {d} {e} {f}"))
}
pub(crate) fn balanced(bytes: &[u8], l: &Context<'_>) -> Result<()> {
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
