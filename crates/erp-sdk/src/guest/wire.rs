// SPDX-License-Identifier: MIT OR Apache-2.0
//! The argument and result types of the core methods a component overrides besides `create`,
//! `write` and `unlink`: `copy`, `copy_data`, `default_get`, `filtered_domain` and `upsert`
//! (WASM.WIRE-NATIVE.CORE-CODECS; `exists` needs none, it takes nothing and answers
//! [`Records`](super::Records)).
//!
//! Each type has the name and the methods of its native counterpart, so one override compiles in
//! both backends, and crosses the boundary in the wire form the host's codec of the method reads
//! and writes (`crates/erp-orm/src/wire.rs`): a values object for [`ValueMap`], list syntax for
//! [`Domain`], the call object and the list of result rows for [`UpsertCall`] and
//! [`UpsertResult`]. The host checks every value against its field when the call reaches the
//! native implementation; nothing here grants a right or converts a value for a field.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use erp_fields::{Cell, Error, RecId, Result, Value, serde_json};

use super::{RawVals, rt};

/// A dynamic value as JSON. The kinds without a form in this ABI are refused, at any depth, as
/// the host refuses them: never sent as text.
fn value_json(value: &Value) -> Result<serde_json::Value> {
    match value {
        Value::BigInt(_) | Value::Uuid(_) | Value::Xid8(_) | Value::TimestampTz(_) => {
            Err(Error::programming(
                "native scalar values are not supported across frozen WASM ABI1; requires a future ABI major with typed native fields",
            ))
        }
        Value::List(items) => Ok(serde_json::Value::Array(
            items.iter().map(value_json).collect::<Result<_>>()?,
        )),
        Value::Map(map) => Ok(serde_json::Value::Object(
            map.iter()
                .map(|(name, value)| Ok((name.clone(), value_json(value)?)))
                .collect::<Result<_>>()?,
        )),
        other => erp_fields::convert::value_to_json(other),
    }
}

/// A JSON value from the host as a dynamic value, as the host reads a client's: `null` is
/// `False`, a whole number an `Int`, any other number a `Float`, a list a `List`, an object a
/// `Map`.
fn json_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::False,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::Int(i),
            (None, Some(f)) => Value::Float(f),
            (None, None) => Value::Str(n.to_string().into()),
        },
        serde_json::Value::String(s) => Value::Str(s.as_str().into()),
        serde_json::Value::Array(items) => Value::List(items.iter().map(json_value).collect()),
        serde_json::Value::Object(object) => Value::Map(Arc::new(
            object
                .iter()
                .map(|(name, value)| (name.clone(), json_value(value)))
                .collect(),
        )),
    }
}

/// Field name to dynamic value, in insertion order: the `default` of `copy` and `copy_data` and
/// the result of `default_get`. It is the map of the native surface (it dereferences to it:
/// `insert`, `get`, `iter` and the rest), with the wire form a component needs. A value is
/// untyped here, as a client's: the host checks it against its field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValueMap(erp_fields::ValueMap);

impl ValueMap {
    /// No values.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Deref for ValueMap {
    type Target = erp_fields::ValueMap;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ValueMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<erp_fields::ValueMap> for ValueMap {
    fn from(map: erp_fields::ValueMap) -> Self {
        Self(map)
    }
}

impl From<ValueMap> for erp_fields::ValueMap {
    fn from(map: ValueMap) -> Self {
        map.0
    }
}

impl FromIterator<(String, Value)> for ValueMap {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl IntoIterator for ValueMap {
    type Item = (String, Value);
    type IntoIter = <erp_fields::ValueMap as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl serde::Serialize for ValueMap {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{Error as _, SerializeMap};
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in &self.0 {
            let json = value_json(value).map_err(S::Error::custom)?;
            map.serialize_entry(name, &json)?;
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for ValueMap {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let object = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        Ok(object
            .iter()
            .map(|(name, value)| (name.clone(), json_value(value)))
            .collect())
    }
}

/// A search domain as a component holds it: its list syntax, as the JSON document that crosses
/// the boundary (`[["state", "=", "done"]]`, with the prefix operators `&`, `|`, `!`). The host
/// parses it as external input when the call reaches the native implementation, so a malformed
/// domain or an unknown field is refused there by name. The builder API of the native `Domain` is
/// not available in a component.
#[derive(Debug, Clone, PartialEq)]
pub struct Domain {
    list: serde_json::Value,
}

impl Domain {
    /// The domain of a list-syntax document: a list, or `true` / `false` for the constants.
    ///
    /// # Errors
    /// `Validation` for any other document.
    pub fn from_json(json: &serde_json::Value) -> Result<Self> {
        match json {
            serde_json::Value::Array(_) | serde_json::Value::Bool(_) => {
                Ok(Self { list: json.clone() })
            }
            _ => Err(Error::validation(
                "a domain is a list in list syntax, or true or false",
            )),
        }
    }

    /// The list-syntax document.
    ///
    /// # Errors
    /// None in a component; the native method of this name can fail, and one source serves both.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        Ok(self.list.clone())
    }
}

impl serde::Serialize for Domain {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.list.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Domain {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let json = serde_json::Value::deserialize(deserializer)?;
        Self::from_json(&json).map_err(|e| serde::de::Error::custom(e.message()))
    }
}

/// An upsert as overrides of `upsert` receive it and pass it to `super`: the same builder and
/// accessors as the native `UpsertCall`.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertCall {
    key: Cow<'static, str>,
    rows: Vec<RawVals>,
    update: Vec<Cow<'static, str>>,
    accumulate: Vec<(Cow<'static, str>, Option<Cell>)>,
}

impl UpsertCall {
    /// An upsert on the business key declared as `key`, with no rows.
    pub fn new(key: impl Into<Cow<'static, str>>) -> Self {
        Self {
            key: key.into(),
            rows: Vec::new(),
            update: Vec::new(),
            accumulate: Vec::new(),
        }
    }

    /// Append rows. Each row sets every key field.
    #[must_use]
    pub fn with_rows(mut self, rows: impl IntoIterator<Item = RawVals>) -> Self {
        self.rows.extend(rows);
        self
    }

    /// Overwrite these fields of existing rows from the input when they differ.
    #[must_use]
    pub fn with_update<S: Into<Cow<'static, str>>>(
        mut self,
        fields: impl IntoIterator<Item = S>,
    ) -> Self {
        self.update.extend(fields.into_iter().map(Into::into));
        self
    }

    /// Add the row's value to the existing value of this field.
    #[must_use]
    pub fn with_accumulate(mut self, field: impl Into<Cow<'static, str>>) -> Self {
        self.accumulate.push((field.into(), None));
        self
    }

    /// Like [`UpsertCall::with_accumulate`], never below `floor`.
    #[must_use]
    pub fn with_accumulate_floor(
        mut self,
        field: impl Into<Cow<'static, str>>,
        floor: Cell,
    ) -> Self {
        self.accumulate.push((field.into(), Some(floor)));
        self
    }

    /// The key's declared name.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The rows, in input order.
    pub fn rows(&self) -> &[RawVals] {
        &self.rows
    }

    /// The rows, for overrides that adjust them before calling `super`. The caller reads the
    /// result by input row, so the number of rows must stay the same.
    pub fn rows_mut(&mut self) -> &mut Vec<RawVals> {
        &mut self.rows
    }

    /// The fields overwritten on conflict.
    pub fn update(&self) -> impl Iterator<Item = &str> {
        self.update.iter().map(AsRef::as_ref)
    }

    /// The accumulated fields with their floors.
    pub fn accumulate(&self) -> impl Iterator<Item = (&str, Option<&Cell>)> {
        self.accumulate
            .iter()
            .map(|(f, c)| (f.as_ref(), c.as_ref()))
    }
}

impl serde::Serialize for UpsertCall {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{Error as _, SerializeMap};
        let accumulate = self
            .accumulate
            .iter()
            .map(|(field, floor)| {
                let floor = match floor {
                    Some(cell) => rt::cell_json(cell).map_err(S::Error::custom)?,
                    None => serde_json::Value::Null,
                };
                Ok((field.as_ref(), floor))
            })
            .collect::<std::result::Result<Vec<(&str, serde_json::Value)>, S::Error>>()?;
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("key", self.key.as_ref())?;
        map.serialize_entry("rows", &self.rows)?;
        map.serialize_entry("update", &self.update)?;
        map.serialize_entry("accumulate", &accumulate)?;
        map.end()
    }
}

/// The call object as the host sends it.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UpsertCallWire {
    key: String,
    #[serde(default)]
    rows: Vec<RawVals>,
    #[serde(default)]
    update: Vec<String>,
    #[serde(default)]
    accumulate: Vec<(String, serde_json::Value)>,
}

impl<'de> serde::Deserialize<'de> for UpsertCall {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let wire = UpsertCallWire::deserialize(deserializer)?;
        Ok(Self {
            key: Cow::Owned(wire.key),
            rows: wire.rows,
            update: wire.update.into_iter().map(Cow::Owned).collect(),
            accumulate: wire
                .accumulate
                .into_iter()
                .map(|(field, floor)| {
                    let floor = (!floor.is_null()).then(|| rt::json_cell(floor));
                    (Cow::Owned(field), floor)
                })
                .collect(),
        })
    }
}

/// What an upsert did with an input row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UpsertOutcome {
    /// The key was new: the row was inserted.
    Inserted,
    /// The key existed and an `update` or `accumulate` field changed it.
    Updated,
    /// The key existed and nothing changed: the row was not written.
    Unchanged,
}

impl UpsertOutcome {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
        }
    }

    fn from_wire_name(name: &str) -> Option<Self> {
        match name {
            "inserted" => Some(Self::Inserted),
            "updated" => Some(Self::Updated),
            "unchanged" => Some(Self::Unchanged),
            _ => None,
        }
    }
}

/// The result of one input row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpsertedRow {
    id: RecId,
    outcome: UpsertOutcome,
    previous_version: Option<i64>,
    version: Option<i64>,
}

impl UpsertedRow {
    /// The record (rows that share a key share it).
    pub fn id(&self) -> RecId {
        self.id
    }

    /// What happened to the row.
    pub fn outcome(&self) -> UpsertOutcome {
        self.outcome
    }

    /// The row version before the call: `None` for an inserted row and for models without row
    /// versions.
    pub fn previous_version(&self) -> Option<i64> {
        self.previous_version
    }

    /// The row version after the call (`None` for models without row versions).
    pub fn version(&self) -> Option<i64> {
        self.version
    }
}

/// The result of an upsert: one [`UpsertedRow`] per input row, in input order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpsertResult {
    rows: Vec<UpsertedRow>,
}

impl UpsertResult {
    /// One entry per input row, in input order.
    pub fn rows(&self) -> &[UpsertedRow] {
        &self.rows
    }

    /// Number of input rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// No input rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The ids of the input rows, in input order (rows sharing a key repeat their id).
    pub fn ids(&self) -> Vec<RecId> {
        self.rows.iter().map(|r| r.id).collect()
    }

    /// The distinct ids, in first-occurrence order.
    pub fn distinct_ids(&self) -> Vec<RecId> {
        // not a HashSet: its random seed is a WASI import this host does not provide
        let mut seen = BTreeSet::new();
        self.rows
            .iter()
            .map(|r| r.id)
            .filter(|id| seen.insert(id.get()))
            .collect()
    }
}

impl serde::Serialize for UpsertResult {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.rows.iter().map(|row| {
            (
                row.id.get(),
                row.outcome.wire_name(),
                row.previous_version,
                row.version,
            )
        }))
    }
}

impl<'de> serde::Deserialize<'de> for UpsertResult {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        let wire = Vec::<(i64, String, Option<i64>, Option<i64>)>::deserialize(deserializer)?;
        let rows = wire
            .into_iter()
            .map(|(id, outcome, previous_version, version)| {
                Ok(UpsertedRow {
                    id: RecId::from_real(id)
                        .ok_or_else(|| D::Error::custom(format!("{id} is not a record id")))?,
                    outcome: UpsertOutcome::from_wire_name(&outcome).ok_or_else(|| {
                        D::Error::custom(format!(
                            "'{outcome}' is not one of inserted, updated, unchanged"
                        ))
                    })?,
                    previous_version,
                    version,
                })
            })
            .collect::<std::result::Result<Vec<_>, D::Error>>()?;
        Ok(Self { rows })
    }
}
