// SPDX-License-Identifier: MIT OR Apache-2.0
//! Optional content (layers) in reading copies.
//!
//! A reading copy has a fresh catalog. A viewer shows a group that no catalog lists, so a layer the
//! source hides by default would become visible if its dictionary were copied alone. The
//! configuration is therefore rebuilt from the groups the copied pages reach: the group list, the
//! default state written out group by group, the panel order, automatic states, radio groups and
//! locks. Alternate configurations, the configuration name and creator and the list mode are not
//! carried. Whole-document operations write the source catalog again and need none of this.
//! A carried annotation counts as part of its page: the group its `OC` entry or its appearance
//! names is listed like a group of the page content.
use crate::Limit;
use crate::error::{Result, malformed, refused};
use crate::graph;
use crate::limits::Context;
use lopdf::{Dictionary, Document, Object, ObjectId, dictionary};
use std::collections::BTreeSet;

/// True for an optional content group dictionary.
pub(crate) fn is_group(o: &Object) -> bool {
    o.as_dict()
        .is_ok_and(|d| matches!(d.get(b"Type").and_then(Object::as_name), Ok(b"OCG")))
}

/// The default configuration of one input, read and checked before its pages are copied.
#[derive(Default)]
pub(crate) struct Source<'d> {
    /// Listed groups in catalog order, each once.
    groups: Vec<ObjectId>,
    /// The same groups as a set; the importer refuses a group outside it.
    pub listed: BTreeSet<ObjectId>,
    /// Groups the default configuration hides.
    hidden: BTreeSet<ObjectId>,
    config: Option<&'d Dictionary>,
}

fn dictionary_of<'a>(
    doc: &'a Document,
    o: &'a Object,
    l: &Context<'_>,
    what: &str,
) -> Result<&'a Dictionary> {
    graph::resolve(doc, o, l)?.as_dict().map_err(|e| {
        malformed(format!("PDF optional content {what} must be a dictionary")).source(e)
    })
}
fn array_of<'a>(
    doc: &'a Document,
    o: &'a Object,
    l: &Context<'_>,
    what: &str,
) -> Result<&'a [Object]> {
    graph::resolve(doc, o, l)?
        .as_array()
        .map(Vec::as_slice)
        .map_err(|e| malformed(format!("PDF optional content {what} must be an array")).source(e))
}

impl<'d> Source<'d> {
    /// Read the catalog configuration. A document without one has no listed group.
    pub(crate) fn read(doc: &'d Document, l: &Context<'_>) -> Result<Self> {
        let Ok(properties) = graph::catalog(doc, l)?.get(b"OCProperties") else {
            return Ok(Self::default()); // No optional content configuration in this document.
        };
        let properties = dictionary_of(doc, properties, l, "properties")?;
        let list = array_of(
            doc,
            properties
                .get(b"OCGs")
                .map_err(|e| malformed("PDF optional content lacks its group list").source(e))?,
            l,
            "group list",
        )?;
        let mut groups = Vec::new();
        let mut listed = BTreeSet::new();
        for entry in list {
            l.step()?;
            let id = entry.as_reference().map_err(|e| {
                malformed("PDF optional content groups must be indirect objects").source(e)
            })?;
            if !doc.objects.get(&id).is_some_and(is_group) {
                return Err(malformed(
                    "PDF optional content group list names an object that is not a group",
                ));
            }
            if listed.insert(id) {
                groups.push(id);
            }
        }
        let config = dictionary_of(
            doc,
            properties.get(b"D").map_err(|e| {
                malformed("PDF optional content lacks a default configuration").source(e)
            })?,
            l,
            "default configuration",
        )?;
        // A copy is written with the default intent, View. A source that considers other intents
        // in its default configuration would be evaluated differently, so it is named instead.
        if let Ok(intent) = config.get(b"Intent") {
            let view = match graph::resolve(doc, intent, l)? {
                Object::Name(n) => n == b"View",
                Object::Array(a) => {
                    a.len() == 1 && matches!(graph::resolve(doc, &a[0], l)?.as_name(), Ok(b"View"))
                }
                _ => {
                    return Err(malformed(
                        "PDF optional content Intent must be a name or an array of names",
                    ));
                }
            };
            if !view {
                return Err(refused("optional content default intent other than View"));
            }
        }
        let base_on = match config.get(b"BaseState") {
            Err(_) => true, // PDF: an absent BaseState means ON.
            Ok(v) => match graph::resolve(doc, v, l)?.as_name() {
                Ok(b"ON") => true,
                Ok(b"OFF") => false,
                Ok(b"Unchanged") => {
                    return Err(refused("optional content base state Unchanged"));
                }
                _ => {
                    return Err(malformed(
                        "PDF optional content BaseState must be ON, OFF or Unchanged",
                    ));
                }
            },
        };
        let mut source = Self {
            groups,
            listed,
            hidden: BTreeSet::new(),
            config: Some(config),
        };
        let on: BTreeSet<_> = source.members(doc, config, b"ON", l)?.into_iter().collect();
        let off: BTreeSet<_> = source
            .members(doc, config, b"OFF", l)?
            .into_iter()
            .collect();
        if on.intersection(&off).next().is_some() {
            return Err(malformed(
                "PDF optional content group is both ON and OFF in the default configuration",
            ));
        }
        source.hidden = if base_on {
            off
        } else {
            source.listed.difference(&on).copied().collect()
        };
        Ok(source)
    }

    /// The listed groups an optional array entry names, in its order.
    fn members(
        &self,
        doc: &Document,
        d: &Dictionary,
        key: &[u8],
        l: &Context<'_>,
    ) -> Result<Vec<ObjectId>> {
        let Ok(value) = d.get(key) else {
            return Ok(Vec::new()); // The entry is optional and names no group when absent.
        };
        let name = String::from_utf8_lossy(key);
        array_of(doc, value, l, &name)?
            .iter()
            .map(|entry| {
                l.step()?;
                self.group(entry, &name)
            })
            .collect()
    }
    fn group(&self, entry: &Object, what: &str) -> Result<ObjectId> {
        match entry.as_reference() {
            Ok(id) if self.listed.contains(&id) => Ok(id),
            _ => Err(malformed(format!(
                "PDF optional content {what} names an object that is not a listed group"
            ))),
        }
    }

    /// Add what the copied pages of this input reached to the configuration of the copy.
    /// `copied` gives the output identifier of a source object that was written.
    pub(crate) fn carry(
        &self,
        doc: &Document,
        copied: &dyn Fn(ObjectId) -> Option<ObjectId>,
        out: &mut Layers,
        l: &Context<'_>,
    ) -> Result<()> {
        let Some(config) = self.config else {
            return Ok(()); // Nothing listed: the importer refused any group it met.
        };
        let before = out.groups.len();
        for id in &self.groups {
            l.step()?;
            if let Some(new) = copied(*id) {
                out.groups.push(Object::Reference(new));
                if self.hidden.contains(id) {
                    out.hidden.push(Object::Reference(new));
                }
            }
        }
        if out.groups.len() == before {
            return Ok(()); // The copied pages use no group of this input.
        }
        let carried = |ids: Vec<ObjectId>| -> Vec<Object> {
            ids.into_iter()
                .filter_map(copied)
                .map(Object::Reference)
                .collect()
        };
        if let Ok(order) = config.get(b"Order") {
            out.order.extend(self.order(doc, order, copied, l, 0)?);
        }
        if let Ok(auto) = config.get(b"AS") {
            for entry in array_of(doc, auto, l, "AS")? {
                l.step()?;
                let usage = dictionary_of(doc, entry, l, "AS entry")?;
                let groups = carried(self.members(doc, usage, b"OCGs", l)?);
                if groups.is_empty() {
                    continue;
                }
                let event = usage.get(b"Event").map_err(|e| {
                    malformed("PDF optional content AS entry lacks Event").source(e)
                })?;
                let event = match graph::resolve(doc, event, l)?.as_name() {
                    Ok(n) if [b"View".as_slice(), b"Print", b"Export"].contains(&n) => n,
                    _ => {
                        return Err(malformed(
                            "PDF optional content AS Event must be View, Print or Export",
                        ));
                    }
                };
                let category = usage.get(b"Category").map_err(|e| {
                    malformed("PDF optional content AS entry lacks Category").source(e)
                })?;
                let mut names = Vec::new();
                for name in array_of(doc, category, l, "AS Category")? {
                    l.step()?;
                    let name = graph::resolve(doc, name, l)?.as_name().map_err(|e| {
                        malformed("PDF optional content AS Category must hold names").source(e)
                    })?;
                    names.push(Object::Name(name.to_vec()));
                }
                out.auto.push(Object::Dictionary(dictionary! {
                    "Event" => Object::Name(event.to_vec()),
                    "Category" => names,
                    "OCGs" => groups,
                }));
            }
        }
        if let Ok(radio) = config.get(b"RBGroups") {
            for entry in array_of(doc, radio, l, "RBGroups")? {
                l.step()?;
                let ids = array_of(doc, entry, l, "RBGroups entry")?
                    .iter()
                    .map(|g| self.group(g, "RBGroups"))
                    .collect::<Result<Vec<_>>>()?;
                let groups = carried(ids);
                if !groups.is_empty() {
                    out.radio.push(Object::Array(groups));
                }
            }
        }
        out.locked
            .extend(carried(self.members(doc, config, b"Locked", l)?));
        Ok(())
    }

    /// The panel order restricted to copied groups. A label is kept with the list it heads; a list
    /// that keeps no group is dropped; the children of a group that is not copied move up a level.
    fn order(
        &self,
        doc: &Document,
        o: &Object,
        copied: &dyn Fn(ObjectId) -> Option<ObjectId>,
        l: &Context<'_>,
        depth: usize,
    ) -> Result<Vec<Object>> {
        l.check(Limit::Depth, depth + 1)?;
        let mut out = Vec::new();
        let mut parent_left_out = false;
        for (index, item) in array_of(doc, o, l, "Order")?.iter().enumerate() {
            l.step()?;
            match graph::resolve(doc, item, l)? {
                Object::Array(_) => {
                    let inner = self.order(doc, item, copied, l, depth + 1)?;
                    let labelled = matches!(inner.first(), Some(Object::String(..)));
                    if inner.iter().any(|v| !matches!(v, Object::String(..))) {
                        if parent_left_out && !labelled {
                            out.extend(inner);
                        } else {
                            out.push(Object::Array(inner));
                        }
                    }
                    parent_left_out = false;
                }
                label @ Object::String(..) if depth > 0 && index == 0 => {
                    out.push(label.clone());
                    parent_left_out = false;
                }
                Object::Dictionary(_) => match copied(self.group(item, "Order")?) {
                    Some(new) => {
                        out.push(Object::Reference(new));
                        parent_left_out = false;
                    }
                    None => parent_left_out = true,
                },
                _ => {
                    return Err(malformed(
                        "PDF optional content Order entry must be a group, a list or a list label",
                    ));
                }
            }
        }
        Ok(out)
    }
}

/// The configuration of a reading copy, gathered over its inputs.
#[derive(Default)]
pub(crate) struct Layers {
    groups: Vec<Object>,
    hidden: Vec<Object>,
    order: Vec<Object>,
    auto: Vec<Object>,
    radio: Vec<Object>,
    locked: Vec<Object>,
}
impl Layers {
    /// The catalog entry of the copy, or nothing when its pages use no group.
    pub(crate) fn properties(self) -> Option<Dictionary> {
        if self.groups.is_empty() {
            return None;
        }
        // Every state is written out: the base state is ON and each hidden group is in OFF, so
        // inputs with different base states read the same way in one copy.
        let mut config = dictionary! {"BaseState" => "ON"};
        for (key, value) in [
            ("OFF", self.hidden),
            ("Order", self.order),
            ("AS", self.auto),
            ("RBGroups", self.radio),
            ("Locked", self.locked),
        ] {
            if !value.is_empty() {
                config.set(key, value);
            }
        }
        Some(dictionary! {"OCGs" => self.groups, "D" => config})
    }
}
