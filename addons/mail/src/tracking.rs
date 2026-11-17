// SPDX-License-Identifier: MIT OR Apache-2.0
//! The history of a record in its chatter: the creation line and one message per write that
//! changed tracked fields, for every model that inherits `mail.thread`, with no code in the model
//! (§4.3, §5.3; guide, "History").
//!
//! Our own implementation:
//!
//! - **Where the changes come from.** The ORM captures tracked changes at the flush (the ORM's
//!   `tracking` module); statements an addon writes itself (a state flipped in one `UPDATE`)
//!   are reported with `Transaction::track_external`, so they come out of this same path.
//! - **When the messages are written.** [`TRACKING_HOOK`], a precommit hook registered by the
//!   server's composition root, runs after the commit flush: it drains the captured changes and
//!   the records `mail.thread.create` noted, and writes all messages of the transaction in one
//!   multi-row insert. Statement count is constant in the number of records: one context read,
//!   one existence read per model with new records, one display-name read pair per comodel, one
//!   insert. Without `mail` in the registry the hook returns at once.
//! - **What a model can change** (overridable methods of the mixin, see [`crate::MailThread`]):
//!   `_creation_message` (the creation text), `_track_subtype` (the subtype of a change),
//!   `_track_body` (text added to the history message; an override may also post its own
//!   message there).
//! - **Who sees what.** The values are stored as JSON on the message (`mail.message.tracking`);
//!   when a reader loads the thread, a change of a field restricted to groups the reader is not
//!   in is left out ([`visible_changes`]).

use std::collections::{BTreeMap, BTreeSet};

use erp_sdk::prelude::*;
use erp_sdk::serde_json::{self, Map, Value as Json};
use erp_sdk::{BoxFuture, FieldType, PrecommitHook, TrackedChange, TrackedRecord};

use crate::post::escape_into;
use crate::thread_rpc::rows_of;

/// A column of a row the history statements read, decoded, or a database error naming it.
macro_rules! column {
    ($row:expr, $i:expr) => {
        $row.try_get($i)
            .map_err(|error| Error::db(format!("history statement column {}: {error}", $i)))?
    };
}

const MESSAGE: &str = "mail.message";
const THREAD: &str = "mail.thread";

/// The system parameter that bounds one stored history value, in characters: a longer text is
/// stored up to it and ends with [`CUT_MARKER`]. Unset or 0 is [`DEFAULT_VALUE_MAX_CHARS`]; read
/// like the other mail limit (`crate::activity_menu`).
pub const VALUE_MAX_CHARS_KEY: &str = "mail.tracking_value_max_chars";
/// The bound when [`VALUE_MAX_CHARS_KEY`] is unset.
pub const DEFAULT_VALUE_MAX_CHARS: i64 = 2_000;
/// The lowest value [`VALUE_MAX_CHARS_KEY`] may hold.
pub const MIN_VALUE_MAX_CHARS: i64 = 100;
/// The highest value [`VALUE_MAX_CHARS_KEY`] may hold.
pub const MAX_VALUE_MAX_CHARS: i64 = 100_000;
/// What ends a history value cut at [`VALUE_MAX_CHARS_KEY`].
pub const CUT_MARKER: char = '\u{2026}';

/// [`VALUE_MAX_CHARS_KEY`], read in superuser mode; a value outside its bounds is refused by name.
async fn value_max_chars(env: &Env, tx: &mut Transaction) -> Result<usize> {
    let value = env
        .records("ir.config_parameter")?
        .sudo(true)
        .call::<_, i64>(tx, "get_int", (VALUE_MAX_CHARS_KEY.to_owned(), None::<i64>))
        .await?;
    let value = if value == 0 {
        DEFAULT_VALUE_MAX_CHARS
    } else {
        value
    };
    match usize::try_from(value) {
        Ok(chars) if (MIN_VALUE_MAX_CHARS..=MAX_VALUE_MAX_CHARS).contains(&value) => Ok(chars),
        _ => Err(Error::validation(format!(
            "The system parameter {VALUE_MAX_CHARS_KEY} must be between {MIN_VALUE_MAX_CHARS} \
             and {MAX_VALUE_MAX_CHARS} (or 0 for the default {DEFAULT_VALUE_MAX_CHARS}), not \
             {value}."
        ))),
    }
}

/// The precommit hook that writes the history messages; the composition root registers it with
/// `OrmBuilder::precommit_hook` (`crates/erp-cli/src/human.rs`).
pub const TRACKING_HOOK: PrecommitHook = PrecommitHook::new("mail.tracking", run_hook);

fn run_hook(tx: &mut Transaction) -> BoxFuture<'_, Result<()>> {
    Box::pin(log_history(tx))
}

/// The thread records created in this transaction, by model, noted by `mail.thread.create`.
#[derive(Debug, Default)]
pub(crate) struct CreatedThreads(BTreeMap<&'static str, Vec<i64>>);

/// Note the records `create` made, for their creation line at commit.
pub(crate) fn note_created(tx: &mut Transaction, model: &'static str, ids: &[i64]) {
    if ids.is_empty() {
        return;
    }
    tx.extension::<CreatedThreads>()
        .0
        .entry(model)
        .or_default()
        .extend_from_slice(ids);
}

/// One message to write.
struct Line {
    model: &'static str,
    id: i64,
    message_type: &'static str,
    body: String,
    tracking: Option<Json>,
    subtype: Option<(i64, bool)>,
}

async fn log_history(tx: &mut Transaction) -> Result<()> {
    let registry = std::sync::Arc::clone(tx.registry());
    if registry.model(MESSAGE).is_none() {
        return Ok(());
    }
    let created = std::mem::take(&mut tx.extension::<CreatedThreads>().0);
    let changed: Vec<TrackedRecord> = tx
        .take_tracked_changes()
        .await?
        .into_iter()
        .filter(|r| {
            registry
                .model(r.model())
                .is_some_and(|m| m.inherits(THREAD))
        })
        .collect();
    if created.is_empty() && changed.is_empty() {
        return Ok(());
    }
    let env = tx.env();
    let mut lines = Vec::new();
    for (model, ids) in created {
        let existing = env
            .records(model)?
            .sudo(true)
            .browse(ids)?
            .exists(tx)
            .await?;
        if existing.is_empty() {
            continue;
        }
        let records = env.records(model)?.browse(existing.ids())?;
        let texts: BTreeMap<i64, String> = records.call(tx, "_creation_message", ()).await?;
        for id in existing.ids() {
            // an override leaves a record out to write no creation line for it
            let Some(text) = texts.get(&id) else {
                continue;
            };
            let mut body = String::new();
            escape_into(&mut body, text);
            lines.push(Line {
                model,
                id,
                message_type: "notification",
                body,
                tracking: None,
                subtype: None,
            });
        }
    }
    let max_chars = if changed.is_empty() {
        0
    } else {
        value_max_chars(&env, tx).await?
    };
    let names = display_names(tx, &env, &registry, &changed).await?;
    let mut by_model: BTreeMap<&'static str, Vec<&TrackedRecord>> = BTreeMap::new();
    for record in &changed {
        by_model.entry(record.model()).or_default().push(record);
    }
    for (model, records) in by_model {
        let spec = registry
            .model(model)
            .ok_or_else(|| Error::invariant(format!("tracked model {model} left the registry")))?;
        let fields: BTreeMap<i64, Vec<String>> = records
            .iter()
            .map(|r| {
                let names = r.changes().iter().map(|c| c.field().to_owned()).collect();
                real(r).map(|id| (id, names))
            })
            .collect::<Result<_>>()?;
        let thread = env.records(model)?.browse(fields.keys().copied())?;
        let subtypes: BTreeMap<i64, String> =
            thread.call(tx, "_track_subtype", (fields.clone(),)).await?;
        let bodies: BTreeMap<i64, String> = thread.call(tx, "_track_body", (fields,)).await?;
        let resolved = resolve_subtypes(tx, subtypes.values()).await?;
        for record in records {
            let id = real(record)?;
            let entries = record
                .changes()
                .iter()
                .map(|change| entry(spec, change, &names, max_chars))
                .collect::<Result<Vec<Json>>>()?;
            let mut body = String::new();
            escape_into(&mut body, bodies.get(&id).map_or("", String::as_str));
            lines.push(Line {
                model,
                id,
                message_type: "tracking",
                body,
                tracking: Some(Json::Array(entries)),
                subtype: subtypes.get(&id).map(|x| resolved[x]),
            });
        }
    }
    if lines.is_empty() {
        return Ok(());
    }
    // one context read: the note subtype and the acting user's partner
    let rows = tx
        .query(
            "SELECT (SELECT d.res_id::int8 FROM ir_model_data d WHERE d.module = 'mail' \
             AND d.name = 'mt_note' AND d.model = 'mail.message.subtype'), \
             (SELECT u.partner_id::int8 FROM res_users u WHERE u.id = $1::int8)",
            &[&env.uid()],
        )
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| Error::invariant("the history context statement returned no row"))?;
    let note: Option<i64> = column!(row, 0);
    let note = note.ok_or_else(|| {
        Error::invariant("the mail subtype mt_note is missing: the mail addon's data is not loaded")
    })?;
    // a process acting without a user partner (a system job) writes the history unauthored
    let author: Option<i64> = column!(row, 1);
    let vals: Vec<RawVals> = lines
        .into_iter()
        .map(|line| {
            let (subtype, internal) = line.subtype.unwrap_or((note, true));
            let mut vals = RawVals::new()
                .set_cell("model", Cell::Str(line.model.into()))
                .set_cell("res_id", Cell::Id(line.id))
                .set_cell("body", Cell::Str(line.body.into()))
                .set_cell("message_type", Cell::Str(line.message_type.into()))
                .set_cell("is_internal", Cell::Bool(internal))
                .set_cell("subtype_id", Cell::Id(subtype));
            if let Some(author) = author {
                vals = vals.set_cell("author_id", Cell::Id(author));
            }
            if let Some(tracking) = line.tracking {
                vals = vals.set_cell("tracking", Cell::Json(std::sync::Arc::new(tracking)));
            }
            vals
        })
        .collect();
    env.records(MESSAGE)?.sudo(true).create(tx, vals).await?;
    Ok(())
}

/// The id of a tracked record: only stored records are tracked.
fn real(record: &TrackedRecord) -> Result<i64> {
    record.id().as_real().map(|id| id.get()).ok_or_else(|| {
        Error::invariant(format!(
            "a tracked {} record has no stored id",
            record.model()
        ))
    })
}

/// The subtype ids and `internal` flags of the external ids `_track_subtype` named, in one read.
async fn resolve_subtypes<'a>(
    tx: &mut Transaction,
    xmlids: impl Iterator<Item = &'a String>,
) -> Result<BTreeMap<String, (i64, bool)>> {
    let wanted: Vec<String> = xmlids
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows = tx
        .query(
            "SELECT d.module || '.' || d.name, s.id::int8, COALESCE(s.internal, false) \
             FROM ir_model_data d JOIN mail_message_subtype s ON s.id = d.res_id \
             WHERE d.model = 'mail.message.subtype' AND d.module || '.' || d.name = ANY($1)",
            &[&wanted],
        )
        .await?;
    let mut found = BTreeMap::new();
    for row in &rows {
        let xmlid: String = column!(row, 0);
        let id: i64 = column!(row, 1);
        let internal: bool = column!(row, 2);
        found.insert(xmlid, (id, internal));
    }
    if let Some(missing) = wanted.iter().find(|x| !found.contains_key(*x)) {
        return Err(Error::validation(format!(
            "_track_subtype names the message subtype {missing}, which does not exist."
        )));
    }
    Ok(found)
}

/// The display names of the relational values of `changed`: one existence read and one name read
/// per comodel, archived records included (a value may point at a record archived since).
async fn display_names(
    tx: &mut Transaction,
    env: &Env,
    registry: &erp_sdk::Registry,
    changed: &[TrackedRecord],
) -> Result<BTreeMap<(String, i64), String>> {
    let mut wanted: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for record in changed {
        let Some(spec) = registry.model(record.model()) else {
            continue;
        };
        for change in record.changes() {
            let comodel = match spec.field(change.field()).map(|f| &f.ty) {
                Some(
                    FieldType::Many2one { comodel, .. } | FieldType::Many2many { comodel, .. },
                ) => comodel,
                _ => continue,
            };
            let ids = wanted.entry(comodel.clone()).or_default();
            for cell in [change.old(), change.new_value()] {
                ids.extend(cell_ids(cell));
            }
        }
    }
    let mut names = BTreeMap::new();
    for (comodel, ids) in wanted {
        if ids.is_empty() {
            continue;
        }
        let records = env
            .records(&comodel)?
            .sudo(true)
            .with_context([("active_test", Value::Bool(false))])
            .browse(ids)?
            .exists(tx)
            .await?;
        for (id, row) in rows_of(tx, &records, &["display_name"]).await? {
            let name = match row.get("display_name") {
                Some(Value::Str(name)) => name.to_string(),
                _ => String::new(),
            };
            names.insert((comodel.clone(), id), name);
        }
    }
    Ok(names)
}

fn cell_ids(cell: &Cell) -> Vec<i64> {
    match cell {
        Cell::Id(id) => vec![*id],
        Cell::Ids(ids) => ids
            .iter()
            .filter_map(|i| i.as_real())
            .map(|i| i.get())
            .collect(),
        _ => Vec::new(),
    }
}

/// The JSON object of one change: raw values plus the text
/// the chatter shows, which is left out when it is the raw text itself. Datetimes keep no display
/// text: the client shows them in the viewer's zone. `g` keeps the field's groups as they were at
/// the write (`null` for none), for the reader check once the field is gone. Texts are cut at
/// `max_chars` characters.
fn entry(
    spec: &erp_sdk::ModelSpec,
    change: &TrackedChange,
    names: &BTreeMap<(String, i64), String>,
    max_chars: usize,
) -> Result<Json> {
    let field = spec.field(change.field()).ok_or_else(|| {
        Error::invariant(format!(
            "tracked field {}.{} is not declared",
            spec.name,
            change.field()
        ))
    })?;
    let mut out = Map::new();
    out.insert("n".into(), Json::from(field.name));
    out.insert("l".into(), Json::from(field.string.as_str()));
    out.insert("t".into(), Json::from(field.ty.type_name()));
    out.insert("g".into(), field.groups.map_or(Json::Null, Json::from));
    for (raw, shown, cell) in [("o", "od", change.old()), ("v", "vd", change.new_value())] {
        let raw_value = cut(raw_json(spec, field.name, cell)?, max_chars);
        let display_value = cut(display(&field.ty, cell, names), max_chars);
        if !(display_value.is_string() && display_value == raw_value) {
            out.insert(shown.into(), display_value);
        }
        out.insert(raw.into(), raw_value);
    }
    Ok(Json::Object(out))
}

/// `value` with a text longer than `max_chars` characters cut there and ended with
/// [`CUT_MARKER`].
fn cut(value: Json, max_chars: usize) -> Json {
    match value {
        Json::String(text) => match text.char_indices().nth(max_chars) {
            Some((end, _)) => {
                let mut short = text[..end].to_owned();
                short.push(CUT_MARKER);
                Json::String(short)
            }
            None => Json::String(text),
        },
        other => other,
    }
}

fn raw_json(spec: &erp_sdk::ModelSpec, field: &str, cell: &Cell) -> Result<Json> {
    Ok(match cell {
        Cell::Null => Json::Null,
        Cell::Bool(b) => Json::from(*b),
        Cell::Int(i) => Json::from(*i),
        Cell::Id(i) => Json::from(*i),
        Cell::BigInt(i) => Json::from(i.to_string()),
        Cell::Float(f) => serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number),
        Cell::Decimal(d) => Json::from(d.to_string()),
        Cell::Str(s) => Json::from(s.as_ref()),
        Cell::Date(d) => Json::from(d.to_string()),
        Cell::DateTime(d) => Json::from(d.to_string()),
        Cell::Ids(_) => Json::from(cell_ids(cell)),
        other => {
            return Err(Error::validation(format!(
                "{}.{field} is tracked, but a {} value cannot be shown in its history.",
                spec.name,
                cell_kind(other)
            )));
        }
    })
}

fn cell_kind(cell: &Cell) -> &'static str {
    match cell {
        Cell::Json(_) => "JSON",
        Cell::Bytes(_) => "binary",
        Cell::Uuid(_) => "UUID",
        _ => "range or instant",
    }
}

fn display(ty: &FieldType, cell: &Cell, names: &BTreeMap<(String, i64), String>) -> Json {
    let text = match (ty, cell) {
        (_, Cell::Null) => return Json::Null,
        (FieldType::Datetime, _) => return Json::Null,
        (FieldType::Boolean, Cell::Bool(b)) => (if *b { "Yes" } else { "No" }).to_owned(),
        (
            FieldType::Selection {
                values: Some(values),
                ..
            },
            Cell::Str(key),
        ) => values
            .iter()
            .find(|(k, _)| k.as_str() == key.as_ref())
            .map_or_else(|| key.to_string(), |(_, label)| label.clone()),
        (FieldType::Many2one { comodel, .. } | FieldType::Many2many { comodel, .. }, _) => {
            let ids = cell_ids(cell);
            if ids.is_empty() {
                return Json::Null;
            }
            ids.iter()
                .map(|id| {
                    names
                        .get(&(comodel.clone(), *id))
                        .cloned()
                        .unwrap_or_else(|| format!("#{id}"))
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
        (_, Cell::Str(s)) => s.to_string(),
        (_, Cell::Int(i)) => i.to_string(),
        (_, Cell::Float(f)) => f.to_string(),
        (_, Cell::Decimal(d)) => d.to_string(),
        (_, Cell::Date(d)) => d.to_string(),
        (_, Cell::Bool(b)) => b.to_string(),
        _ => return Json::Null,
    };
    Json::from(text)
}

/// The changes of a stored `tracking` value that the reader of `env` may see: a change of a field
/// restricted to groups the reader is not in is left out. `None` when nothing is
/// left. A field the model no longer declares is checked against the groups stored with its
/// change (`g`); a change of such a field stored without them is left out for every reader. The
/// stored groups are not sent.
///
/// # Errors
/// `Invariant` for a stored value that is not an array of objects; the group check's errors.
pub(crate) async fn visible_changes(
    tx: &mut Transaction,
    env: &Env,
    model: &str,
    stored: &Json,
    allowed: &mut BTreeMap<(String, String), bool>,
) -> Result<Option<String>> {
    let Json::Array(entries) = stored else {
        return Err(Error::invariant(format!(
            "a stored tracking value is not an array: {stored}"
        )));
    };
    let spec = env.registry().model(model);
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry.get("n").and_then(Json::as_str).ok_or_else(|| {
            Error::invariant(format!(
                "a stored tracking change has no field name: {entry}"
            ))
        })?;
        let groups = match spec.and_then(|s| s.field(name)) {
            Some(field) => field.groups,
            None => match entry.get("g") {
                Some(Json::Null) => None,
                Some(Json::String(groups)) => Some(groups.as_str()),
                // written before the groups were stored: who may see it is unknown
                _ => continue,
            },
        };
        let visible = match groups {
            None => true,
            Some(groups) => {
                let key = (model.to_owned(), groups.to_owned());
                match allowed.get(&key) {
                    Some(v) => *v,
                    None => {
                        let v = tx.has_groups(env, groups).await?;
                        allowed.insert(key, v);
                        v
                    }
                }
            }
        };
        if visible {
            let mut entry = entry.clone();
            if let Some(object) = entry.as_object_mut() {
                object.remove("g");
            }
            kept.push(entry);
        }
    }
    if kept.is_empty() {
        return Ok(None);
    }
    Ok(Some(Json::Array(kept).to_string()))
}
