// SPDX-License-Identifier: MIT OR Apache-2.0
//! Compaction and titles inside user turns (5, D4.6): when a session is past its
//! declared event threshold, the turn makes exactly one `summarize` provider call over the
//! covered range and appends a `compaction` event naming the range and the summary — the log is
//! never rewritten, earlier events stay as appended. A title is made the same way on the
//! `title` route, from the opening exchange up to the first answer. No provider call ever runs
//! outside a user turn: the exported functions refuse when they are not given
//! the turn the caller began ([`InTurn`]), and they defer to the next turn's start when the
//! turn has no provider round trip left, so no cap is ever raised.
//!
//! The provider exchange is injected ([`Summarizer`]) so the module is testable with a scripted
//! provider; [`RouteTransport`] is the production executor on the task's route, resolved by the
//! turn loop through [`crate::routing::resolve_task`] (a missing route answers
//! `erp.ai.route_missing` naming the task kind, and no other route is used). The
//! returned usage is folded by the caller into the turn's single reservation settle
//! (`crate::ledger`), so the summarize call is counted against the turn's budget (D4.5). The
//! call site is the assistant turn loop in `erp-cli`, behind `ai_session: Option<i64>` until
//! `AI1.ASSIST-TURN` wires the session id.

use crate::{catalog, session, transport::ProviderTransport};
use erp_sdk::{
    BoxFuture,
    ai::{ContentPart, Instruction, ToolTurnRequest, TurnMessage, TurnRole, UserText},
    prelude::*,
    settings::ParameterDecl,
};

// ---------------------------------------------------------------------------
// Declared limits (amendment L1): the threshold and the summarize request target are system
// parameters with the design's proposed numbers as defaults, a floor and a hard bound. A value
// stored outside the declaration is refused by name (the read fails), and zero never means
// unlimited.
// ---------------------------------------------------------------------------

/// Uncompacted events that trigger compaction (5).
pub const COMPACTION_THRESHOLD_EVENTS: &str = "ai_base.compaction_threshold_events";
/// Bytes of the summarize or title request built from the covered range (5, D4.6);
/// the covered range is cut at this target, the log itself is never truncated.
pub const SUMMARY_TARGET_BYTES: &str = "ai_base.summary_target_bytes";

fn parameters() -> Result<Vec<ParameterDecl>> {
    Ok(vec![
        ParameterDecl::integer(
            COMPACTION_THRESHOLD_EVENTS,
            "Assistant: uncompacted events that trigger compaction",
            400,
            (2, 100_000),
        )?,
        ParameterDecl::integer(
            SUMMARY_TARGET_BYTES,
            "Assistant: summarize request bytes",
            12288,
            (1024, 262_144),
        )?,
    ])
}
erp_sdk::system_parameters!(parameters);

/// The `ai.session.title` field length a model-written title must fit (6); a longer
/// title is refused, never truncated.
const TITLE_MAX_CHARS: usize = 1024;

// ---------------------------------------------------------------------------
// The in-turn contract (5, D4.6).
// ---------------------------------------------------------------------------

/// One of the two provider tasks that run only inside a user turn: the rolling summary
/// (`summarize` route) or the session title (`title` route).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// The rolling summary over the covered range (5).
    Summarize,
    /// The model-written session title (6).
    Title,
}

impl Task {
    /// The `ai.route` task kind the task resolves; names a missing route by it.
    pub const fn route_kind(self) -> &'static str {
        match self {
            Task::Summarize => "summarize",
            Task::Title => "title",
        }
    }
}

/// The running turn. Only the turn loop creates one (at turn start, from the turn id the event
/// log and the context cache share); the provider is reachable only through it, so no summarize
/// or title call can run outside a user turn (5, D4.6).
#[derive(Debug, Clone, Copy)]
pub struct InTurn {
    turn_id: Uuid,
}

impl InTurn {
    /// Begin the in-turn phase of the turn `turn_id` belongs to.
    pub fn begin(turn_id: Uuid) -> Self {
        Self { turn_id }
    }

    /// The turn id the appended `compaction` event records (1).
    pub fn turn_id(self) -> Uuid {
        self.turn_id
    }
}

/// The provider capacity the turn still has: when no round trip is left, the
/// compaction or the title happens at the start of the next turn instead (D4.5).
#[derive(Debug, Clone, Copy)]
pub struct TurnCaps {
    /// Provider round trips the turn may still make.
    pub round_trips_left: usize,
}

/// One completed summarize or title exchange: the model-written text and the provider-reported
/// usage. The caller settles the usage against the reservation it made for the task's route
/// (`crate::ledger`), so the call is counted against the turn's budget (5).
#[derive(Debug, Clone)]
pub struct ProviderReply {
    /// The model-written summary or title.
    pub text: String,
    /// The provider-reported input tokens, when it reported any.
    pub input_tokens: Option<i64>,
    /// The provider-reported output tokens, when it reported any.
    pub output_tokens: Option<i64>,
}

/// The summarize/title provider call, injected by the turn loop: the transport on the task's
/// route in production ([`RouteTransport`]), a scripted provider in tests. The module calls it
/// at most once per invocation, and only through an [`InTurn`] with a round trip left.
pub trait Summarizer: Send + Sync {
    /// Run one provider exchange for `task` with the built `prompt`.
    fn call<'a>(&'a self, task: Task, prompt: &'a str) -> BoxFuture<'a, Result<ProviderReply>>;
}

/// What [`compact_in_turn`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactOutcome {
    /// One summarize call ran and the `compaction` event was appended over the covered range,
    /// with the exchange's usage for the caller's settle.
    Compacted {
        /// The first sequence the summary covers.
        from_seq: i32,
        /// The last sequence the summary covers.
        to_seq: i32,
        /// The provider-reported input tokens, when reported.
        input_tokens: Option<i64>,
        /// The provider-reported output tokens, when reported.
        output_tokens: Option<i64>,
    },
    /// The session is past its threshold but the turn has no provider round trip left; the next
    /// turn starts with the compaction (5). Nothing was called or written.
    Pending,
    /// The session is not past its threshold; nothing was called or written.
    NotNeeded,
}

/// What [`title_in_turn`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleOutcome {
    /// One title call ran and the session title was set, with the exchange's usage for the
    /// caller's settle.
    Titled {
        /// The stored model-written title.
        title: String,
        /// The provider-reported input tokens, when reported.
        input_tokens: Option<i64>,
        /// The provider-reported output tokens, when reported.
        output_tokens: Option<i64>,
    },
    /// The session needs a title but the turn has no provider round trip left; the next turn
    /// makes it (6). Nothing was called or written.
    Pending,
    /// The session already has a title (typed or made earlier), or has no answer to title yet;
    /// nothing was called or written.
    NotNeeded,
}

// ---------------------------------------------------------------------------
// Store reads and the prompt build (kinds, text, references — never record values).
// ---------------------------------------------------------------------------

fn require_user(tx: &Transaction) -> Result<i64> {
    if !crate::ordinary_user(&tx.env()) {
        return Err(Error::access("An interactive session is required."));
    }
    Ok(tx.env().user_id().get())
}

/// The concealed access error for a session the caller does not own or that does not exist: the
/// same wording as the session store's, either way (2).
fn concealed() -> Error {
    Error::access(
        "Your access rights do not let you read 'Assistant Session' (ai.session) records.",
    )
}

/// The turn-required refusal (5, D4.6): the exported functions refuse
/// to call the provider when they are not given the turn the caller began.
fn turn_required() -> Error {
    Error::user("erp.ai.turn_required: assistant summaries and titles run inside a user turn only.")
}

/// The caller's own session's `last_seq`, or the concealed refusal (2).
async fn owned_last_seq(tx: &mut Transaction, session: i64, uid: i64) -> Result<i32> {
    let rows = tx
        .query(
            "SELECT last_seq FROM ai_session WHERE id = $1 AND owner_user_id = $2",
            &[&session, &uid],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(concealed());
    };
    Ok(row.get(0))
}

/// The end of the latest `compaction` event's covered range, when one exists (5).
/// The payload was validated on write, so a shape this build cannot read is a defect.
async fn last_compaction_to_seq(tx: &mut Transaction, session: i64) -> Result<Option<i32>> {
    let rows = tx
        .query(
            "SELECT payload::text FROM ai_session_event \
             WHERE session_id = $1 AND kind = 'compaction' ORDER BY seq DESC LIMIT 1",
            &[&session],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let text: String = row.get(0);
    let compaction: session::Compaction = serde_json::from_str(&text).map_err(|error| {
        Error::internal(format!(
            "The stored compaction payload cannot be read: {error}"
        ))
    })?;
    Ok(Some(compaction.to_seq))
}

/// One event of the covered range, as the store keeps it: the kind, the validated payload and
/// the derived references of a source-bearing event.
struct CoveredEvent {
    seq: i32,
    kind: String,
    payload: serde_json::Value,
    source_refs: Option<serde_json::Value>,
}

/// The events of the caller's own session in `from..=to`, in sequence order. The read is
/// bounded by the session's declared event cap.
async fn load_events(
    tx: &mut Transaction,
    session: i64,
    from: i32,
    to: i32,
) -> Result<Vec<CoveredEvent>> {
    let rows = tx
        .query(
            "SELECT seq, kind::text, payload::text, source_refs::text \
             FROM ai_session_event WHERE session_id = $1 AND seq >= $2 AND seq <= $3 ORDER BY seq",
            &[&session, &from, &to],
        )
        .await?;
    let mut events = Vec::with_capacity(rows.len());
    for row in &rows {
        let seq: i32 = row.get(0);
        let payload: Option<String> = row.get(2);
        let source_refs: Option<String> = row.get(3);
        events.push(CoveredEvent {
            seq,
            kind: row.get(1),
            payload: payload
                .as_deref()
                .map(|text| {
                    serde_json::from_str(text).map_err(|error| {
                        Error::internal(format!(
                            "The stored payload of event {seq} cannot be read: {error}"
                        ))
                    })
                })
                .transpose()?
                // The payload column is required, so a NULL here is a schema violation; the
                // line renders as no payload rather than failing the whole compaction.
                .unwrap_or(serde_json::Value::Null),
            source_refs: source_refs
                .as_deref()
                .map(|text| {
                    serde_json::from_str(text).map_err(|error| {
                        Error::internal(format!(
                            "The stored references of event {seq} cannot be read: {error}"
                        ))
                    })
                })
                .transpose()?,
        });
    }
    Ok(events)
}

/// One line of the provider prompt for one event: the kind, and the text or the references as
/// the store keeps them (5). A source-bearing event contributes its references,
/// never record values. The renderer is lenient about fields a validated payload must carry
/// (an empty string renders where the schema says the field always exists): the prompt is a
/// display of the stored log, never a rewrite of it, and the compaction event names the range
/// either way.
fn event_line(event: &CoveredEvent) -> String {
    let seq = event.seq;
    let text = |key: &str| {
        event
            .payload
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    match event.kind.as_str() {
        "user_message" => format!("[{seq}] user: {}", text("text")),
        "assistant_message" => format!("[{seq}] assistant: {}", text("text")),
        "tool_call" => format!(
            "[{seq}] tool_call {}: {}",
            text("tool"),
            event
                .payload
                .get("arguments")
                .map(serde_json::Value::to_string)
                .unwrap_or_default()
        ),
        "tool_result" => format!(
            "[{seq}] tool_result {}: {}",
            text("tool"),
            event
                .source_refs
                .as_ref()
                .map(serde_json::Value::to_string)
                .unwrap_or_default()
        ),
        "context_loaded" => format!(
            "[{seq}] context: {}",
            event
                .source_refs
                .as_ref()
                .map(serde_json::Value::to_string)
                .unwrap_or_default()
        ),
        _ => format!("[{seq}] {}: {}", event.kind, event.payload),
    }
}

/// The built provider prompt over the covered prefix of `events`, and how many events it covers.
struct BuiltPrompt {
    prompt: String,
    covered: usize,
}

/// Build the prompt from the oldest events first, cutting the covered range at the declared
/// byte target (5). The cut never touches the log: the `compaction` event names
/// exactly the range that went into the request, and the events after it stay for the next
/// compaction. One event that alone exceeds the target is refused, never truncated.
fn build_prompt(events: &[CoveredEvent], cap: usize) -> Result<BuiltPrompt> {
    let mut prompt = String::new();
    let mut covered = 0usize;
    for event in events {
        let line = event_line(event);
        let added = prompt.len() + usize::from(!prompt.is_empty()) + line.len();
        if added > cap {
            if prompt.is_empty() {
                return Err(Error::user(format!(
                    "erp.ai.summarize_range_too_large: an event of the covered range exceeds \
                     the summarize request limit of {cap} bytes."
                )));
            }
            break;
        }
        if !prompt.is_empty() {
            prompt.push('\n');
        }
        prompt.push_str(&line);
        covered += 1;
    }
    Ok(BuiltPrompt { prompt, covered })
}

// ---------------------------------------------------------------------------
// The two in-turn tasks.
// ---------------------------------------------------------------------------

/// Run one compaction inside the caller's turn when the session is past its threshold (
/// D4.5): exactly one `summarize` call, then a `compaction` event naming the
/// covered sequence range and the summary. The log is never rewritten; earlier events stay as
/// appended. The covered range starts after the latest `compaction` event (or at 1) and is cut
/// at the declared request target, so the summary always covers the oldest uncompacted events.
///
/// The provider is called only when `turn` is given and the turn still has a
/// provider round trip; otherwise the work is deferred
/// ([`CompactOutcome::Pending`]). The returned usage is for the caller's reservation settle, so
/// the call is counted against the turn's budget.
///
/// # Errors
/// `Access` for a non-interactive caller or a missing or foreign session (concealed); `User`
/// `erp.ai.turn_required` without a turn, `erp.ai.summarize_range_too_large` naming the request
/// target, `erp.ai.summary_empty` for an empty reply, `erp.ai.summary_too_large` naming the
/// event payload cap; the provider's error, or the store's refusals.
pub async fn compact_in_turn(
    tx: &mut Transaction,
    session: i64,
    turn: Option<&InTurn>,
    caps: &TurnCaps,
    summarize: &dyn Summarizer,
) -> Result<CompactOutcome> {
    let uid = require_user(tx)?;
    let last_seq = owned_last_seq(tx, session, uid).await?;
    let Some(turn) = turn else {
        return Err(turn_required());
    };
    let threshold = catalog::limit(tx, COMPACTION_THRESHOLD_EVENTS).await?;
    let from = last_compaction_to_seq(tx, session)
        .await?
        .map_or(1, |to| to.saturating_add(1));
    let count = i64::from(last_seq) - i64::from(from) + 1;
    if count
        < i64::try_from(threshold)
            .map_err(|_| Error::validation("Invalid assistant compaction threshold."))?
    {
        return Ok(CompactOutcome::NotNeeded);
    }
    if caps.round_trips_left == 0 {
        return Ok(CompactOutcome::Pending);
    }
    let events = load_events(tx, session, from, last_seq).await?;
    let target = catalog::limit(tx, SUMMARY_TARGET_BYTES).await?;
    let built = build_prompt(&events, target)?;
    let reply = summarize.call(Task::Summarize, &built.prompt).await?;
    let summary = reply.text.trim().to_string();
    if summary.is_empty() {
        return Err(Error::user(
            "erp.ai.summary_empty: the summarize route returned no summary.",
        ));
    }
    let from_seq = events[0].seq;
    let to_seq = events[built.covered - 1].seq;
    let payload = serde_json::json!({"from_seq": from_seq, "to_seq": to_seq, "summary": summary});
    let bytes = serde_json::to_vec(&payload)
        .map_err(|error| {
            Error::internal(format!("Could not serialize the compaction event: {error}"))
        })?
        .len();
    let event_cap = catalog::limit(tx, session::EVENT_BYTES).await?;
    if bytes > event_cap {
        return Err(Error::user(format!(
            "erp.ai.summary_too_large: the summary exceeds {event_cap} payload bytes; nothing was appended."
        )));
    }
    session::append(
        tx,
        session,
        &session::NewEvent {
            kind: session::EventKind::Compaction.as_str(),
            version: session::EventKind::Compaction.version(),
            turn_id: Some(turn.turn_id()),
            payload,
        },
    )
    .await?;
    Ok(CompactOutcome::Compacted {
        from_seq,
        to_seq,
        input_tokens: reply.input_tokens,
        output_tokens: reply.output_tokens,
    })
}

/// Make the session title inside the caller's turn (6), on the same in-turn path as
/// compaction: exactly one `title` call over the opening exchange up to the first answer, then
/// the title and the covered source-bearing events' references are stored on the session row
/// (`title_sources`,.3; the display path re-checks them and masks the title when
/// one is unreadable). A session that already has a title — typed or made earlier — is left
/// alone, and a session without an answer yet waits for the next turn.
///
/// # Errors
/// As [`compact_in_turn`], plus `User` `erp.ai.title_empty` for an empty reply and
/// `erp.ai.title_too_large` naming the field length.
pub async fn title_in_turn(
    tx: &mut Transaction,
    session: i64,
    turn: Option<&InTurn>,
    caps: &TurnCaps,
    summarize: &dyn Summarizer,
) -> Result<TitleOutcome> {
    let uid = require_user(tx)?;
    let rows = tx
        .query(
            "SELECT title::text FROM ai_session WHERE id = $1 AND owner_user_id = $2",
            &[&session, &uid],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(concealed());
    };
    if row.get::<_, Option<String>>(0).is_some() {
        return Ok(TitleOutcome::NotNeeded);
    }
    if turn.is_none() {
        return Err(turn_required());
    }
    if caps.round_trips_left == 0 {
        return Ok(TitleOutcome::Pending);
    }
    let answers = tx
        .query(
            "SELECT seq FROM ai_session_event WHERE session_id = $1 \
             AND kind = 'assistant_message' ORDER BY seq LIMIT 1",
            &[&session],
        )
        .await?;
    let Some(first) = answers.first() else {
        return Ok(TitleOutcome::NotNeeded);
    };
    let answer_seq: i32 = first.get(0);
    let events = load_events(tx, session, 1, answer_seq).await?;
    let target = catalog::limit(tx, SUMMARY_TARGET_BYTES).await?;
    let built = build_prompt(&events, target)?;
    let reply = summarize.call(Task::Title, &built.prompt).await?;
    let title = reply.text.trim().to_string();
    if title.is_empty() {
        return Err(Error::user(
            "erp.ai.title_empty: the title route returned no title.",
        ));
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(Error::user(format!(
            "erp.ai.title_too_large: the title exceeds the session title length of \
             {TITLE_MAX_CHARS} characters."
        )));
    }
    let sources: Vec<serde_json::Value> = events[..built.covered]
        .iter()
        // Only source-bearing events contribute sources (3); the store always
        // derives their references on append, so a missing one simply leaves the set shorter.
        .filter(|event| matches!(event.kind.as_str(), "tool_result" | "context_loaded"))
        .filter_map(|event| event.source_refs.clone())
        .collect();
    let sources = serde_json::to_string(&sources).map_err(|error| {
        Error::internal(format!("Could not serialize the title sources: {error}"))
    })?;
    tx.execute(
        "UPDATE ai_session SET title = $2, title_sources = $3::text::jsonb, \
         write_date = (now() AT TIME ZONE 'UTC') WHERE id = $1 AND owner_user_id = $4",
        &[&session, &title, &sources, &uid],
    )
    .await?;
    Ok(TitleOutcome::Titled {
        title,
        input_tokens: reply.input_tokens,
        output_tokens: reply.output_tokens,
    })
}

// ---------------------------------------------------------------------------
// The production executor on the task's route (5, D4.6).
// ---------------------------------------------------------------------------

/// The production [`Summarizer`]: one bounded provider exchange per call, with the task's
/// static instruction template and the built prompt as user text — the prompt is runtime text,
/// so it is `UserText`, never an `Instruction`. The turn loop resolves the task's
/// route with [`crate::routing::resolve_task`] and settles the returned usage against the
/// reservation it made, so the call stays inside the turn's caps and budget.
pub struct RouteTransport<'route> {
    model: String,
    transport: ProviderTransport<'route>,
}

impl<'route> RouteTransport<'route> {
    /// Bind the executor to the provider transport of the task's resolved route.
    pub fn new(
        route: &'route crate::routing::ConfiguredRoute,
        transport: ProviderTransport<'route>,
    ) -> Self {
        Self {
            model: route.model().to_owned(),
            transport,
        }
    }
}

/// The static instruction template of each task; only string literals reach the model as
/// instructions.
fn instruction(task: Task) -> &'static str {
    match task {
        Task::Summarize => {
            "Summarize this assistant conversation so later turns can continue without the \
             covered details: keep the user's facts and decisions, the records and tools \
             involved, and any open questions."
        }
        Task::Title => "Write a short title for this assistant conversation.",
    }
}

impl Summarizer for RouteTransport<'_> {
    fn call<'a>(&'a self, task: Task, prompt: &'a str) -> BoxFuture<'a, Result<ProviderReply>> {
        Box::pin(async move {
            let messages = vec![
                TurnMessage::new(
                    TurnRole::System,
                    vec![ContentPart::Instruction(Instruction::from_static(
                        instruction(task),
                    ))],
                )
                .map_err(|error| {
                    Error::validation(format!(
                        "erp.ai.message_invalid: could not prepare the {} request: {error}",
                        task.route_kind()
                    ))
                })?,
                TurnMessage::new(
                    TurnRole::User,
                    vec![ContentPart::UserText(UserText::new(prompt.to_owned()))],
                )
                .map_err(|error| {
                    Error::validation(format!(
                        "erp.ai.message_invalid: could not prepare the {} request: {error}",
                        task.route_kind()
                    ))
                })?,
            ];
            let request = ToolTurnRequest::new(self.model.as_str(), messages, Vec::new()).map_err(
                |error| {
                    Error::validation(format!(
                        "erp.ai.message_invalid: could not prepare the {} request: {error}",
                        task.route_kind()
                    ))
                },
            )?;
            let reply = self
                .transport
                .complete_turn(&request)
                .await
                .map_err(|error| Error::user(format!("erp.ai.provider_failed: {error}")))?;
            let mut text = String::new();
            for part in reply.parts() {
                if let ContentPart::AssistantText(chunk) = part {
                    text.push_str(chunk);
                }
            }
            Ok(ProviderReply {
                text,
                input_tokens: Some(i64::from(reply.usage().prompt_tokens)),
                output_tokens: Some(i64::from(reply.usage().completion_tokens)),
            })
        })
    }
}
