//! The wire form of the core `create`, `write` and `unlink`, of
//! `name_create`, `action_archive` and `action_unarchive` (WASM.WIRE-NATIVE), and of `copy`,
//! `copy_data`, `default_get`, `exists`, `filtered_domain` and `upsert`
//! (WASM.WIRE-NATIVE.CORE-CODECS).
//!
//! A WebAssembly addon overrides these like a native addon does, and its `super()` reaches
//! the core implementation in `base`. The chain then crosses the native/WebAssembly seam in JSON,
//! so the core implementations need a [`MethodCodec`]; `#[method(wire)]` cannot be written on them
//! (they are not declared with `#[methods]`), so they are registered here and added by
//! [`crate::Dispatch::build_with`] (a `linkme` element in this crate would need `unsafe`, which the
//! workspace forbids).
//!
//! | Method | Arguments (JSON array, the argument tuple) | Result |
//! |---|---|---|
//! | `create` | `[[vals, ...]]` | the ids of the new records, `[id, ...]` |
//! | `write` | `[vals]` | `null` |
//! | `unlink` | `null` | `null` |
//! | `action_archive`, `action_unarchive` | `null` | `null` |
//! | `name_create` | `[name]` | `[id, display_name]` |
//! | `copy` | `[default]`, `default` a values object or `null` | the ids of the copies, `[id, ...]` |
//! | `copy_data` | `[default]` | one entry per record, a values object or `null`: `[vals, ...]` |
//! | `default_get` | `[[field, ...]]` | a values object |
//! | `exists` | `null` | the ids that exist, `[id, ...]`, each one of the records it ran on |
//! | `filtered_domain` | `[domain]`, the domain in list syntax | the matching ids, each one of the records it ran on |
//! | `upsert` | `[call]`, see below | one entry per input row: `[[id, outcome, previous_version, version], ...]` |
//!
//! The `call` of `upsert` is the object `{"key": name, "rows": [vals, ...], "update": [field, ...],
//! "accumulate": [[field, floor], ...]}`: `key` names a declared key, `rows`, `update` and
//! `accumulate` may be left out, a `floor` is a value or `null` for none. In its result `outcome`
//! is one of `inserted`, `updated`, `unchanged` and the versions are a number or `null`.
//!
//! `vals` is a JSON object, field name to value.
//! Decoding goes through the one converter the RPC surface uses
//! ([`Records::vals_from_values`]), so a value is checked against its field exactly as a value
//! from a client, and a malformed document is a `Validation` error naming the field.
//!
//! The values object of `copy`, `copy_data` and `default_get` is the untyped map a client sends
//! and reads (`ValueMap`): it crosses value by value ([`wasm_abi1_value_to_json`] out,
//! [`json_to_value`] in), and the receiving method checks each value against its field as it does
//! for a client. The rows and floors of `upsert` are converted with their fields before the call
//! ([`Records::upsert_call_from_values`]). A domain crosses in list syntax and comes back as
//! external input ([`Domain::from_json`]): internal operators do not cross. Every document is
//! bounded before its values are read ([`MAX_WIRE_BATCH`] entries, the value limits of
//! [`crate::wasm_abi1`]), and the fields without a form in this ABI are refused by name as for
//! `create` and `write` ([`crate::wasm_abi1`]).
//!
//! What a WebAssembly implementation answers is checked before typed code sees it: `exists` and
//! `filtered_domain` answer a subset of the records they ran on, each id once; `copy` and
//! `copy_data` answer one entry per record; `default_get` answers only fields it was asked for
//! ([`MethodCodec::admit_result`]); an `upsert` result has one row per input row
//! ([`Records::upsert`]). The answers of `copy` and `upsert` are not compared with what their
//! `super()` did (the host does that for `create` only). Which capabilities an addon needs to
//! answer at all is the host's rule (`erp-wasm-host`, `bridge.rs`).

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::sync::Arc;

use erp_domain::Domain;
use erp_fields::convert::value_to_json;
use erp_fields::{Error, RecId, Result, Value, ValueMap, serde_json};

use crate::Transaction;
use crate::commands::Command;
use crate::dispatch::{
    BoxFuture, Chain, MethodCodec, MethodCodecImpl, MethodFn, TypedCodec, invoke,
};
use crate::ir_default::json_to_value;
use crate::records::Records;
use crate::upsert::{UpsertCall, UpsertOutcome, UpsertResult, UpsertedRow};
use crate::vals::RawVals;

/// Most records one `create` document may carry across the boundary. The ORM's own batch limits
/// apply after decoding; this bounds the work spent on a hostile document first.
pub(crate) const MAX_WIRE_BATCH: usize = 100_000;

/// Serialize an ABI1 data value without pretending a native scalar is an ordinary string.
/// Ordinary JSON documents retain their untyped JSON contract. This is not a field ACL check.
///
/// # Errors
/// `Programming` for a native or unknown value kind requiring a future ABI major;
/// errors from reading a result row or converting a legacy scalar are preserved.
#[doc(hidden)]
pub fn wasm_abi1_value_to_json(value: &Value) -> Result<serde_json::Value> {
    fn convert(value: &Value, depth: usize, remaining: &mut usize) -> Result<serde_json::Value> {
        use serde_json::Value as J;
        if depth > crate::wasm_abi1::MAX_DEPTH {
            return Err(Error::new(
                erp_fields::ErrorKind::ResourceExhausted,
                "WASM ABI1 value exceeds its size or recursion admission limit",
            ));
        }
        *remaining = remaining.checked_sub(1).ok_or_else(|| {
            Error::new(
                erp_fields::ErrorKind::ResourceExhausted,
                "WASM ABI1 value exceeds its size or recursion admission limit",
            )
        })?;
        let unsupported = |name: &str| {
            crate::wasm_abi1::native_error(format!(
                "native {name} values are not supported across frozen WASM ABI1; requires a future ABI major with typed native fields"
            ))
        };
        let next = depth.checked_add(1).ok_or_else(|| {
            Error::new(
                erp_fields::ErrorKind::ResourceExhausted,
                "WASM ABI1 value exceeds its size or recursion admission limit",
            )
        })?;
        Ok(match value {
            Value::BigInt(_) => return Err(unsupported("bigint")),
            Value::Uuid(_) => return Err(unsupported("uuid")),
            Value::Xid8(_) => return Err(unsupported("xid8")),
            Value::TimestampTz(_) => return Err(unsupported("timestamptz")),
            Value::Float(value) if !value.is_finite() => return Err(Error::programming("a non-finite float cannot cross WASM ABI1")),
            Value::List(items) => J::Array(items.iter().map(|value| convert(value, next, remaining)).collect::<Result<_>>()?),
            Value::Map(map) => J::Object(map.iter().map(|(key, value)| Ok((key.clone(), convert(value, next, remaining)?))).collect::<Result<_>>()?),
            Value::Row(row) => J::Object(row.iter().map(|item| { let (key, value) = item?; Ok((key.to_owned(), convert(&value, next, remaining)?)) }).collect::<Result<_>>()?),
            Value::Rows(rows) => J::Array(rows.iter()?.map(|row| convert(&Value::Row(row), next, remaining)).collect::<Result<_>>()?),
            Value::False | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Decimal(_)
            | Value::Str(_) | Value::Bytes(_) | Value::Date(_) | Value::DateTime(_)
            | Value::Json(_) | Value::Range(_) => value_to_json(value)?,
            _ => return Err(crate::wasm_abi1::native_error("this value kind is not supported across frozen WASM ABI1; requires a future ABI major".to_owned())),
        })
    }
    let mut remaining = crate::wasm_abi1::MAX_VALUE_NODES;
    convert(value, 0, &mut remaining)
}

/// `vals` as the JSON object of the wire form.
///
/// # Errors
/// `Programming` when a value has no JSON form.
pub(crate) fn vals_to_json(vals: &RawVals) -> Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for (name, cell) in vals.iter() {
        map.insert(
            name.to_owned(),
            wasm_abi1_value_to_json(&Value::from_cell(cell))?,
        );
    }
    for (name, commands) in vals.iter_commands() {
        let items = commands
            .iter()
            .map(|c: &Command| wasm_abi1_value_to_json(&c.to_value()))
            .collect::<Result<Vec<_>>>()?;
        map.insert(name.to_owned(), serde_json::Value::Array(items));
    }
    Ok(serde_json::Value::Object(map))
}

/// The values of the wire form `json`, converted with the fields of `records`' model.
///
/// This route invokes the method chain directly, without the generated-input adapter (
/// D2: no wire codec), so every generated name, flagged or not, is refused here before any
/// override sees it.
///
/// # Errors
/// `Validation` when `json` is not an object, names a generated field, or a value does not fit
/// its field.
pub(crate) fn vals_from_json(
    tx: &Transaction,
    records: &Records,
    json: &serde_json::Value,
) -> Result<RawVals> {
    let serde_json::Value::Object(map) = json else {
        return Err(Error::validation(format!(
            "the values of a write on '{}' must be a JSON object, got {json}",
            records.model_name()
        )));
    };
    crate::database_generated::check_write_names(
        tx,
        records.spec(),
        map.keys().map(String::as_str),
    )?;
    let values: Vec<(String, Value)> = map
        .iter()
        .map(|(name, value)| (name.clone(), json_to_value(value)))
        .collect();
    records.vals_from_values(values.iter().map(|(name, value)| (name, value)))
}

fn bad_args(method: &str, error: &serde_json::Error) -> Error {
    Error::validation(format!(
        "arguments of '{method}' do not match its wire form: {error}"
    ))
}

fn ids_to_json(records: &Records) -> Result<String> {
    let ids: Vec<i64> = records.rec_ids().iter().map(|id| id.get()).collect();
    serde_json::to_string(&ids)
        .map_err(|e| Error::programming(format!("record ids are not serialisable: {e}")))
}

type CreateFn = MethodFn<(Vec<RawVals>,), Records>;
type WriteFn = MethodFn<(RawVals,), ()>;
type UnlinkFn = MethodFn<(), ()>;

struct CreateCodec;

impl MethodCodec for CreateCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<CreateFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        let args = args
            .downcast::<(Vec<RawVals>,)>()
            .map_err(|_| Error::programming("create codec used with arguments of another type"))?;
        let list = args
            .0
            .iter()
            .map(vals_to_json)
            .collect::<Result<Vec<_>>>()?;
        serde_json::to_string(&(list,))
            .map_err(|e| Error::programming(format!("create arguments are not serialisable: {e}")))
    }

    fn decode_result(&self, records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        let ids: Vec<i64> = serde_json::from_str(json).map_err(|e| {
            Error::validation(format!(
                "a WASM create returned something that is not a list of ids: {e}"
            ))
        })?;
        let ids = ids
            .into_iter()
            .map(|id| {
                RecId::from_real(id).ok_or_else(|| {
                    Error::validation(format!("a WASM create returned the record id {id}"))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Box::new(records.browse_ids(ids)))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let (list,): (Vec<serde_json::Value>,) =
                serde_json::from_str(args_json).map_err(|e| bad_args("create", &e))?;
            if list.len() > MAX_WIRE_BATCH {
                return Err(Error::new(
                    erp_fields::ErrorKind::ResourceExhausted,
                    format!(
                        "create received {} records across the WebAssembly boundary; at most \
                         {MAX_WIRE_BATCH} per call",
                        list.len()
                    ),
                ));
            }
            let vals_list = list
                .iter()
                .map(|doc| vals_from_json(tx, records, doc))
                .collect::<Result<Vec<_>>>()?;
            let created =
                invoke::<(Vec<RawVals>,), Records>(chain, index, tx, records, (vals_list,)).await?;
            ids_to_json(&created)
        })
    }
}

struct WriteCodec;

impl MethodCodec for WriteCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<WriteFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        let args = args
            .downcast::<(RawVals,)>()
            .map_err(|_| Error::programming("write codec used with arguments of another type"))?;
        serde_json::to_string(&(vals_to_json(&args.0)?,))
            .map_err(|e| Error::programming(format!("write arguments are not serialisable: {e}")))
    }

    fn decode_result(&self, _records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        let unit: () = serde_json::from_str(json).map_err(|e| {
            Error::validation(format!("a WASM write returned something but null: {e}"))
        })?;
        Ok(Box::new(unit))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let (doc,): (serde_json::Value,) =
                serde_json::from_str(args_json).map_err(|e| bad_args("write", &e))?;
            let vals = vals_from_json(tx, records, &doc)?;
            invoke::<(RawVals,), ()>(chain, index, tx, records, (vals,)).await?;
            Ok("null".to_owned())
        })
    }
}

struct UnlinkCodec;

impl MethodCodec for UnlinkCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<UnlinkFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        args.downcast::<()>()
            .map_err(|_| Error::programming("unlink codec used with arguments of another type"))?;
        Ok("null".to_owned())
    }

    fn decode_result(&self, _records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        let unit: () = serde_json::from_str(json).map_err(|e| {
            Error::validation(format!("a WASM unlink returned something but null: {e}"))
        })?;
        Ok(Box::new(unit))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let _: () = serde_json::from_str(args_json).map_err(|e| bad_args("unlink", &e))?;
            invoke::<(), ()>(chain, index, tx, records, ()).await?;
            Ok("null".to_owned())
        })
    }
}

static CREATE_CODEC: MethodCodecImpl = MethodCodecImpl {
    addon: "base",
    model: "base",
    method: "create",
    codec: &CreateCodec,
};
static WRITE_CODEC: MethodCodecImpl = MethodCodecImpl {
    addon: "base",
    model: "base",
    method: "write",
    codec: &WriteCodec,
};
static UNLINK_CODEC: MethodCodecImpl = MethodCodecImpl {
    addon: "base",
    model: "base",
    method: "unlink",
    codec: &UnlinkCodec,
};

type NameCreateFn = MethodFn<(String,), (i64, Arc<str>)>;

/// `name_create`: `[name]` in, `[id, display_name]` out. Written by hand because `Arc<str>` has
/// no serde implementation without serde's `rc` feature, which the workspace does not enable.
struct NameCreateCodec;

impl MethodCodec for NameCreateCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<NameCreateFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        let args = args.downcast::<(String,)>().map_err(|_| {
            Error::programming("name_create codec used with arguments of another type")
        })?;
        serde_json::to_string(&*args).map_err(|e| {
            Error::programming(format!("name_create arguments are not serialisable: {e}"))
        })
    }

    fn decode_result(&self, _records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        let (id, name): (i64, String) = serde_json::from_str(json).map_err(|e| {
            Error::validation(format!(
                "a WASM name_create returned something but [id, display_name]: {e}"
            ))
        })?;
        let result: (i64, Arc<str>) = (id, Arc::from(name));
        Ok(Box::new(result))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let args: (String,) =
                serde_json::from_str(args_json).map_err(|e| bad_args("name_create", &e))?;
            let (id, name) =
                invoke::<(String,), (i64, Arc<str>)>(chain, index, tx, records, args).await?;
            serde_json::to_string(&(id, &*name)).map_err(|e| {
                Error::programming(format!("name_create result is not serialisable: {e}"))
            })
        })
    }
}

static NAME_CREATE_CODEC: MethodCodecImpl = MethodCodecImpl {
    addon: "base",
    model: "base",
    method: "name_create",
    codec: &NameCreateCodec,
};
static ACTION_ARCHIVE_CODEC: MethodCodecImpl = MethodCodecImpl {
    addon: "base",
    model: "base",
    method: "action_archive",
    codec: &TypedCodec::<(), ()>::new(),
};
static ACTION_UNARCHIVE_CODEC: MethodCodecImpl = MethodCodecImpl {
    addon: "base",
    model: "base",
    method: "action_unarchive",
    codec: &TypedCodec::<(), ()>::new(),
};

// ---- copy, copy_data, default_get, exists, filtered_domain, upsert ----------------------------
// (WASM.WIRE-NATIVE.CORE-CODECS)

fn too_many(method: &str, what: &str, count: usize) -> Error {
    Error::new(
        erp_fields::ErrorKind::ResourceExhausted,
        format!(
            "{method} carried {count} {what} across the WebAssembly boundary; at most \
             {MAX_WIRE_BATCH} per call"
        ),
    )
}

fn document_limit(method: &str) -> Error {
    Error::new(
        erp_fields::ErrorKind::ResourceExhausted,
        format!("the wire document of '{method}' exceeds its size or recursion admission limit"),
    )
}

/// Most bytes of untrusted text (a parser's message, a name) an error of this module repeats.
const MAX_ECHO: usize = 160;

/// `text`, cut to [`MAX_ECHO`] bytes: a parser's message quotes the value it refused and a name
/// comes from the document, so neither is repeated at the length its sender chose.
fn brief(text: &str) -> String {
    if text.len() <= MAX_ECHO {
        return text.to_owned();
    }
    let mut end = MAX_ECHO;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

/// A field name of a wire document: longer than an identifier can be, it names no field and is
/// refused before anything repeats it. `what` names the document.
fn admit_name(what: &str, name: &str) -> Result<()> {
    if name.len() > erp_registry::ident::MAX_IDENTIFIER_LEN {
        return Err(Error::validation(format!(
            "{what} names a field with {} bytes; a field name has at most {}",
            name.len(),
            erp_registry::ident::MAX_IDENTIFIER_LEN
        )));
    }
    Ok(())
}

/// The JSON kind of a document, for an error that must not repeat the document.
fn json_kind(json: &serde_json::Value) -> &'static str {
    match json {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "text",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Parse one wire document of `method` and bound it (bytes, nodes, depth: the limits of
/// [`crate::wasm_abi1`]) before anything reads its values. `refuse` words the parse error.
fn parse_document(
    method: &str,
    json: &str,
    refuse: impl FnOnce(&serde_json::Error) -> Error,
) -> Result<serde_json::Value> {
    if json.len() > crate::wasm_abi1::MAX_PACKET_BYTES {
        return Err(document_limit(method));
    }
    let document: serde_json::Value = serde_json::from_str(json).map_err(|e| refuse(&e))?;
    let mut remaining = crate::wasm_abi1::MAX_VALUE_NODES;
    let mut pending = vec![(&document, 0_usize)];
    while let Some((node, depth)) = pending.pop() {
        if depth > crate::wasm_abi1::MAX_DEPTH {
            return Err(document_limit(method));
        }
        remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| document_limit(method))?;
        match node {
            serde_json::Value::Array(items) => {
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            serde_json::Value::Object(map) => {
                pending.extend(map.values().map(|item| (item, depth + 1)));
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    Ok(document)
}

/// The arguments a WebAssembly layer passed to `super()` of `method`, bounded.
fn parse_args(method: &str, json: &str) -> Result<serde_json::Value> {
    parse_document(method, json, |e| bad_shape(method, e))
}

/// [`bad_args`] for a document read by this module's codecs: the parser's message is cut.
fn bad_shape(method: &str, error: &serde_json::Error) -> Error {
    Error::validation(format!(
        "arguments of '{method}' do not match its wire form: {}",
        brief(&error.to_string())
    ))
}

/// The result a WebAssembly implementation of `method` answered, bounded.
fn parse_result(method: &str, json: &str) -> Result<serde_json::Value> {
    parse_document(method, json, |e| {
        Error::validation(format!(
            "a WASM {method} returned a document that is not JSON: {}",
            brief(&e.to_string())
        ))
    })
}

/// The one argument of a method whose argument tuple has one element (`[argument]`).
fn single_arg(method: &str, document: serde_json::Value) -> Result<serde_json::Value> {
    match document {
        serde_json::Value::Array(mut items) if items.len() == 1 => Ok(items.remove(0)),
        other => Err(Error::validation(format!(
            "arguments of '{method}' do not match its wire form: expected a list of one \
             argument, got {}",
            match &other {
                serde_json::Value::Array(items) => format!("a list of {}", items.len()),
                other => json_kind(other).to_owned(),
            }
        ))),
    }
}

fn to_text(what: &str, value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| Error::programming(format!("{what} is not serialisable: {e}")))
}

/// A values object (field name to value) of the wire form. A value without a form in this ABI
/// is refused by [`wasm_abi1_value_to_json`], never sent as text.
fn value_map_to_json(map: &ValueMap) -> Result<serde_json::Value> {
    let mut object = serde_json::Map::new();
    for (name, value) in map {
        object.insert(name.clone(), wasm_abi1_value_to_json(value)?);
    }
    Ok(serde_json::Value::Object(object))
}

/// The value map of a wire-form values object. The values stay untyped, as a client's: the
/// method that receives them converts each against its field. `what` names the document
/// (`the default of 'copy'`).
fn value_map_from_json(what: &str, json: &serde_json::Value) -> Result<ValueMap> {
    let serde_json::Value::Object(object) = json else {
        return Err(Error::validation(format!(
            "{what} must be a JSON object, got {}",
            json_kind(json)
        )));
    };
    object
        .iter()
        .map(|(name, value)| {
            admit_name(what, name)?;
            Ok((name.clone(), json_to_value(value)))
        })
        .collect()
}

/// `default` of `copy` and `copy_data`: a values object, or `null` for none.
fn default_from_json(method: &str, json: &serde_json::Value) -> Result<Option<ValueMap>> {
    if json.is_null() {
        return Ok(None);
    }
    value_map_from_json(&format!("the default of '{method}'"), json).map(Some)
}

fn default_to_json(default: Option<&ValueMap>) -> Result<serde_json::Value> {
    default.map_or(Ok(serde_json::Value::Null), value_map_to_json)
}

/// What the ids a WebAssembly implementation answered must satisfy.
#[derive(Clone, Copy)]
enum Answered {
    /// A filter (`exists`, `filtered_domain`): each id is one of the records the method ran on,
    /// and none comes twice, so the answer is a subset its caller can count and act on.
    Subset,
    /// `copy`: one record per record the method ran on.
    OnePerRecord,
}

/// The recordset a WebAssembly implementation of `method` answered as a list of ids, in the
/// environment of the records it ran on, checked against `rule`.
fn records_from_json(
    method: &str,
    records: &Records,
    json: &str,
    rule: Answered,
) -> Result<Records> {
    let ids: Vec<i64> = serde_json::from_value(parse_result(method, json)?).map_err(|e| {
        Error::validation(format!(
            "a WASM {method} returned something that is not a list of ids: {}",
            brief(&e.to_string())
        ))
    })?;
    let ran_on = records.rec_ids().len();
    if matches!(rule, Answered::OnePerRecord) && ids.len() != ran_on {
        return Err(Error::validation(format!(
            "a WASM {method} returned {} records for the {ran_on} records it ran on",
            ids.len()
        )));
    }
    if matches!(rule, Answered::Subset) && ids.len() > ran_on {
        return Err(Error::validation(format!(
            "a WASM {method} returned {} record ids, more than the {ran_on} records it ran on",
            ids.len()
        )));
    }
    let own: rustc_hash::FxHashSet<RecId> = match rule {
        Answered::Subset => records.rec_ids().iter().copied().collect(),
        Answered::OnePerRecord => rustc_hash::FxHashSet::default(),
    };
    let mut seen = rustc_hash::FxHashSet::default();
    let ids = ids
        .into_iter()
        .map(|id| {
            let rec = RecId::from_real(id).ok_or_else(|| {
                Error::validation(format!("a WASM {method} returned the record id {id}"))
            })?;
            if matches!(rule, Answered::Subset) {
                if !own.contains(&rec) {
                    return Err(Error::validation(format!(
                        "a WASM {method} returned the record id {id}, which is not one of the \
                         records it ran on"
                    )));
                }
                if !seen.insert(rec) {
                    return Err(Error::validation(format!(
                        "a WASM {method} returned the record id {id} more than once"
                    )));
                }
            }
            Ok(rec)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(records.browse_ids(ids))
}

type CopyFn = MethodFn<(Option<ValueMap>,), Records>;
type CopyDataFn = MethodFn<(Option<ValueMap>,), Vec<Option<ValueMap>>>;
type DefaultGetFn = MethodFn<(Vec<String>,), ValueMap>;
type ExistsFn = MethodFn<(), Records>;
type FilteredDomainFn = MethodFn<(Domain,), Records>;
type UpsertFn = MethodFn<(UpsertCall,), UpsertResult>;

/// `copy`: `[default]` in, the ids of the copies out.
struct CopyCodec;

impl MethodCodec for CopyCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<CopyFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        let args = args
            .downcast::<(Option<ValueMap>,)>()
            .map_err(|_| Error::programming("copy codec used with arguments of another type"))?;
        to_text("copy arguments", &(default_to_json(args.0.as_ref())?,))
    }

    fn decode_result(&self, records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        Ok(Box::new(records_from_json(
            "copy",
            records,
            json,
            Answered::OnePerRecord,
        )?))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let default = single_arg("copy", parse_args("copy", args_json)?)?;
            let default = default_from_json("copy", &default)?;
            let copies =
                invoke::<(Option<ValueMap>,), Records>(chain, index, tx, records, (default,))
                    .await?;
            ids_to_json(&copies)
        })
    }
}

/// `copy_data`: `[default]` in, one values object (or `null`) per record out.
struct CopyDataCodec;

impl MethodCodec for CopyDataCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<CopyDataFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        let args = args.downcast::<(Option<ValueMap>,)>().map_err(|_| {
            Error::programming("copy_data codec used with arguments of another type")
        })?;
        to_text("copy_data arguments", &(default_to_json(args.0.as_ref())?,))
    }

    fn decode_result(&self, records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        let serde_json::Value::Array(items) = parse_result("copy_data", json)? else {
            return Err(Error::validation(
                "a WASM copy_data returned something that is not a list of values objects",
            ));
        };
        // one entry per record: `copy` creates a record for each entry, so another count would
        // make one copy into many (or none)
        let ran_on = records.rec_ids().len();
        if items.len() != ran_on {
            return Err(Error::validation(format!(
                "a WASM copy_data returned {} entries for the {ran_on} records it ran on",
                items.len()
            )));
        }
        let list = items
            .iter()
            .map(|item| {
                if item.is_null() {
                    return Ok(None);
                }
                let values = value_map_from_json("an entry a WASM copy_data returned", item)?;
                crate::wasm_abi1::refuse_native_names(records, values.keys().map(String::as_str))?;
                Ok(Some(values))
            })
            .collect::<Result<Vec<Option<ValueMap>>>>()?;
        Ok(Box::new(list))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let default = single_arg("copy_data", parse_args("copy_data", args_json)?)?;
            let default = default_from_json("copy_data", &default)?;
            let list = invoke::<(Option<ValueMap>,), Vec<Option<ValueMap>>>(
                chain,
                index,
                tx,
                records,
                (default,),
            )
            .await?;
            let items = list
                .iter()
                .map(|entry| default_to_json(entry.as_ref()))
                .collect::<Result<Vec<_>>>()?;
            to_text("copy_data result", &items)
        })
    }
}

/// `default_get`: `[[field, ...]]` in, a values object out.
struct DefaultGetCodec;

impl MethodCodec for DefaultGetCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<DefaultGetFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        let args = args.downcast::<(Vec<String>,)>().map_err(|_| {
            Error::programming("default_get codec used with arguments of another type")
        })?;
        to_text("default_get arguments", &*args)
    }

    fn decode_result(&self, records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        let document = parse_result("default_get", json)?;
        let values = value_map_from_json("the values a WASM default_get returned", &document)?;
        crate::wasm_abi1::refuse_native_names(records, values.keys().map(String::as_str))?;
        Ok(Box::new(values))
    }

    /// Only the fields that were asked for: the answer becomes values of a create, and a layer
    /// that could add any other field would set what its caller never asked a default for.
    fn admit_result(&self, args_json: &str, result: &(dyn Any + Send)) -> Result<()> {
        let values = result.downcast_ref::<ValueMap>().ok_or_else(|| {
            Error::programming("default_get codec used with a result of another type")
        })?;
        let (asked,): (Vec<String>,) = serde_json::from_str(args_json).map_err(|e| {
            Error::programming(format!(
                "default_get arguments are not in their wire form: {}",
                brief(&e.to_string())
            ))
        })?;
        let asked: rustc_hash::FxHashSet<&str> = asked.iter().map(String::as_str).collect();
        match values.keys().find(|name| !asked.contains(name.as_str())) {
            Some(name) => Err(Error::validation(format!(
                "a WASM default_get answered a value for '{}', which is not one of the fields \
                 it was asked for",
                brief(name)
            ))),
            None => Ok(()),
        }
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let (names,): (Vec<String>,) =
                serde_json::from_value(parse_args("default_get", args_json)?)
                    .map_err(|e| bad_shape("default_get", &e))?;
            if names.len() > MAX_WIRE_BATCH {
                return Err(too_many("default_get", "field names", names.len()));
            }
            let values =
                invoke::<(Vec<String>,), ValueMap>(chain, index, tx, records, (names,)).await?;
            to_text("default_get result", &value_map_to_json(&values)?)
        })
    }
}

/// `exists`: no argument, the ids that exist out (each one of the records it ran on).
struct ExistsCodec;

impl MethodCodec for ExistsCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<ExistsFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        args.downcast::<()>()
            .map_err(|_| Error::programming("exists codec used with arguments of another type"))?;
        Ok("null".to_owned())
    }

    fn decode_result(&self, records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        Ok(Box::new(records_from_json(
            "exists",
            records,
            json,
            Answered::Subset,
        )?))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let _: () = serde_json::from_str(args_json).map_err(|e| bad_args("exists", &e))?;
            let existing = invoke::<(), Records>(chain, index, tx, records, ()).await?;
            ids_to_json(&existing)
        })
    }
}

/// `filtered_domain`: `[domain]` in list syntax, the matching ids out (each one of the records
/// it ran on). The domain a layer passes down is external input ([`Domain::from_json`]).
struct FilteredDomainCodec;

impl MethodCodec for FilteredDomainCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<FilteredDomainFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        let args = args.downcast::<(Domain,)>().map_err(|_| {
            Error::programming("filtered_domain codec used with arguments of another type")
        })?;
        to_text("filtered_domain arguments", &(args.0.to_json()?,))
    }

    fn decode_result(&self, records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        Ok(Box::new(records_from_json(
            "filtered_domain",
            records,
            json,
            Answered::Subset,
        )?))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let domain = single_arg("filtered_domain", parse_args("filtered_domain", args_json)?)?;
            let domain = Domain::from_json(&domain)?;
            let matching =
                invoke::<(Domain,), Records>(chain, index, tx, records, (domain,)).await?;
            ids_to_json(&matching)
        })
    }
}

/// The call document of `upsert`, parsed and bounded but not yet converted with the fields.
struct UpsertDocument {
    key: String,
    rows: Vec<BTreeMap<String, Value>>,
    update: Vec<String>,
    accumulate: Vec<(String, Option<Value>)>,
}

fn upsert_document(document: serde_json::Value) -> Result<UpsertDocument> {
    let mut object = match document {
        serde_json::Value::Object(object) => object,
        other => {
            return Err(Error::validation(format!(
                "the call of 'upsert' must be a JSON object, got {}",
                json_kind(&other)
            )));
        }
    };
    if let Some(unknown) = object
        .keys()
        .find(|k| !matches!(k.as_str(), "key" | "rows" | "update" | "accumulate"))
    {
        return Err(Error::validation(format!(
            "the call of 'upsert' has the unknown entry '{}' (it takes key, rows, update and \
             accumulate)",
            brief(unknown)
        )));
    }
    let key = match object.remove("key") {
        Some(serde_json::Value::String(key)) => key,
        Some(other) => {
            return Err(Error::validation(format!(
                "the key of 'upsert' must be the name of a declared key, got {}",
                json_kind(&other)
            )));
        }
        None => {
            return Err(Error::validation(
                "the call of 'upsert' names no key (the name of a declared key)",
            ));
        }
    };
    let rows = match object.remove("rows") {
        None => Vec::new(),
        Some(serde_json::Value::Array(rows)) => rows,
        Some(other) => {
            return Err(Error::validation(format!(
                "the rows of 'upsert' must be a list of values objects, got {}",
                json_kind(&other)
            )));
        }
    };
    if rows.len() > MAX_WIRE_BATCH {
        return Err(too_many("upsert", "rows", rows.len()));
    }
    let rows = rows
        .iter()
        .map(|row| match row {
            serde_json::Value::Object(row) => row
                .iter()
                .map(|(name, value)| {
                    admit_name("a row of 'upsert'", name)?;
                    Ok((name.clone(), json_to_value(value)))
                })
                .collect(),
            other => Err(Error::validation(format!(
                "a row of 'upsert' must be a JSON object, got {}",
                json_kind(other)
            ))),
        })
        .collect::<Result<Vec<BTreeMap<String, Value>>>>()?;
    let update: Vec<String> = match object.remove("update") {
        None => Vec::new(),
        Some(update) => serde_json::from_value(update).map_err(|e| {
            Error::validation(format!(
                "the update of 'upsert' must be a list of field names: {}",
                brief(&e.to_string())
            ))
        })?,
    };
    let accumulate: Vec<(String, serde_json::Value)> = match object.remove("accumulate") {
        None => Vec::new(),
        Some(accumulate) => serde_json::from_value(accumulate).map_err(|e| {
            Error::validation(format!(
                "the accumulate of 'upsert' must be a list of [field, floor] pairs: {}",
                brief(&e.to_string())
            ))
        })?,
    };
    let accumulate = accumulate
        .into_iter()
        .map(|(field, floor)| {
            let floor = (!floor.is_null()).then(|| json_to_value(&floor));
            (field, floor)
        })
        .collect();
    Ok(UpsertDocument {
        key,
        rows,
        update,
        accumulate,
    })
}

fn upsert_call_to_json(call: &UpsertCall) -> Result<serde_json::Value> {
    let rows = call
        .rows()
        .iter()
        .map(vals_to_json)
        .collect::<Result<Vec<_>>>()?;
    let update = call
        .update()
        .map(|field| serde_json::Value::String(field.to_owned()))
        .collect();
    let accumulate = call
        .accumulate()
        .map(|(field, floor)| {
            let floor = match floor {
                Some(cell) => wasm_abi1_value_to_json(&Value::from_cell(cell))?,
                None => serde_json::Value::Null,
            };
            Ok(serde_json::Value::Array(vec![
                serde_json::Value::String(field.to_owned()),
                floor,
            ]))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut object = serde_json::Map::new();
    object.insert(
        "key".to_owned(),
        serde_json::Value::String(call.key().to_owned()),
    );
    object.insert("rows".to_owned(), serde_json::Value::Array(rows));
    object.insert("update".to_owned(), serde_json::Value::Array(update));
    object.insert(
        "accumulate".to_owned(),
        serde_json::Value::Array(accumulate),
    );
    Ok(serde_json::Value::Object(object))
}

/// One row of the wire form of an upsert result: `[id, outcome, previous_version, version]`.
type UpsertRowWire = (i64, String, Option<i64>, Option<i64>);

/// `upsert`: `[call]` in, one `[id, outcome, previous_version, version]` per input row out.
struct UpsertCodec;

impl MethodCodec for UpsertCodec {
    fn signature(&self) -> TypeId {
        TypeId::of::<UpsertFn>()
    }

    fn encode_args(&self, args: Box<dyn Any + Send>) -> Result<String> {
        let args = args
            .downcast::<(UpsertCall,)>()
            .map_err(|_| Error::programming("upsert codec used with arguments of another type"))?;
        to_text("upsert arguments", &(upsert_call_to_json(&args.0)?,))
    }

    fn decode_result(&self, _records: &Records, json: &str) -> Result<Box<dyn Any + Send>> {
        let rows: Vec<UpsertRowWire> = serde_json::from_value(parse_result("upsert", json)?)
            .map_err(|e| {
                Error::validation(format!(
                    "a WASM upsert returned something that is not a list of [id, outcome, \
                     previous_version, version]: {}",
                    brief(&e.to_string())
                ))
            })?;
        if rows.len() > MAX_WIRE_BATCH {
            return Err(too_many("upsert", "result rows", rows.len()));
        }
        let rows = rows
            .into_iter()
            .map(|(id, outcome, previous_version, version)| {
                let rec = RecId::from_real(id).ok_or_else(|| {
                    Error::validation(format!("a WASM upsert returned the record id {id}"))
                })?;
                let outcome = UpsertOutcome::from_wire_name(&outcome).ok_or_else(|| {
                    Error::validation(
                        "a WASM upsert returned an outcome that is not one of inserted, updated, \
                         unchanged",
                    )
                })?;
                Ok(UpsertedRow::from_wire(
                    rec,
                    outcome,
                    previous_version,
                    version,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Box::new(UpsertResult::from_rows(rows)))
    }

    fn call_native<'a>(
        &'a self,
        chain: Chain,
        index: usize,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let document =
                upsert_document(single_arg("upsert", parse_args("upsert", args_json)?)?)?;
            let update: Vec<&str> = document.update.iter().map(String::as_str).collect();
            let accumulate: Vec<(&str, Option<Value>)> = document
                .accumulate
                .iter()
                .map(|(field, floor)| (field.as_str(), floor.clone()))
                .collect();
            let call = records.upsert_call_from_values(
                tx,
                &document.key,
                &document.rows,
                &update,
                &accumulate,
            )?;
            let result =
                invoke::<(UpsertCall,), UpsertResult>(chain, index, tx, records, (call,)).await?;
            let rows: Vec<(i64, &str, Option<i64>, Option<i64>)> = result
                .rows()
                .iter()
                .map(|row| {
                    (
                        row.id().get(),
                        row.outcome().wire_name(),
                        row.previous_version(),
                        row.version(),
                    )
                })
                .collect();
            to_text("upsert result", &rows)
        })
    }
}

macro_rules! core_codec {
    ($name:ident, $method:expr, $codec:expr) => {
        static $name: MethodCodecImpl = MethodCodecImpl {
            addon: "base",
            model: "base",
            method: $method,
            codec: &$codec,
        };
    };
}

core_codec!(COPY_CODEC, "copy", CopyCodec);
core_codec!(COPY_DATA_CODEC, "copy_data", CopyDataCodec);
core_codec!(DEFAULT_GET_CODEC, "default_get", DefaultGetCodec);
core_codec!(EXISTS_CODEC, "exists", ExistsCodec);
core_codec!(
    FILTERED_DOMAIN_CODEC,
    "filtered_domain",
    FilteredDomainCodec
);
core_codec!(UPSERT_CODEC, "upsert", UpsertCodec);

/// The codecs of the core `create`, `write` and `unlink`, of `name_create`, `action_archive` and
/// `action_unarchive` (WASM.WIRE-NATIVE), and of `copy`, `copy_data`, `default_get`, `exists`,
/// `filtered_domain` and `upsert` (WASM.WIRE-NATIVE.CORE-CODECS): every method the core registers
/// by hand has one.
pub(crate) static BUILTIN_CODECS: [&MethodCodecImpl; 12] = [
    &CREATE_CODEC,
    &WRITE_CODEC,
    &UNLINK_CODEC,
    &NAME_CREATE_CODEC,
    &ACTION_ARCHIVE_CODEC,
    &ACTION_UNARCHIVE_CODEC,
    &COPY_CODEC,
    &COPY_DATA_CODEC,
    &DEFAULT_GET_CODEC,
    &EXISTS_CODEC,
    &FILTERED_DOMAIN_CODEC,
    &UPSERT_CODEC,
];

#[cfg(test)]
mod native_fields_tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use erp_fields::{TimestampTz, Uuid, Xid8};

    #[test]
    fn native_fields_abi1_json_refuses_nested_tagged_scalars_without_inferring_strings() {
        for (value, name) in [
            (
                Value::BigInt(erp_fields::BigInt::from_i64(i64::MAX)),
                "bigint",
            ),
            (Value::Uuid(Uuid::NIL), "uuid"),
            (Value::Xid8(Xid8::from_u64(u64::MAX)), "xid8"),
            (Value::TimestampTz(TimestampTz::UNIX_EPOCH), "timestamptz"),
        ] {
            let error = wasm_abi1_value_to_json(&Value::list([value])).unwrap_err();
            assert_eq!(error.kind(), erp_fields::ErrorKind::Programming);
            assert_eq!(
                error.message(),
                format!(
                    "native {name} values are not supported across frozen WASM ABI1; requires a future ABI major with typed native fields"
                )
            );
        }
        assert_eq!(
            wasm_abi1_value_to_json(&Value::str("18446744073709551615")).unwrap(),
            serde_json::json!("18446744073709551615")
        );
        assert_eq!(
            wasm_abi1_value_to_json(&Value::False).unwrap(),
            serde_json::json!(false)
        );
    }
    #[test]
    fn native_fields_abi1_value_guard_bounds_recursion_and_never_substitutes_null() {
        let mut nested = Value::False;
        for _ in 0..=crate::wasm_abi1::MAX_DEPTH {
            nested = Value::list([nested]);
        }
        let error = wasm_abi1_value_to_json(&nested).unwrap_err();
        assert_eq!(error.kind(), erp_fields::ErrorKind::ResourceExhausted);
        assert_eq!(
            error.message(),
            "WASM ABI1 value exceeds its size or recursion admission limit"
        );
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = wasm_abi1_value_to_json(&Value::Float(value)).unwrap_err();
            assert_eq!(error.kind(), erp_fields::ErrorKind::Programming);
            assert_eq!(error.message(), "a non-finite float cannot cross WASM ABI1");
        }
    }
}
