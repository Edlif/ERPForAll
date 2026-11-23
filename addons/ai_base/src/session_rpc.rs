// SPDX-License-Identifier: MIT OR Apache-2.0
//! The session view RPC surface (2, D1.3, D2.4, D9.5; to 004, 011,
//! 012): the six owner-only methods of `ai.session` that the client panel calls —
//! `search_sessions`, `resume_session`, `fork_session`, `archive_session`, `session_delete`,
//! `session_export`. Every statement is scoped to the caller's own sessions, so a missing or
//! foreign session answers the same concealed access error and existence is never revealed
//! (D1.2). Summaries carry identifiers only — the title, state, last activity and the anchor as
//! identifiers — never event contents. The export re-reads tool results under the owner's
//! current rights and replaces model-written text whose sources are no longer readable (D2.4).
//! The owner's deletion path stays with AI1.SESSION-RETENTION (D9.2); no provider call happens
//! here.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use erp_sdk::{
    FieldOp, ValueMap, access::Operation, domain::Value as DomainValue, prelude::*,
    settings::ParameterDecl,
};
use serde_json::json;

use crate::session::AiSession;

/// Summaries one `search_sessions` call returns (L1: a declared value with a floor and a bound,
/// never a literal; the slice names it in its report).
pub const SESSION_LIST_LIMIT: &str = "ai_base.session_list_limit";

fn parameters() -> Result<Vec<ParameterDecl>> {
    Ok(vec![ParameterDecl::integer(
        SESSION_LIST_LIMIT,
        "Assistant: sessions per list",
        200,
        (1, 2000),
    )?])
}
erp_sdk::system_parameters!(parameters);

/// The concealed access error for a session the caller does not own or that does not exist; the
/// same wording either way, so existence is never revealed (2). It matches the
/// store's wording exactly.
fn concealed() -> Error {
    Error::access(
        "Your access rights do not let you read 'Assistant Session' (ai.session) records.",
    )
}

/// The caller as an ordinary, unimpersonated user: the session view never runs for
/// an elevated or system principal.
fn require_user(tx: &Transaction) -> Result<i64> {
    if !crate::ordinary_user(&tx.env()) {
        return Err(Error::access("An interactive session is required."));
    }
    Ok(tx.env().user_id().get())
}

/// The ids of each model the caller can still read: one `search` per model over the union of the
/// wanted ids (the batched read.4). Any failure — an unknown model, a refused
/// search — reads as nothing readable, the fail-safe side of the check.
async fn readable_sets(
    tx: &mut Transaction,
    wanted: &[(String, Vec<i64>)],
) -> HashMap<String, HashSet<i64>> {
    let mut per_model: HashMap<String, BTreeSet<i64>> = HashMap::new();
    for (model, ids) in wanted {
        per_model.entry(model.clone()).or_default().extend(ids);
    }
    let mut out = HashMap::new();
    for (model, ids) in per_model {
        let set = readable(tx, &model, &ids).await;
        out.insert(model, set);
    }
    out
}

/// `ids` as the caller can still read them, through one ordinary `search` (record rules, company
/// policy and the active test all apply).
async fn readable(tx: &mut Transaction, model: &str, ids: &BTreeSet<i64>) -> HashSet<i64> {
    let Ok(records) = tx.env().records(model) else {
        return HashSet::new();
    };
    let domain = match Domain::condition(
        "id",
        Operator::In,
        ids.iter()
            .copied()
            .map(DomainValue::Int)
            .collect::<Vec<_>>(),
    ) {
        Ok(domain) => domain,
        Err(_) => return HashSet::new(),
    };
    match records.search(tx, &domain).await {
        Ok(found) => found.ids().into_iter().collect(),
        Err(_) => HashSet::new(),
    }
}

/// The references of one source-bearing event, read from the `source_refs` column (the uniform
/// shape the store derives;.4). `Records` covers an ungrouped `tool_result` and one
/// entry of a title's sources, `Grouped` a grouped `tool_result`, `Context` a `context_loaded`.
enum Refs {
    Records {
        model: String,
        ids: Vec<i64>,
        fields: Vec<String>,
    },
    Grouped {
        model: String,
        group_bys: Vec<String>,
    },
    Context {
        records: Vec<(String, Vec<i64>, Vec<String>)>,
    },
}

fn string_list(value: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    value
        .get(key)?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn id_list(value: &serde_json::Value, key: &str) -> Option<Vec<i64>> {
    value
        .get(key)?
        .as_array()?
        .iter()
        .map(serde_json::Value::as_i64)
        .collect()
}

/// Parse the stored references; `None` for a shape this build does not know, which the display
/// path treats as unreadable (fail-safe, never shown).
fn parse_refs(value: &serde_json::Value) -> Option<Refs> {
    let object = value.as_object()?;
    if object.contains_key("records") {
        let mut records = Vec::new();
        for record in value.get("records")?.as_array()? {
            records.push((
                record.get("model")?.as_str()?.to_owned(),
                id_list(record, "ids")?,
                string_list(record, "field_names")?,
            ));
        }
        Some(Refs::Context { records })
    } else if object.contains_key("domain") {
        Some(Refs::Grouped {
            model: value.get("model")?.as_str()?.to_owned(),
            group_bys: string_list(value, "group_bys")?,
        })
    } else {
        Some(Refs::Records {
            model: value.get("model")?.as_str()?.to_owned(),
            ids: id_list(value, "ids")?,
            fields: string_list(value, "field_names")?,
        })
    }
}

/// The fields of one source set the caller can still read ('s group mask;
/// D2.4). An unknown model or field reads as unreadable, as does a policy error (fail-safe).
async fn accessible_fields(tx: &mut Transaction, model: &str, fields: &[String]) -> Vec<String> {
    let env = tx.env().clone();
    let mut ok = Vec::with_capacity(fields.len());
    for field in fields {
        if tx
            .has_field_access(&env, model, field, FieldOp::Read)
            .await
            .unwrap_or(false)
        {
            ok.push(field.clone());
        }
    }
    ok
}

/// Whether every record and field name of `refs` is still fully readable (4). A
/// grouped result is readable while its model is readable and every group-by field is; its
/// aggregate names are not field-checked in this slice (no grouped event exists yet; the turn
/// slice records the richer shape this check will grow from).
async fn refs_fully_readable(
    refs: &Refs,
    readable: &HashMap<String, HashSet<i64>>,
    tx: &mut Transaction,
) -> Result<bool> {
    match refs {
        Refs::Records { model, ids, fields } => {
            let set = readable
                .get(model)
                .map_or_else(HashSet::new, HashSet::clone);
            if !ids.iter().all(|id| set.contains(id)) {
                return Ok(false);
            }
            Ok(accessible_fields(tx, model, fields).await.len() == fields.len())
        }
        Refs::Grouped { model, group_bys } => {
            let Ok(records) = tx.env().records(model) else {
                return Ok(false);
            };
            if !records
                .has_access(tx, Operation::Read)
                .await
                .unwrap_or(false)
            {
                return Ok(false);
            }
            Ok(accessible_fields(tx, model, group_bys).await.len() == group_bys.len())
        }
        Refs::Context { records } => {
            for (model, ids, fields) in records {
                let set = readable
                    .get(model)
                    .map_or_else(HashSet::new, HashSet::clone);
                if !ids.iter().all(|id| set.contains(id)) {
                    return Ok(false);
                }
                if accessible_fields(tx, model, fields).await.len() != fields.len() {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// Whether a title's stored source sets are all still fully readable (4); a shape
/// this build cannot check reads as hidden (fail-safe).
async fn title_hidden_for(
    refs: &[Refs],
    readable: &HashMap<String, HashSet<i64>>,
    tx: &mut Transaction,
) -> bool {
    for refs in refs {
        if !refs_fully_readable(refs, readable, tx)
            .await
            .unwrap_or(true)
        {
            return true;
        }
    }
    false
}

/// Whether the event kind is model-written text (3): its text depends on every
/// source-bearing event before it.
fn is_model_written(kind: &str) -> bool {
    matches!(kind, "assistant_message" | "compaction" | "memory_proposed")
}

/// Whether the event kind stores record references (1).
fn is_source_bearing(kind: &str) -> bool {
    matches!(kind, "tool_result" | "context_loaded")
}

/// The anchor as identifiers plus the verdict, never a record value (3). An action
/// anchor has no shape in the client contract and reads as `null`.
fn anchor_json(model: Option<&str>, res_id: Option<i64>, available: bool) -> serde_json::Value {
    let Some(model) = model else {
        return serde_json::Value::Null;
    };
    let Some(res_id) = res_id.filter(|res_id| *res_id > 0) else {
        return serde_json::Value::Null;
    };
    json!({"model": model, "res_id": res_id, "available": available})
}

/// The `about` filter of `search_sessions`: a model and a record id together, or nothing.
fn about_filter(about: &Option<ValueMap>) -> Result<Option<(String, i64)>> {
    let Some(about) = about else {
        return Ok(None);
    };
    let invalid = || {
        Error::validation(
            "erp.ai.about_invalid: the about filter is exactly {model: string, res_id: positive \
             integer}.",
        )
    };
    let model = match about.get("model") {
        Some(Value::Str(model)) if !model.is_empty() => model.to_string(),
        _ => return Err(invalid()),
    };
    let res_id = match about.get("res_id") {
        Some(Value::Int(res_id)) if *res_id > 0 => *res_id,
        _ => return Err(invalid()),
    };
    if about.len() != 2 {
        return Err(invalid());
    }
    Ok(Some((model, res_id)))
}

/// Escape `%`, `_` and `\` of a user search word for `ILIKE ... ESCAPE E'\\'` (the runtime SQL is
/// an E-string holding one literal backslash).
fn escape_like(search: &str) -> String {
    let mut out = String::with_capacity(search.len());
    for ch in search.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// What a stored `title_sources` column says about the title (3, D2.4): no sources
/// (typed or default titles), a list of source sets to check, or a shape this build cannot read,
/// which masks the title (fail-safe).
enum TitleMask {
    Visible,
    Check(Vec<Refs>),
    Hidden,
}

fn title_mask(sources: Option<&str>) -> TitleMask {
    let Some(sources) = sources else {
        return TitleMask::Visible;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(sources) else {
        return TitleMask::Hidden;
    };
    let Some(items) = value.as_array() else {
        return TitleMask::Hidden;
    };
    let Some(refs) = items.iter().map(parse_refs).collect::<Option<Vec<_>>>() else {
        return TitleMask::Hidden;
    };
    if !refs.iter().all(|refs| matches!(refs, Refs::Records { .. })) {
        return TitleMask::Hidden;
    }
    TitleMask::Check(refs)
}

/// The event rows of one session, in sequence order, with the stored payload and references.
struct EventRow {
    seq: i32,
    kind: String,
    occurred_at: Option<DateTime>,
    payload: serde_json::Value,
    source_refs: Option<serde_json::Value>,
}

async fn load_events(tx: &mut Transaction, session: i64) -> Result<Vec<EventRow>> {
    let rows = tx
        .query(
            "SELECT seq, kind::text, occurred_at, payload::text, source_refs::text \
             FROM ai_session_event WHERE session_id = $1 ORDER BY seq",
            &[&session],
        )
        .await?;
    let mut events = Vec::with_capacity(rows.len());
    for row in &rows {
        let payload: Option<String> = row.get(3);
        let source_refs: Option<String> = row.get(4);
        events.push(EventRow {
            seq: row.get(0),
            kind: row.get(1),
            occurred_at: row.get(2),
            payload: payload
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .unwrap_or(None)
                .unwrap_or(serde_json::Value::Null),
            source_refs: source_refs
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .unwrap_or(None),
        });
    }
    Ok(events)
}

/// The re-read of one ungrouped `tool_result` (4, D9.5): the
/// stored header without its id and field lists — the export never re-sends the references of
/// records the caller can no longer read — plus the rows the caller can still read with their
/// current values, and whether the fingerprint over the readable rows' current write dates
/// differs from the stored one.
async fn re_read_tool_result(
    tx: &mut Transaction,
    payload: &serde_json::Value,
    refs: &Refs,
    readable: &HashMap<String, HashSet<i64>>,
) -> Result<serde_json::Value> {
    let Refs::Records {
        model,
        ids: stored,
        fields,
    } = refs
    else {
        return Ok(payload.clone());
    };
    let set = readable
        .get(model)
        .map_or_else(HashSet::new, HashSet::clone);
    let mut kept: Vec<i64> = stored
        .iter()
        .copied()
        .filter(|id| set.contains(id))
        .collect();
    kept.sort_unstable();
    let fields = accessible_fields(tx, model, fields).await;
    let header = |rows: Vec<serde_json::Value>, changed_since: bool| {
        json!({
            "tool": payload.get("tool").cloned().unwrap_or(serde_json::Value::Null),
            "call_id": payload.get("call_id").cloned().unwrap_or(serde_json::Value::Null),
            "model": payload.get("model").cloned().unwrap_or(serde_json::Value::Null),
            "row_count": payload.get("row_count").cloned().unwrap_or(serde_json::Value::Null),
            "truncated": payload.get("truncated").cloned().unwrap_or(serde_json::Value::Null),
            "fingerprint": payload.get("fingerprint").cloned().unwrap_or(serde_json::Value::Null),
            "rows": rows,
            "changed_since": changed_since,
        })
    };
    if kept.is_empty() {
        return Ok(header(Vec::new(), false));
    }
    let Ok(records) = tx.env().records(model) else {
        return Ok(header(Vec::new(), false));
    };
    let recordset = match records.browse(kept.clone()) {
        Ok(recordset) => recordset,
        Err(_) => return Ok(header(Vec::new(), false)),
    };
    let mut fields_with_date: Vec<&str> = fields.iter().map(String::as_str).collect();
    fields_with_date.push("write_date");
    let current = match recordset.read(tx, &fields_with_date).await {
        Ok(current) => current,
        // A model without log access reports `write_date` as an invalid field: re-read the
        // fields alone and do not compare against the stored fingerprint.
        Err(_) => {
            let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
            let current = recordset.read(tx, &fields).await?;
            return Ok(header(current_rows(current), false));
        }
    };
    // The fingerprint is over ids and write dates, never over values (2); it
    // changes exactly when a covered record's `write_date` changed.
    let mut write_dates = Vec::with_capacity(current.len());
    for row in &current {
        match row.get("write_date") {
            Some(Value::DateTime(date)) => write_dates.push(*date),
            _ => write_dates.clear(),
        }
    }
    let mut names: Vec<&str> = fields.iter().map(String::as_str).collect();
    names.sort_unstable();
    let changed_since = write_dates.len() == current.len()
        && crate::session::fingerprint(model, &kept, &write_dates, &names)
            .ok()
            .is_some_and(|now| {
                payload
                    .get("fingerprint")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|stored| stored != now)
            });
    Ok(header(current_rows(current), changed_since))
}

/// A `read` row map as export rows: `{id, values}` with `write_date` dropped and unprintable
/// content withheld, never guessed.
fn current_rows(current: Vec<BTreeMap<String, Value>>) -> Vec<serde_json::Value> {
    current
        .into_iter()
        .filter_map(|row| {
            let id = match row.get("id") {
                Some(Value::Int(id)) => *id,
                _ => return None,
            };
            let mut values = serde_json::Map::new();
            for (key, value) in &row {
                if key == "id" || key == "write_date" {
                    continue;
                }
                if let Some(value) = export_value(value) {
                    values.insert(key.clone(), value);
                }
            }
            Some(json!({"id": id, "values": values}))
        })
        .collect()
}

/// A read cell as export JSON; unprintable content (binary, ranges) is withheld, never guessed.
fn export_value(value: &Value) -> Option<serde_json::Value> {
    Some(match value {
        Value::False => serde_json::Value::Null,
        Value::Bool(value) => json!(value),
        Value::Int(value) => json!(value),
        Value::Float(value) => json!(value),
        Value::Decimal(value) => json!(value.to_string()),
        Value::Str(value) => json!(value.as_ref()),
        Value::Bytes(_) => return None,
        Value::Date(value) => json!(value.to_string()),
        Value::DateTime(value) => json!(value.to_string()),
        Value::Uuid(value) => json!(value.to_string()),
        Value::BigInt(value) => json!(value.to_string()),
        Value::Xid8(value) => json!(value.to_string()),
        Value::TimestampTz(value) => json!(value.to_string()),
        Value::Json(value) => value.as_ref().clone(),
        Value::List(items) => json!(items.iter().filter_map(export_value).collect::<Vec<_>>()),
        Value::Map(entries) => {
            let mut object = serde_json::Map::new();
            for (key, value) in entries.iter() {
                if let Some(value) = export_value(value) {
                    object.insert(key.clone(), value);
                }
            }
            serde_json::Value::Object(object)
        }
        Value::Range(_) | Value::Row(_) | Value::Rows(_) => return None,
        _ => return None,
    })
}

/// The hidden marker.4 /.
const HIDDEN: &str = "hidden: based on records you can no longer read";

#[methods(AiSession, addon = "ai_base")]
impl AiSession {
    /// List and search the caller's own sessions, ordered by last activity (2):
    /// summaries of identifiers only; a hidden title is masked and never matches (D2.4).
    #[method]
    #[rpc(effect = read, model)]
    async fn search_sessions(
        _this: &RecordSet<AiSession>,
        tx: &mut Transaction,
        search: String,
        include_archived: bool,
        about: Option<ValueMap>,
    ) -> Result<Value> {
        let uid = require_user(tx)?;
        let about = about_filter(&about)?;
        let limit = i64::try_from(crate::catalog::limit(tx, SESSION_LIST_LIMIT).await?)
            .map_err(|_| Error::validation("Invalid assistant session list limit."))?;
        let pattern = escape_like(search.trim());
        let searching = !pattern.is_empty();
        let rows = tx
            .query(
                "SELECT s.id::int8, s.title::text, s.state::text, s.last_event_at, \
                 s.anchor_model::text, s.anchor_res_id, s.title_sources::text, \
                 COALESCE(s.title ILIKE '%' || $3 || '%' ESCAPE E'\\\\', false) AS title_matched, \
                 EXISTS (SELECT 1 FROM ai_session_event e WHERE e.session_id = s.id \
                 AND e.kind = 'user_message' \
                 AND e.user_text ILIKE '%' || $3 || '%' ESCAPE E'\\\\') AS text_matched \
                 FROM ai_session s \
                 WHERE s.owner_user_id = $1 AND ($2 OR s.state = 'open') \
                 AND (($3 = '') OR s.title ILIKE '%' || $3 || '%' ESCAPE E'\\\\' OR EXISTS (\
                 SELECT 1 FROM ai_session_event e WHERE e.session_id = s.id \
                 AND e.kind = 'user_message' \
                 AND e.user_text ILIKE '%' || $3 || '%' ESCAPE E'\\\\')) \
                 AND ($4::text IS NULL OR (s.anchor_model = $4 AND s.anchor_res_id = $5)) \
                 ORDER BY s.last_event_at DESC, s.id DESC LIMIT $6",
                &[
                    &uid,
                    &include_archived,
                    &pattern,
                    &about.as_ref().map(|(model, _)| model.clone()),
                    &about.as_ref().map(|(_, res_id)| *res_id),
                    &limit,
                ],
            )
            .await?;
        // One batched readability check over every anchor and every title's sources.
        let mut wanted: Vec<(String, Vec<i64>)> = Vec::new();
        let mut title_masks = Vec::with_capacity(rows.len());
        for row in &rows {
            let sources: Option<String> = row.get(6);
            let mask = title_mask(sources.as_deref());
            if let TitleMask::Check(refs) = &mask {
                for refs in refs {
                    if let Refs::Records { model, ids, .. } = refs {
                        wanted.push((model.clone(), ids.clone()));
                    }
                }
            }
            title_masks.push(mask);
            let anchor_model: Option<String> = row.get(4);
            let anchor_res_id: Option<i64> = row.get(5);
            if let (Some(model), Some(res_id)) = (&anchor_model, anchor_res_id) {
                wanted.push((model.clone(), vec![res_id]));
            }
        }
        let readable = readable_sets(tx, &wanted).await;
        let mut summaries = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let id: i64 = row.get(0);
            let title: Option<String> = row.get(1);
            let state: String = row.get(2);
            let last_event_at: Option<DateTime> = row.get(3);
            let anchor_model: Option<String> = row.get(4);
            let anchor_res_id: Option<i64> = row.get(5);
            let title_matched: bool = row.get(7);
            let text_matched: bool = row.get(8);
            let title_hidden = match &title_masks[index] {
                TitleMask::Visible => false,
                TitleMask::Hidden => true,
                TitleMask::Check(refs) => title_hidden_for(refs, &readable, tx).await,
            };
            // A hidden title never matches the search (4); the caller's own message
            // text is always readable and keeps the session in the results.
            if searching && title_matched && title_hidden && !text_matched {
                continue;
            }
            let available =
                anchor_model
                    .as_deref()
                    .zip(anchor_res_id)
                    .is_some_and(|(model, res_id)| {
                        res_id > 0 && readable.get(model).is_some_and(|set| set.contains(&res_id))
                    });
            summaries.push(json!({
                "id": id,
                "title": if title_hidden { String::new() } else { title.unwrap_or_default() },
                "state": state,
                "last_event_at": last_event_at.map(|date| date.to_string()),
                "anchor": anchor_json(anchor_model.as_deref(), anchor_res_id, available),
            }));
        }
        Ok(Value::Json(std::sync::Arc::new(serde_json::Value::Array(
            summaries,
        ))))
    }

    /// Resume a session: the sequence the next turn continues and the anchor as identifiers,
    /// never a value (2, D1.3).
    #[method]
    #[rpc(effect = read, model)]
    async fn resume_session(
        _this: &RecordSet<AiSession>,
        tx: &mut Transaction,
        id: i64,
    ) -> Result<Value> {
        let uid = require_user(tx)?;
        let rows = tx
            .query(
                "SELECT last_seq, anchor_model::text, anchor_res_id FROM ai_session \
                 WHERE id = $1 AND owner_user_id = $2",
                &[&id, &uid],
            )
            .await?;
        let Some(row) = rows.first() else {
            return Err(concealed());
        };
        let last_seq: i32 = row.get(0);
        let anchor_model: Option<String> = row.get(1);
        let anchor_res_id: Option<i64> = row.get(2);
        let available = match (&anchor_model, anchor_res_id) {
            (Some(model), Some(res_id)) if res_id > 0 => {
                readable(tx, model, &BTreeSet::from([res_id]))
                    .await
                    .contains(&res_id)
            }
            _ => false,
        };
        let anchor = anchor_json(anchor_model.as_deref(), anchor_res_id, available);
        Ok(Value::Json(std::sync::Arc::new(json!({
            "id": id,
            "last_seq": last_seq,
            "anchor": anchor,
        }))))
    }

    /// Fork a session from its latest summary: the fork point is the last `compaction` event, or
    /// the whole log when none exists (5).
    #[method]
    #[rpc(effect = write, model)]
    async fn fork_session(
        _this: &RecordSet<AiSession>,
        tx: &mut Transaction,
        id: i64,
    ) -> Result<Value> {
        let uid = require_user(tx)?;
        let rows = tx
            .query(
                "SELECT last_seq FROM ai_session WHERE id = $1 AND owner_user_id = $2",
                &[&id, &uid],
            )
            .await?;
        let Some(row) = rows.first() else {
            return Err(concealed());
        };
        let last_seq: i32 = row.get(0);
        let summaries = tx
            .query(
                "SELECT COALESCE(max(seq), 0)::int4 FROM ai_session_event \
                 WHERE session_id = $1 AND kind = 'compaction'",
                &[&id],
            )
            .await?;
        let summary_seq: i32 = summaries[0].get(0);
        let at = if summary_seq > 0 {
            summary_seq
        } else {
            last_seq
        };
        let forked = crate::session::fork(tx, id, at).await?;
        Ok(Value::Json(std::sync::Arc::new(json!({"id": forked}))))
    }

    /// Archive a session or return it to open; it stays listed and readable (1,
    #[method]
    #[rpc(effect = write, model)]
    async fn archive_session(
        _this: &RecordSet<AiSession>,
        tx: &mut Transaction,
        id: i64,
        state: String,
    ) -> Result<Value> {
        let uid = require_user(tx)?;
        if !matches!(state.as_str(), "open" | "archived") {
            return Err(Error::validation(format!(
                "erp.ai.state_invalid: the assistant session state must be open or archived \
                 (got '{state}')."
            )));
        }
        let updated = tx
            .query(
                "UPDATE ai_session SET state = $2, write_date = (now() AT TIME ZONE 'UTC') \
                 WHERE id = $1 AND owner_user_id = $3 RETURNING id::int8",
                &[&id, &state, &uid],
            )
            .await?;
        if updated.is_empty() {
            return Err(concealed());
        }
        Ok(Value::Json(std::sync::Arc::new(
            json!({"id": id, "state": state}),
        )))
    }

    /// Delete one of the caller's own sessions (2). The deletion path itself — the
    /// SECURITY DEFINER purge — arrives with AI1.SESSION-RETENTION; until then the owner's call
    /// answers the named refusal and a foreign session the concealed access error.
    #[method]
    #[rpc(effect = destructive, model)]
    async fn session_delete(
        _this: &RecordSet<AiSession>,
        tx: &mut Transaction,
        id: i64,
    ) -> Result<Value> {
        let uid = require_user(tx)?;
        let rows = tx
            .query(
                "SELECT 1 FROM ai_session WHERE id = $1 AND owner_user_id = $2",
                &[&id, &uid],
            )
            .await?;
        if rows.is_empty() {
            return Err(concealed());
        }
        Err(Error::user(
            "erp.ai.session_delete_pending: deleting a session arrives with AI1.SESSION-RETENTION; the session stays until that slice lands.",
        ))
    }

    /// Export one of the caller's own sessions as JSON (5): events in sequence order,
    /// tool results re-read under the caller's current rights, model-written text whose sources
    /// are no longer readable replaced by the hidden marker (D2.4); a gapped sequence hides all
    /// model-written text.
    #[method]
    #[rpc(effect = read, model)]
    async fn session_export(
        _this: &RecordSet<AiSession>,
        tx: &mut Transaction,
        id: i64,
    ) -> Result<Value> {
        let uid = require_user(tx)?;
        let rows = tx
            .query(
                "SELECT title::text, state::text, company_id::int8, anchor_model::text, \
                 anchor_res_id, title_sources::text, created_at, last_event_at \
                 FROM ai_session WHERE id = $1 AND owner_user_id = $2",
                &[&id, &uid],
            )
            .await?;
        let Some(row) = rows.first() else {
            return Err(concealed());
        };
        let title: Option<String> = row.get(0);
        let state: String = row.get(1);
        let company: i64 = row.get(2);
        let anchor_model: Option<String> = row.get(3);
        let anchor_res_id: Option<i64> = row.get(4);
        let stored_sources: Option<String> = row.get(5);
        let created_at: Option<DateTime> = row.get(6);
        let last_event_at: Option<DateTime> = row.get(7);
        let events = load_events(tx, id).await?;

        // A sequence that does not start at 1 or has a gap hides every model-written text (D2.4).
        let mask_all = events
            .iter()
            .enumerate()
            .any(|(index, event)| event.seq != index as i32 + 1);

        // One batched readability check over every source the export shows or masks on.
        let mut wanted: Vec<(String, Vec<i64>)> = Vec::new();
        let sources_mask = title_mask(stored_sources.as_deref());
        if let TitleMask::Check(refs) = &sources_mask {
            for refs in refs {
                if let Refs::Records { model, ids, .. } = refs {
                    wanted.push((model.clone(), ids.clone()));
                }
            }
        }
        if let (Some(model), Some(res_id)) = (&anchor_model, anchor_res_id) {
            wanted.push((model.clone(), vec![res_id]));
        }
        for event in &events {
            let Some(refs) = event.source_refs.as_ref().and_then(parse_refs) else {
                continue;
            };
            match &refs {
                Refs::Records { model, ids, .. } => wanted.push((model.clone(), ids.clone())),
                Refs::Context { records } => {
                    for (model, ids, _) in records {
                        wanted.push((model.clone(), ids.clone()));
                    }
                }
                Refs::Grouped { .. } => {}
            }
        }
        let readable = readable_sets(tx, &wanted).await;

        // The first source-bearing event that is no longer fully readable (D2.4).
        let mut boundary: Option<i32> = None;
        for event in &events {
            if !is_source_bearing(&event.kind) {
                continue;
            }
            let fully = match event.source_refs.as_ref().and_then(parse_refs) {
                Some(refs) => refs_fully_readable(&refs, &readable, tx)
                    .await
                    .unwrap_or(false),
                None => false,
            };
            if !fully {
                boundary = Some(event.seq);
                break;
            }
        }
        let title_hidden = match &sources_mask {
            TitleMask::Visible => false,
            TitleMask::Hidden => true,
            TitleMask::Check(refs) => title_hidden_for(refs, &readable, tx).await,
        };

        let mut rendered = Vec::with_capacity(events.len());
        for event in &events {
            let hidden_after_boundary = boundary.is_some_and(|boundary| event.seq > boundary);
            let base = json!({
                "seq": event.seq,
                "kind": event.kind,
                "occurred_at": event.occurred_at.map(|date| date.to_string()),
            });
            if is_model_written(&event.kind) && (mask_all || hidden_after_boundary) {
                let mut hidden = base;
                hidden["hidden"] = json!(HIDDEN);
                rendered.push(hidden);
                continue;
            }
            match (
                event.kind.as_str(),
                event.source_refs.as_ref().and_then(parse_refs),
            ) {
                ("tool_result", Some(refs @ Refs::Records { .. })) => {
                    let mut entry = base;
                    entry["payload"] =
                        re_read_tool_result(tx, &event.payload, &refs, &readable).await?;
                    rendered.push(entry);
                }
                ("context_loaded", Some(refs)) => {
                    let mut entry = base;
                    entry["payload"] =
                        context_loaded_filtered(&event.payload, &refs, &readable, tx).await?;
                    rendered.push(entry);
                }
                _ => {
                    let mut entry = base;
                    entry["payload"] = event.payload.clone();
                    rendered.push(entry);
                }
            }
        }

        let available =
            anchor_model
                .as_deref()
                .zip(anchor_res_id)
                .is_some_and(|(model, res_id)| {
                    res_id > 0 && readable.get(model).is_some_and(|set| set.contains(&res_id))
                });
        let doc = json!({
            "session": {
                "id": id,
                "title": if title_hidden { String::new() } else { title.unwrap_or_default() },
                "state": state,
                "company_id": company,
                "anchor": anchor_json(anchor_model.as_deref(), anchor_res_id, available),
                "created_at": created_at.map(|date| date.to_string()),
                "last_event_at": last_event_at.map(|date| date.to_string()),
            },
            "events": rendered,
        });
        let content = serde_json::to_string(&doc).map_err(|error| {
            Error::internal(format!("Could not serialize the session export: {error}"))
        })?;
        Ok(Value::Json(std::sync::Arc::new(json!({
            "format": "json",
            "filename": format!("assistant-session-{id}.json"),
            "content": content,
        }))))
    }
}

/// The `context_loaded` payload with each record's ids and field names restricted to what the
/// caller can still read (4); the anchor stays, identifiers only.
async fn context_loaded_filtered(
    payload: &serde_json::Value,
    refs: &Refs,
    readable: &HashMap<String, HashSet<i64>>,
    tx: &mut Transaction,
) -> Result<serde_json::Value> {
    let Refs::Context { records } = refs else {
        return Ok(payload.clone());
    };
    let mut filtered = Vec::with_capacity(records.len());
    for (model, stored, fields) in records {
        let set = readable
            .get(model)
            .map_or_else(HashSet::new, HashSet::clone);
        let kept: Vec<i64> = stored
            .iter()
            .copied()
            .filter(|id| set.contains(id))
            .collect();
        let kept_fields = accessible_fields(tx, model, fields).await;
        filtered.push(json!({
            "model": model,
            "ids": kept,
            "field_names": kept_fields,
        }));
    }
    Ok(json!({
        "records": filtered,
        "anchor": payload.get("anchor").cloned().unwrap_or(serde_json::Value::Null),
    }))
}
