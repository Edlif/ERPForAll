//! Field admission for the frozen ABI1 bridge. Metadata and caller rights are
//! checked before a new native scalar can become an ambiguous string/NULL.
//! This module only refuses operations; it grants no rights or storage authority.

use std::any::Any;
use std::collections::VecDeque;
use std::error::Error as StdError;
use std::fmt;

use erp_fields::{Cell, Error, ErrorKind, FieldType, RecId, Result, ValueMap, serde_json};
use erp_registry::ModelId;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::access::Operation;
use crate::field_access::FieldOp;
use crate::upsert::UpsertCall;
use crate::{Command, Env, RawVals, Records, Transaction};

// Admission limits, not performance measurements. The byte bound preserves the
// existing host hard payload ceiling; narrower per-addon limits still apply.
pub(crate) const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_DEPTH: usize = 64;
pub(crate) const MAX_VALUE_NODES: usize = 1_000_000;
const MAX_RECORD_MAPS: usize = 500_000;
const MAX_FIELD_MENTIONS: usize = 1_000_000;

#[derive(Debug)]
struct NativeRefusal(String);
impl fmt::Display for NativeRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl StdError for NativeRefusal {}

pub(crate) fn native_error(message: String) -> Error {
    Error::programming(message.clone()).with_source(NativeRefusal(message))
}

/// Whether an error is this bridge's typed native refusal, rather than a policy
/// or ordinary validation error. No message text controls classification.
#[doc(hidden)]
pub fn is_wasm_abi1_native_refusal(error: &Error) -> bool {
    if error.kind() != ErrorKind::Programming {
        return false;
    }
    let mut source = StdError::source(error);
    for _ in 0..16 {
        let Some(current) = source else { return false };
        if current.is::<NativeRefusal>() {
            return true;
        }
        source = current.source();
    }
    false
}

pub(crate) fn native_type(ty: &FieldType) -> Option<&'static str> {
    match ty {
        FieldType::BigInt => Some("bigint"),
        FieldType::Uuid => Some("uuid"),
        FieldType::Xid8 => Some("xid8"),
        FieldType::TimestampTz => Some("timestamptz"),
        // file content has no ABI1 value form
        FieldType::Binary { .. } => Some("binary"),
        FieldType::Id
        | FieldType::Boolean
        | FieldType::Integer
        | FieldType::Float { .. }
        | FieldType::Monetary { .. }
        | FieldType::Char { .. }
        | FieldType::Text
        | FieldType::Html
        | FieldType::Selection { .. }
        | FieldType::Date
        | FieldType::Datetime
        | FieldType::Json
        | FieldType::Many2one { .. }
        | FieldType::One2many { .. }
        | FieldType::Many2many { .. }
        | FieldType::Reference { .. }
        | FieldType::Many2oneReference { .. }
        | FieldType::RowVersion
        | FieldType::DateRange { .. }
        | FieldType::DatetimeRange { .. } => None,
    }
}

fn native_cell(cell: &Cell) -> Option<&'static str> {
    match cell {
        Cell::BigInt(_) => Some("bigint"),
        Cell::Uuid(_) => Some("uuid"),
        Cell::Xid8(_) => Some("xid8"),
        Cell::TimestampTz(_) => Some("timestamptz"),
        Cell::Bytes(_) => Some("binary"),
        Cell::Null
        | Cell::Bool(_)
        | Cell::Int(_)
        | Cell::Id(_)
        | Cell::Float(_)
        | Cell::Decimal(_)
        | Cell::Str(_)
        | Cell::Date(_)
        | Cell::DateTime(_)
        | Cell::Json(_)
        | Cell::Ids(_)
        | Cell::DateRange(_)
        | Cell::DatetimeRange(_) => None,
    }
}

fn field_message(records: &Records, field: &str, kind: &str) -> String {
    format!(
        "field '{}.{field}' ({kind}) is not supported across frozen WASM ABI1; requires a future ABI major with typed native fields",
        records.model_name()
    )
}

fn limit() -> Error {
    Error::new(
        ErrorKind::ResourceExhausted,
        "WASM ABI1 field preflight exceeds its record-map, field-name or recursion admission limit",
    )
}

struct Budget {
    maps: usize,
    fields: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            maps: MAX_RECORD_MAPS,
            fields: MAX_FIELD_MENTIONS,
        }
    }
    fn map(&mut self, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(limit());
        }
        self.maps = self.maps.checked_sub(1).ok_or_else(limit)?;
        Ok(())
    }
    fn name(&mut self, name: &str) -> Result<()> {
        if name.len() > erp_registry::ident::MAX_IDENTIFIER_LEN {
            return Err(limit());
        }
        self.fields = self.fields.checked_sub(1).ok_or_else(limit)?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Document<'a> {
    Raw(&'a RawVals),
    Json(&'a serde_json::Map<String, serde_json::Value>),
    Names(&'a [String]),
}
struct Pending<'a> {
    records: Records,
    operation: Operation,
    document: Document<'a>,
    depth: usize,
}
fn enqueue<'a>(
    pending: &mut VecDeque<Pending<'a>>,
    budget: &Budget,
    next: Pending<'a>,
) -> Result<()> {
    if next.depth > MAX_DEPTH || pending.len() >= budget.maps {
        return Err(limit());
    }
    pending.push_back(next);
    Ok(())
}

struct Frame {
    records: Records,
    operation: Operation,
    names: Vec<String>,
}

/// An already detected refusal. Authorization still runs before exposing its
/// diagnostic. A plan never represents permission to continue an operation.
pub(crate) struct RefusalPlan {
    frames: Vec<Frame>,
    message: String,
}

struct Group {
    records: Records,
    operation: Operation,
    ids: Vec<RecId>,
    seen_ids: FxHashSet<RecId>,
    names: Vec<String>,
    seen_names: FxHashSet<String>,
}
impl Group {
    fn name(&mut self, name: &str) {
        if self.seen_names.insert(name.to_owned()) {
            self.names.push(name.to_owned());
        }
    }
}

impl RefusalPlan {
    pub(crate) async fn reject(self, tx: &mut Transaction) -> Error {
        let mut groups: Vec<Group> = Vec::new();
        // False positive: `Env`'s `Hash`/`Eq` are hand-written over the principal, context and
        // registry identity only; the interior-mutable statement cache is never hashed or compared.
        #[allow(clippy::mutable_key_type)]
        let mut positions: FxHashMap<(Env, ModelId, Operation), usize> = FxHashMap::default();
        for frame in &self.frames {
            let key = (
                frame.records.env().clone(),
                frame.records.model_id(),
                frame.operation,
            );
            let index = match positions.get(&key) {
                Some(index) => *index,
                None => {
                    let index = groups.len();
                    let mut group = Group {
                        records: frame.records.clone(),
                        operation: frame.operation,
                        ids: Vec::new(),
                        seen_ids: FxHashSet::default(),
                        names: Vec::new(),
                        seen_names: FxHashSet::default(),
                    };
                    if frame.operation == Operation::Create {
                        for name in frame.records.env().context().default_field_names() {
                            group.name(name);
                        }
                    }
                    groups.push(group);
                    positions.insert(key, index);
                    index
                }
            };
            let group = &mut groups[index];
            if frame.operation != Operation::Create {
                for id in frame.records.rec_ids() {
                    if group.seen_ids.insert(*id) {
                        group.ids.push(*id);
                    }
                }
            }
            for name in &frame.names {
                if name != "id" {
                    group.name(name);
                }
            }
        }
        // Model/record/field checks for every touched group precede both unknown
        // field and native-kind diagnostics. Record predicates are batched.
        for group in groups {
            let records = group.records.browse_ids(group.ids);
            if let Err(error) = records.check_access(tx, group.operation).await {
                return error;
            }
            let field_op = if group.operation == Operation::Read {
                FieldOp::Read
            } else {
                FieldOp::Write
            };
            if let Err(error) = tx.check_field_names(
                records.env(),
                records.model_id(),
                group.names.iter().map(String::as_str),
                field_op,
            ) {
                return error;
            }
        }
        for frame in self.frames {
            for name in &frame.names {
                if name != "id" && frame.records.spec().field(name).is_none() {
                    return Error::programming(format!(
                        "Invalid field '{name}' in '{}'",
                        frame.records.model_name()
                    ));
                }
            }
        }
        native_error(self.message)
    }
}

fn child_records(
    tx: &Transaction,
    parent: &Pending<'_>,
    field: &erp_registry::FieldSpec,
) -> Result<Option<Records>> {
    let Some(comodel) = field.ty.comodel().filter(|_| field.ty.is_x2many()) else {
        return Ok(None);
    };
    let registry = parent.records.env().registry();
    let spec = registry
        .model(comodel)
        .ok_or_else(|| Error::programming("WASM ABI1 command comodel is missing"))?;
    let env = if parent.operation == Operation::Create {
        parent
            .records
            .env()
            .with_context_replaced(parent.records.env().context().without_defaults())
    } else {
        parent.records.env().clone()
    };
    let env = tx.command_env(&env, parent.records.model_id(), field.id, spec);
    Ok(Some(env.records(comodel)?))
}

fn raw_tag(root: &RawVals, budget: &mut Budget, depth: usize) -> Result<Option<&'static str>> {
    let mut pending = vec![(root, depth)];
    while let Some((values, depth)) = pending.pop() {
        budget.map(depth)?;
        for (name, cell) in values.iter() {
            budget.name(name)?;
            if let Some(kind) = native_cell(cell) {
                return Ok(Some(kind));
            }
        }
        for (name, commands) in values.iter_commands() {
            budget.name(name)?;
            for command in commands {
                match command {
                    Command::Create(values) | Command::Update(_, values) => {
                        let next = depth.checked_add(1).ok_or_else(limit)?;
                        if next > MAX_DEPTH || pending.len() >= budget.maps {
                            return Err(limit());
                        }
                        pending.push((values, next));
                    }
                    Command::Delete(_)
                    | Command::Unlink(_)
                    | Command::Link(_)
                    | Command::Clear
                    | Command::Set(_) => {}
                }
            }
        }
    }
    Ok(None)
}

fn inspect(tx: &Transaction, roots: Vec<Pending<'_>>) -> Result<Option<RefusalPlan>> {
    let mut pending: VecDeque<_> = roots.into();
    let mut frames = Vec::new();
    let mut message = None;
    let mut budget = Budget::new();
    // False positive: see `RefusalPlan::reject`; `Env` hashes by identity-defining fields only.
    #[allow(clippy::mutable_key_type)]
    let mut defaults_seen = FxHashSet::default();
    while let Some(current) = pending.pop_front() {
        budget.map(current.depth)?;
        let names: Vec<&str> = match current.document {
            Document::Raw(values) => values
                .iter()
                .map(|(name, _)| name)
                .chain(values.iter_commands().map(|(name, _)| name))
                .take(budget.fields.saturating_add(1))
                .collect(),
            Document::Json(values) => values
                .keys()
                .map(String::as_str)
                .take(budget.fields.saturating_add(1))
                .collect(),
            Document::Names(names) => names
                .iter()
                .map(String::as_str)
                .take(budget.fields.saturating_add(1))
                .collect(),
        };
        let spec = current.records.spec();
        for name in &names {
            budget.name(name)?;
            if message.is_none()
                && let Some(kind) = spec.field(name).and_then(|field| native_type(&field.ty))
            {
                message = Some(field_message(&current.records, name, kind));
            }
        }
        if current.operation == Operation::Create
            && message.is_none()
            && defaults_seen.insert((current.records.env().clone(), current.records.model_id()))
        {
            for name in current.records.env().context().default_field_names() {
                budget.name(name)?;
                if let Some(kind) = spec.field(name).and_then(|field| native_type(&field.ty)) {
                    message = Some(field_message(&current.records, name, kind));
                    break;
                }
            }
        }
        let depth = current.depth.checked_add(1).ok_or_else(limit)?;
        match current.document {
            Document::Raw(values) => {
                if message.is_none() {
                    for (_, cell) in values.iter() {
                        if let Some(kind) = native_cell(cell) {
                            message = Some(format!(
                                "native {kind} values are not supported across frozen WASM ABI1; requires a future ABI major with typed native fields"
                            ));
                            break;
                        }
                    }
                }
                for (name, commands) in values.iter_commands() {
                    let child = match spec.field(name) {
                        Some(field) => child_records(tx, &current, field)?,
                        None => None,
                    };
                    for command in commands {
                        let (operation, ids, values) = match command {
                            Command::Create(values) => (Operation::Create, None, values),
                            Command::Update(id, values) => (Operation::Write, Some(*id), values),
                            Command::Delete(_)
                            | Command::Unlink(_)
                            | Command::Link(_)
                            | Command::Clear
                            | Command::Set(_) => continue,
                        };
                        match &child {
                            Some(child) => enqueue(
                                &mut pending,
                                &budget,
                                Pending {
                                    records: child.browse_ids(ids),
                                    operation,
                                    document: Document::Raw(values),
                                    depth,
                                },
                            )?,
                            None if message.is_none() => {
                                if let Some(kind) = raw_tag(values, &mut budget, depth)? {
                                    message = Some(format!(
                                        "native {kind} values are not supported across frozen WASM ABI1; requires a future ABI major with typed native fields"
                                    ));
                                }
                            }
                            None => {}
                        }
                    }
                }
            }
            Document::Json(values) => {
                for (name, value) in values {
                    let Some(field) = spec.field(name) else {
                        continue;
                    };
                    let Some(child) = child_records(tx, &current, field)? else {
                        continue;
                    };
                    let Some(commands) = value.as_array() else {
                        continue;
                    };
                    for command in commands {
                        if let Some(values) = command.as_object() {
                            enqueue(
                                &mut pending,
                                &budget,
                                Pending {
                                    records: child.clone(),
                                    operation: Operation::Create,
                                    document: Document::Json(values),
                                    depth,
                                },
                            )?;
                            continue;
                        }
                        let Some(triple) = command.as_array().filter(|items| items.len() == 3)
                        else {
                            continue;
                        };
                        let Some(values) = triple[2].as_object() else {
                            continue;
                        };
                        let (operation, ids) = match triple[0].as_i64() {
                            Some(0) => (Operation::Create, None),
                            Some(1) => match triple[1].as_i64().and_then(RecId::from_real) {
                                Some(id) => (Operation::Write, Some(id)),
                                None => continue,
                            },
                            _ => continue,
                        };
                        enqueue(
                            &mut pending,
                            &budget,
                            Pending {
                                records: child.browse_ids(ids),
                                operation,
                                document: Document::Json(values),
                                depth,
                            },
                        )?;
                    }
                }
            }
            Document::Names(_) => {}
        }
        if pending.len() > budget.maps {
            return Err(limit());
        }
        frames.push(Frame {
            records: current.records,
            operation: current.operation,
            names: names.into_iter().map(str::to_owned).collect(),
        });
    }
    Ok(message.map(|message| RefusalPlan { frames, message }))
}

/// Refuse a field without a form in this ABI among `names`, by name. For values a WebAssembly
/// implementation answered (`default_get`, `copy_data`): the caller of the method already passed
/// its access checks, so nothing is disclosed by naming the field here.
///
/// # Errors
/// The typed native refusal naming the first such field.
pub(crate) fn refuse_native_names<'a>(
    records: &Records,
    names: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let spec = records.spec();
    for name in names {
        if let Some(kind) = spec.field(name).and_then(|field| native_type(&field.ty)) {
            return Err(native_error(field_message(records, name, kind)));
        }
    }
    Ok(())
}

/// One root of a field preflight on `records`.
fn root<'a>(records: &Records, operation: Operation, document: Document<'a>) -> Pending<'a> {
    Pending {
        records: records.clone(),
        operation,
        document,
        depth: 0,
    }
}

/// The field preflight of a typed call that is about to cross into a WebAssembly layer: the
/// values of `create` and `write`, and since WASM.WIRE-NATIVE.CORE-CODECS the `default` of `copy`
/// and `copy_data` (fields of the copies: a create) and the rows, `update` and `accumulate` of
/// `upsert` (rows are created, the named fields written). Other methods carry no field values.
pub(crate) fn typed_plan(
    tx: &Transaction,
    records: &Records,
    method: &str,
    args: &(dyn Any + Send),
) -> Result<Option<RefusalPlan>> {
    if method == "create"
        && args
            .downcast_ref::<(Vec<RawVals>,)>()
            .is_some_and(|(rows,)| rows.len() > crate::wire::MAX_WIRE_BATCH)
    {
        return Err(limit());
    }
    match method {
        "create" => match args.downcast_ref::<(Vec<RawVals>,)>() {
            Some((rows,)) => inspect(
                tx,
                rows.iter()
                    .map(|row| root(records, Operation::Create, Document::Raw(row)))
                    .collect(),
            ),
            None => Ok(None),
        },
        "write" => match args.downcast_ref::<(RawVals,)>() {
            Some((row,)) => inspect(
                tx,
                vec![root(records, Operation::Write, Document::Raw(row))],
            ),
            None => Ok(None),
        },
        "copy" | "copy_data" => match args.downcast_ref::<(Option<ValueMap>,)>() {
            Some((Some(default),)) => {
                let names: Vec<String> = default.keys().cloned().collect();
                inspect(
                    tx,
                    vec![root(records, Operation::Create, Document::Names(&names))],
                )
            }
            Some((None,)) | None => Ok(None),
        },
        "upsert" => match args.downcast_ref::<(UpsertCall,)>() {
            Some((call,)) => {
                if call.rows().len() > crate::wire::MAX_WIRE_BATCH {
                    return Err(limit());
                }
                let names: Vec<String> = call
                    .update()
                    .chain(call.accumulate().map(|(field, _)| field))
                    .map(str::to_owned)
                    .collect();
                let mut roots: Vec<Pending<'_>> = call
                    .rows()
                    .iter()
                    .map(|row| root(records, Operation::Create, Document::Raw(row)))
                    .collect();
                if !names.is_empty() {
                    roots.push(root(records, Operation::Write, Document::Names(&names)));
                }
                inspect(tx, roots)
            }
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

/// The field preflight of a call in the wire form (a package's call or its `super()`), for the
/// methods of [`typed_plan`]. A document that does not have the method's shape is left to the
/// codec, which refuses it by name.
pub(crate) fn json_plan(
    tx: &Transaction,
    records: &Records,
    method: &str,
    args: &str,
) -> Result<Option<RefusalPlan>> {
    if !matches!(method, "create" | "write" | "copy" | "copy_data" | "upsert") {
        return Ok(None);
    }
    if args.len() > MAX_PACKET_BYTES {
        return Err(limit());
    }
    let document: serde_json::Value = serde_json::from_str(args).map_err(|error| {
        Error::validation(format!(
            "arguments of '{method}' do not match its wire form: {error}"
        ))
    })?;
    let Some(tuple) = document.as_array().filter(|items| items.len() == 1) else {
        return Ok(None);
    };
    // the fields an upsert names outside its rows; declared before the roots that borrow them
    let mut names: Vec<String> = Vec::new();
    let mut roots: Vec<Pending<'_>> = Vec::new();
    match method {
        "create" => {
            let Some(rows) = tuple[0].as_array() else {
                return Ok(None);
            };
            if rows.len() > crate::wire::MAX_WIRE_BATCH {
                return Err(limit());
            }
            roots.extend(
                rows.iter()
                    .filter_map(serde_json::Value::as_object)
                    .map(|row| root(records, Operation::Create, Document::Json(row))),
            );
        }
        "write" => {
            let Some(row) = tuple[0].as_object() else {
                return Ok(None);
            };
            roots.push(root(records, Operation::Write, Document::Json(row)));
        }
        "copy" | "copy_data" => {
            // `null` is "no default": nothing to check
            let Some(default) = tuple[0].as_object() else {
                return Ok(None);
            };
            roots.push(root(records, Operation::Create, Document::Json(default)));
        }
        _ => {
            // upsert: `{"key", "rows", "update", "accumulate"}` (`wire.rs`)
            let Some(call) = tuple[0].as_object() else {
                return Ok(None);
            };
            if let Some(rows) = call.get("rows").and_then(serde_json::Value::as_array) {
                if rows.len() > crate::wire::MAX_WIRE_BATCH {
                    return Err(limit());
                }
                roots.extend(
                    rows.iter()
                        .filter_map(serde_json::Value::as_object)
                        .map(|row| root(records, Operation::Create, Document::Json(row))),
                );
            }
            let update = call
                .get("update")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str);
            let accumulate = call
                .get("accumulate")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|pair| pair.as_array()?.first()?.as_str());
            names.extend(update.chain(accumulate).map(str::to_owned));
        }
    }
    if !names.is_empty() {
        roots.push(root(records, Operation::Write, Document::Names(&names)));
    }
    if roots.is_empty() {
        return Ok(None);
    }
    inspect(tx, roots)
}

/// Admit explicit ABI1 field names before decoding their values. Native fields
/// refuse after the existing model/record/field ACL, including NULL/empty packets.
///
/// # Errors
/// Existing access errors, admission limits, or a typed native Unsupported cause.
#[doc(hidden)]
pub async fn wasm_abi1_check_fields(
    tx: &mut Transaction,
    records: &Records,
    operation: Operation,
    names: &[String],
) -> Result<()> {
    if let Some(plan) = inspect(
        tx,
        vec![Pending {
            records: records.clone(),
            operation,
            document: Document::Names(names),
            depth: 0,
        }],
    )? {
        return Err(plan.reject(tx).await);
    }
    Ok(())
}

/// Refuse unsupported native fields in guest-origin CRUD JSON, even when the
/// target method chain contains only native implementations. Grants no access.
///
/// # Errors
/// Existing record, JSON admission and access errors precede typed native refusal.
#[doc(hidden)]
pub async fn wasm_abi1_check_call(
    tx: &mut Transaction,
    records: &Records,
    method: &str,
    args_json: &str,
) -> Result<()> {
    tx.admit_records(records).await?;
    if let Some(plan) = json_plan(tx, records, method, args_json)? {
        return Err(plan.reject(tx).await);
    }
    Ok(())
}

#[cfg(test)]
mod native_fields_tests {
    use super::*;

    #[test]
    fn native_fields_refusal_kind_uses_typed_cause_and_preserves_access_denial() {
        let ordinary =
            Error::programming("native uuid values are not supported across frozen WASM ABI1");
        assert!(!is_wasm_abi1_native_refusal(&ordinary));
        let native = native_error("native UUID refusal".to_owned());
        assert!(is_wasm_abi1_native_refusal(&native));
        let denied = Error::access("denied").with_source(native);
        assert!(!is_wasm_abi1_native_refusal(&denied));
    }
}
