// SPDX-License-Identifier: MIT OR Apache-2.0
//! Page annotations in reading copies: which ones stand on their own.
//!
//! A reading copy has a fresh catalog with a page tree and nothing else. An annotation is carried
//! only when everything it means lives in the annotation and in pages of the same copy; one that
//! needs a form, a name tree or a script is named, never carried with a dangling dependency.
//! Optional-content membership is carried: the copy lists the group in its rebuilt configuration.
use crate::error::{Result, malformed, owner};
use crate::graph;
use crate::limits::Context;
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::BTreeSet;

/// Annotation types whose meaning needs nothing outside the annotation and its page.
const CARRIED: [&[u8]; 16] = [
    b"Link",
    b"Text",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Stamp",
    b"Caret",
    b"Ink",
    b"Popup",
];

/// Why an annotation is not self-contained, and the task that will carry it when one will.
#[derive(Clone, Copy)]
pub(crate) struct Why {
    pub text: &'static str,
    pub owner: Option<&'static str>,
}
/// Not carried by design: no task plans to.
const fn never(text: &'static str) -> Option<Why> {
    Some(Why { text, owner: None })
}
/// Not carried until `owner` lands.
const fn until(owner: &'static str, text: &'static str) -> Option<Why> {
    Some(Why {
        text,
        owner: Some(owner),
    })
}
const ORPHAN: Option<Why> = never("a parent annotation that is not carried");

/// One entry of a page's `Annots` array while its fate is decided.
pub(crate) struct Entry<'d> {
    /// Object identifier when the array holds a reference; popups and replies name it.
    pub id: Option<ObjectId>,
    pub dict: &'d Dictionary,
    popup: bool,
    /// Why it cannot be carried; `None` when it stands on its own.
    pub why: Option<Why>,
}

/// Read one `Annots` entry and decide whether it stands on its own.
/// `copied` answers whether an object is a page of this reading copy.
pub(crate) fn entry<'d>(
    doc: &'d Document,
    item: &'d Object,
    l: &Context<'_>,
    copied: &dyn Fn(ObjectId) -> bool,
) -> Result<Entry<'d>> {
    let dict = graph::annotation(doc, item, l)?;
    let subtype = graph::annotation_type(doc, dict, l)?;
    Ok(Entry {
        id: item.as_reference().ok(),
        dict,
        popup: subtype == b"Popup",
        why: dependency(doc, dict, subtype, l, copied)?,
    })
}

fn name<'d>(
    doc: &'d Document,
    d: &'d Dictionary,
    key: &[u8],
    l: &Context<'_>,
    message: &'static str,
) -> Result<&'d [u8]> {
    let value = d.get(key).map_err(|e| malformed(message).source(e))?;
    graph::resolve(doc, value, l)?
        .as_name()
        .map_err(|e| malformed(message).source(e))
}

fn dependency(
    doc: &Document,
    d: &Dictionary,
    subtype: &[u8],
    l: &Context<'_>,
    copied: &dyn Fn(ObjectId) -> bool,
) -> Result<Option<Why>> {
    if subtype == b"Widget" {
        return Ok(until(owner::FORMS, "a form field"));
    }
    if !CARRIED.contains(&subtype) {
        return Ok(never("a type outside the reading-copy profile"));
    }
    // `PA` is an earlier action kept for reference; neither is the action a reading copy checks.
    if d.has(b"AA") || d.has(b"PA") {
        return Ok(never("additional actions"));
    }
    // Without its own appearance a FreeText is drawn from the form's default resources.
    if subtype == b"FreeText" && !d.has(b"AP") {
        return Ok(never("no appearance stream"));
    }
    if let Ok(dest) = d.get(b"Dest")
        && let Some(why) = destination(doc, dest, l, copied)?
    {
        return Ok(Some(why));
    }
    if let Ok(action) = d.get(b"A") {
        let action = graph::resolve(doc, action, l)?
            .as_dict()
            .map_err(|e| malformed("PDF annotation action must be a dictionary").source(e))?;
        if action.has(b"Next") {
            return Ok(never("a chained action"));
        }
        // A structure destination names an element of the structure tree, which is not carried.
        if action.has(b"SD") {
            return Ok(never("a structure destination"));
        }
        match name(doc, action, b"S", l, "PDF annotation action lacks a type")? {
            b"URI" => {}
            b"GoTo" => {
                let dest = action
                    .get(b"D")
                    .map_err(|e| malformed("PDF GoTo action lacks a destination").source(e))?;
                if let Some(why) = destination(doc, dest, l, copied)? {
                    return Ok(Some(why));
                }
            }
            _ => return Ok(never("an action other than URI or GoTo")),
        }
    }
    Ok(None)
}

fn destination(
    doc: &Document,
    o: &Object,
    l: &Context<'_>,
    copied: &dyn Fn(ObjectId) -> bool,
) -> Result<Option<Why>> {
    match graph::resolve(doc, o, l)? {
        Object::Array(a) => match a.first() {
            Some(Object::Reference(page)) if copied(*page) => Ok(None),
            // Another page, a page number of another file, or anything else that is not ours.
            Some(_) => Ok(never("a destination outside the copied pages")),
            None => Err(malformed("PDF destination array is empty")),
        },
        // Named destinations live in the catalog, which a reading copy does not carry.
        Object::Name(_) | Object::String(_, _) => Ok(until(owner::DEST, "a named destination")),
        _ => Err(malformed(
            "PDF destination must be an array, a name or a string",
        )),
    }
}

/// Identifiers of the entries that are carried.
pub(crate) fn carried(entries: &[Entry<'_>]) -> BTreeSet<ObjectId> {
    entries
        .iter()
        .filter(|e| e.why.is_none())
        .filter_map(|e| e.id)
        .collect()
}

/// A reply or a popup follows its parent: when the parent is not carried neither is it.
/// Repeats until nothing changes, so a reply to a reply to a dropped note goes too.
pub(crate) fn settle(entries: &mut [Entry<'_>], l: &Context<'_>) -> Result<()> {
    loop {
        let kept = carried(entries);
        let mut changed = false;
        for e in entries.iter_mut() {
            l.step()?;
            if e.why.is_some() {
                continue;
            }
            let orphan = [
                Some(b"IRT".as_slice()),
                e.popup.then_some(b"Parent".as_slice()),
            ]
            .into_iter()
            .flatten()
            .filter_map(|key| e.dict.get(key).ok())
            .any(|parent| !matches!(parent, Object::Reference(id) if kept.contains(id)));
            if orphan {
                e.why = ORPHAN;
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
    }
}
