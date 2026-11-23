// SPDX-License-Identifier: MIT OR Apache-2.0
//! Compaction and titles inside turns (5, D4.6): the one in-turn summarize call,
//! the compaction event over the covered range, the never-rewritten log, the no-provider-call-
//! outside-a-turn refusal, and the caps and refusals ('s
//! route half). Every "provider" below is scripted — a recording test double — never a live
//! provider call.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use erp_addon_ai_base::compact::{
    COMPACTION_THRESHOLD_EVENTS, CompactOutcome, InTurn, ProviderReply, Summarizer, Task,
    TitleOutcome, TurnCaps, compact_in_turn, title_in_turn,
};
use erp_addon_ai_base::config::Credentials;
use erp_addon_ai_base::routing::resolve_task;
use erp_addon_ai_base::session::{EVENT_BYTES, NewEvent, NewSession, append, open_session};
use erp_sdk::{BoxFuture, ErrorKind, Result, Uuid};
use erp_testkit::Harness;
use serde_json::json;
use std::sync::Mutex;

const CONCEALED: &str =
    "Your access rights do not let you read 'Assistant Session' (ai.session) records.";

const INTERACTIVE: &str = "An interactive session is required.";

const TURN_REQUIRED: &str =
    "erp.ai.turn_required: assistant summaries and titles run inside a user turn only.";

const TURN_ID: &str = "6f25f1ba-1eb2-4a19-8b16-5e07ea1a1c93";

fn turn() -> InTurn {
    InTurn::begin(Uuid::parse(TURN_ID).unwrap())
}

async fn harness() -> Harness {
    Harness::builder([&erp_addon_base::ADDON, &erp_addon_ai_base::ADDON])
        .build()
        .await
        .unwrap()
}

async fn seed_company(h: &Harness, name: &str) -> i64 {
    let mut tx = h.begin().await.unwrap();
    let currencies = tx.env().records("res.currency").unwrap();
    let currency_id = match currencies
        .search(&mut tx, &erp_sdk::Domain::constant(true))
        .await
        .unwrap()
        .ids()
        .first()
    {
        Some(id) => *id,
        None => currencies
            .create(
                &mut tx,
                vec![
                    erp_sdk::RawVals::new()
                        .set_cell("name", erp_sdk::Cell::Str("USD".into()))
                        .set_cell("symbol", erp_sdk::Cell::Str("$".into())),
                ],
            )
            .await
            .unwrap()
            .ids()[0],
    };
    let company = tx
        .env()
        .records("res.company")
        .unwrap()
        .create(
            &mut tx,
            vec![
                erp_sdk::RawVals::new()
                    .set_cell("name", erp_sdk::Cell::Str(name.into()))
                    .set_cell("currency_id", erp_sdk::Cell::Id(currency_id)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    tx.commit().await.unwrap();
    drop(tx);
    company
}

/// A user with a company membership, the membership table the store's admission reads.
async fn seed_user(h: &Harness, uid: i64, company: i64) {
    let mut tx = h.begin().await.unwrap();
    tx.execute(
        "INSERT INTO res_users (id, name, login, active, company_id) \
         VALUES ($1, $2, $2, true, $3)",
        &[&uid, &format!("user-{uid}"), &company],
    )
    .await
    .unwrap();
    tx.execute(
        "INSERT INTO res_company_users_rel (user_id, cid) VALUES ($1, $2)",
        &[&uid, &company],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drop(tx);
}

/// Set a system parameter to a small test value before the assertions run.
async fn set_parameter(h: &Harness, key: &str, value: i64) {
    let mut tx = h.begin().await.unwrap();
    tx.execute(
        "WITH updated AS (UPDATE ir_config_parameter SET value = $2 WHERE key = $1 RETURNING 1) \
         INSERT INTO ir_config_parameter (key, value) SELECT $1, $2 \
         WHERE NOT EXISTS (SELECT 1 FROM updated)",
        &[&key, &format!("{value}")],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drop(tx);
}

/// One synthetic event of the closed kind set; every payload below is fixture data.
fn event<'a>(kind: &'a str, payload: serde_json::Value) -> NewEvent<'a> {
    NewEvent {
        kind,
        version: 1,
        turn_id: None,
        payload,
    }
}

async fn open_with_events(
    h: &Harness,
    uid: i64,
    company: i64,
    events: &[(&str, serde_json::Value)],
) -> i64 {
    let mut tx = h.begin_as(uid).await.unwrap();
    let session = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    for (kind, payload) in events {
        append(&mut tx, session, &event(kind, payload.clone()))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    drop(tx);
    session
}

/// The scripted provider: records every task and prompt it received and answers with a fixed
/// reply and token counts. It makes no network call.
struct ScriptedProvider {
    calls: Mutex<Vec<(Task, String)>>,
    reply: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

impl ScriptedProvider {
    fn new(reply: impl Into<String>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            reply: reply.into(),
            input_tokens: Some(11),
            output_tokens: Some(2),
        }
    }

    fn calls(&self) -> Vec<(Task, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Summarizer for ScriptedProvider {
    fn call<'a>(&'a self, task: Task, prompt: &'a str) -> BoxFuture<'a, Result<ProviderReply>> {
        let prompt = prompt.to_owned();
        Box::pin(async move {
            self.calls.lock().unwrap().push((task, prompt));
            Ok(ProviderReply {
                text: self.reply.clone(),
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
            })
        })
    }
}

/// The synthetic `tool_result` fixture used throughout (references, never record values).
fn tool_result() -> serde_json::Value {
    json!({
        "tool": "search_records",
        "call_id": "call-1",
        "model": "res.partner",
        "ids": [11],
        "field_names": ["name"],
        "row_count": 1,
        "truncated": false,
        "fingerprint": "aa"
    })
}

/// event naming the covered range, and the earlier events unchanged — the log is never
/// rewritten. The budget half of the case is the turn loop's reservation settle, wired with
/// AI1.ASSIST-TURN, which folds in the usage this outcome returns)
#[tokio::test]
async fn spec_ais_019_one_summarize_call_names_the_covered_range_and_never_rewrites_the_log() {
    let h = harness().await;
    let company = seed_company(&h, "Compact Sessions").await;
    seed_user(&h, 7, company).await;
    set_parameter(&h, COMPACTION_THRESHOLD_EVENTS, 3).await;
    let session = open_with_events(
        &h,
        7,
        company,
        &[
            ("user_message", json!({"text": "Who is customer 11?"})),
            ("tool_result", tool_result()),
            ("assistant_message", json!({"text": "Customer 11 is Acme."})),
        ],
    )
    .await;
    let provider = ScriptedProvider::new("Looked up Acme (res.partner 11) by name.");
    let caps = TurnCaps {
        round_trips_left: 2,
    };
    let outcome = {
        let mut tx = h.begin_as(7).await.unwrap();
        let outcome = compact_in_turn(&mut tx, session, Some(&turn()), &caps, &provider)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        outcome
    };
    assert_eq!(
        outcome,
        CompactOutcome::Compacted {
            from_seq: 1,
            to_seq: 3,
            input_tokens: Some(11),
            output_tokens: Some(2)
        }
    );
    let calls = provider.calls();
    assert_eq!(calls.len(), 1, "exactly one summarize call: {calls:?}");
    assert_eq!(calls[0].0, Task::Summarize);
    for text in [
        "Who is customer 11?",
        "search_records",
        "res.partner",
        "Customer 11 is Acme.",
    ] {
        assert!(
            calls[0].1.contains(text),
            "the prompt misses {text:?}: {}",
            calls[0].1
        );
    }

    // The compaction event names the covered range and the summary, with the turn id; the
    // earlier events are exactly as appended.
    let mut tx = h.begin_as(7).await.unwrap();
    let rows = tx
        .query(
            "SELECT seq, kind::text, payload::text, turn_id::text FROM ai_session_event \
             WHERE session_id = $1 ORDER BY seq",
            &[&session],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 4, "one compaction event appended, nothing else");
    assert_eq!(rows[0].get::<_, String>(1), "user_message");
    assert_eq!(rows[1].get::<_, String>(1), "tool_result");
    assert_eq!(rows[2].get::<_, String>(1), "assistant_message");
    assert_eq!(rows[3].get::<_, String>(1), "compaction");
    let compaction: serde_json::Value = serde_json::from_str(&rows[3].get::<_, String>(2)).unwrap();
    assert_eq!(
        compaction,
        json!({"from_seq": 1, "to_seq": 3, "summary": "Looked up Acme (res.partner 11) by name."})
    );
    assert_eq!(rows[3].get::<_, String>(3), TURN_ID);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[0].get::<_, String>(2)).unwrap(),
        json!({"text": "Who is customer 11?"})
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[1].get::<_, String>(2)).unwrap(),
        tool_result()
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[2].get::<_, String>(2)).unwrap(),
        json!({"text": "Customer 11 is Acme."})
    );
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn spec_ais_020_no_provider_call_outside_a_turn() {
    let h = harness().await;
    let company = seed_company(&h, "No Background Summaries").await;
    seed_user(&h, 7, company).await;
    set_parameter(&h, COMPACTION_THRESHOLD_EVENTS, 3).await;
    let session = open_with_events(
        &h,
        7,
        company,
        &[
            ("user_message", json!({"text": "First question."})),
            ("tool_result", tool_result()),
            ("assistant_message", json!({"text": "First answer."})),
        ],
    )
    .await;
    let provider = ScriptedProvider::new("A summary");
    let caps = TurnCaps {
        round_trips_left: 1,
    };
    let mut tx = h.begin_as(7).await.unwrap();
    let error = compact_in_turn(&mut tx, session, None, &caps, &provider)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(error.message(), TURN_REQUIRED);
    let error = title_in_turn(&mut tx, session, None, &caps, &provider)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(error.message(), TURN_REQUIRED);
    tx.rollback().await.unwrap();
    drop(tx);
    assert!(
        provider.calls().is_empty(),
        "no provider call outside a turn"
    );
    let mut tx = h.begin_as(7).await.unwrap();
    let rows = tx
        .query(
            "SELECT count(*)::int8 FROM ai_session_event WHERE session_id = $1 \
             AND kind = 'compaction'",
            &[&session],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get::<_, i64>(0), 0);
    let title = tx
        .query(
            "SELECT title::text FROM ai_session WHERE id = $1",
            &[&session],
        )
        .await
        .unwrap();
    assert!(
        title[0].get::<_, Option<String>>(0).is_none(),
        "no title was set outside a turn"
    );
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

/// and the work moves to the next turn as `Pending`; the fifth-round-trip count and the
/// pipeline-split halves of the case ride the turn loop, AI1.ASSIST-TURN)
#[tokio::test]
async fn the_threshold_caps_and_refusals() {
    let h = harness().await;
    let company = seed_company(&h, "Compact Refusals").await;
    seed_user(&h, 7, company).await;
    seed_user(&h, 8, company).await;
    set_parameter(&h, COMPACTION_THRESHOLD_EVENTS, 3).await;
    let session = open_with_events(
        &h,
        7,
        company,
        &[
            ("user_message", json!({"text": "First question."})),
            ("assistant_message", json!({"text": "First answer."})),
        ],
    )
    .await;
    let provider = ScriptedProvider::new("A summary");
    let caps = TurnCaps {
        round_trips_left: 1,
    };

    // Below the threshold: nothing is called or written.
    let mut tx = h.begin_as(7).await.unwrap();
    assert_eq!(
        compact_in_turn(&mut tx, session, Some(&turn()), &caps, &provider)
            .await
            .unwrap(),
        CompactOutcome::NotNeeded
    );
    tx.rollback().await.unwrap();
    drop(tx);
    assert!(provider.calls().is_empty());

    // Past the threshold but with no round trip left: deferred, nothing called or written.
    let mut tx = h.begin_as(7).await.unwrap();
    append(
        &mut tx,
        session,
        &event("assistant_message", json!({"text": "Third."})),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    let mut tx = h.begin_as(7).await.unwrap();
    assert_eq!(
        compact_in_turn(
            &mut tx,
            session,
            Some(&turn()),
            &TurnCaps {
                round_trips_left: 0
            },
            &provider
        )
        .await
        .unwrap(),
        CompactOutcome::Pending
    );
    tx.rollback().await.unwrap();
    drop(tx);
    assert!(provider.calls().is_empty());

    // A foreign session answers the concealed refusal and makes no call.
    let mut tx = h.begin_as(8).await.unwrap();
    let error = compact_in_turn(&mut tx, session, Some(&turn()), &caps, &provider)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access);
    assert_eq!(error.message(), CONCEALED);
    tx.rollback().await.unwrap();
    drop(tx);
    assert!(provider.calls().is_empty());

    // A non-interactive caller is refused before anything else.
    let mut tx = h.begin().await.unwrap();
    let error = compact_in_turn(&mut tx, session, Some(&turn()), &caps, &provider)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access);
    assert_eq!(error.message(), INTERACTIVE);
    tx.rollback().await.unwrap();
    drop(tx);

    // An empty reply is refused after the one call, and nothing is appended.
    let empty = ScriptedProvider::new("   ");
    let mut tx = h.begin_as(7).await.unwrap();
    let error = compact_in_turn(&mut tx, session, Some(&turn()), &caps, &empty)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(
        error.message(),
        "erp.ai.summary_empty: the summarize route returned no summary."
    );
    tx.rollback().await.unwrap();
    drop(tx);
    assert_eq!(empty.calls().len(), 1);

    // A summary that does not fit the event payload cap is refused; nothing was appended.
    set_parameter(&h, EVENT_BYTES, 200).await;
    let long = ScriptedProvider::new("x".repeat(500));
    let mut tx = h.begin_as(7).await.unwrap();
    let error = compact_in_turn(&mut tx, session, Some(&turn()), &caps, &long)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(
        error.message(),
        "erp.ai.summary_too_large: the summary exceeds 200 payload bytes; nothing was appended."
    );
    tx.rollback().await.unwrap();
    drop(tx);

    // No compaction event was ever appended through the refusals.
    let mut tx = h.begin_as(7).await.unwrap();
    let rows = tx
        .query(
            "SELECT count(*)::int8 FROM ai_session_event WHERE session_id = $1 \
             AND kind = 'compaction'",
            &[&session],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get::<_, i64>(0), 0);
    tx.rollback().await.unwrap();
    drop(tx);

    // A stored threshold outside the declaration is refused by name (L1), never a fallback.
    set_parameter(&h, COMPACTION_THRESHOLD_EVENTS, 1).await;
    let mut tx = h.begin_as(7).await.unwrap();
    let error = compact_in_turn(&mut tx, session, Some(&turn()), &caps, &provider)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Stored parameter ai_base.compaction_threshold_events is invalid."
    );
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

/// Titles run on the same in-turn path: one title call, the stored title and its source set, a
/// titled session left alone, and the empty and oversized refusals (6).
#[tokio::test]
async fn titles_run_in_turn_once_and_are_refused_by_name() {
    let h = harness().await;
    let company = seed_company(&h, "Titled Sessions").await;
    seed_user(&h, 7, company).await;
    let session = open_with_events(
        &h,
        7,
        company,
        &[
            ("user_message", json!({"text": "List my customers."})),
            ("tool_result", tool_result()),
            (
                "assistant_message",
                json!({"text": "You have one customer."}),
            ),
        ],
    )
    .await;
    let provider = ScriptedProvider::new("Customer list");
    let caps = TurnCaps {
        round_trips_left: 1,
    };
    let outcome = {
        let mut tx = h.begin_as(7).await.unwrap();
        let outcome = title_in_turn(&mut tx, session, Some(&turn()), &caps, &provider)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        outcome
    };
    assert_eq!(
        outcome,
        TitleOutcome::Titled {
            title: "Customer list".to_string(),
            input_tokens: Some(11),
            output_tokens: Some(2)
        }
    );
    let calls = provider.calls();
    assert_eq!(calls.len(), 1, "exactly one title call: {calls:?}");
    assert_eq!(calls[0].0, Task::Title);
    assert!(
        calls[0].1.contains("List my customers.") && calls[0].1.contains("You have one customer."),
        "the prompt covers the opening exchange: {}",
        calls[0].1
    );

    // The title and the covered source-bearing events' references are stored; a second call
    // leaves them alone.
    let mut tx = h.begin_as(7).await.unwrap();
    let rows = tx
        .query(
            "SELECT title::text, title_sources::text FROM ai_session WHERE id = $1",
            &[&session],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get::<_, String>(0), "Customer list");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[0].get::<_, String>(1)).unwrap(),
        json!([{"model": "res.partner", "ids": [11], "field_names": ["name"]}])
    );
    tx.rollback().await.unwrap();
    drop(tx);
    let mut tx = h.begin_as(7).await.unwrap();
    assert_eq!(
        title_in_turn(&mut tx, session, Some(&turn()), &caps, &provider)
            .await
            .unwrap(),
        TitleOutcome::NotNeeded
    );
    tx.rollback().await.unwrap();
    drop(tx);
    assert_eq!(provider.calls().len(), 1, "a titled session is left alone");

    // A session without an answer yet gets no title call.
    let unanswered = open_with_events(
        &h,
        7,
        company,
        &[("user_message", json!({"text": "Question only."}))],
    )
    .await;
    let other = ScriptedProvider::new("Title");
    let mut tx = h.begin_as(7).await.unwrap();
    assert_eq!(
        title_in_turn(&mut tx, unanswered, Some(&turn()), &caps, &other)
            .await
            .unwrap(),
        TitleOutcome::NotNeeded
    );
    tx.rollback().await.unwrap();
    drop(tx);
    assert!(other.calls().is_empty());

    // An empty and an oversized title are refused.
    let answered = open_with_events(
        &h,
        7,
        company,
        &[
            ("user_message", json!({"text": "A question."})),
            ("assistant_message", json!({"text": "An answer."})),
        ],
    )
    .await;
    let empty = ScriptedProvider::new(" ");
    let mut tx = h.begin_as(7).await.unwrap();
    let error = title_in_turn(&mut tx, answered, Some(&turn()), &caps, &empty)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(
        error.message(),
        "erp.ai.title_empty: the title route returned no title."
    );
    tx.rollback().await.unwrap();
    drop(tx);
    let long = ScriptedProvider::new("t".repeat(1025));
    let mut tx = h.begin_as(7).await.unwrap();
    let error = title_in_turn(&mut tx, answered, Some(&turn()), &caps, &long)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(
        error.message(),
        "erp.ai.title_too_large: the title exceeds the session title length of 1024 characters."
    );
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

/// resolve_task half; the kept-answer half of the case rides the turn loop's call site)
#[tokio::test]
async fn spec_ais_043_partial_a_missing_task_route_is_named_and_never_falls_back() {
    let h = harness().await;
    let company = seed_company(&h, "Route Kinds").await;
    let mut tx = h.begin().await.unwrap();
    tx.execute(
        "INSERT INTO ai_provider (name, family, base_url, allowed_models, model_limit, active) \
         VALUES ('local', 'openai_compatible', 'http://127.0.0.1:11434/v1', \
         '[\"local-model\"]'::text::jsonb, 128, true)",
        &[],
    )
    .await
    .unwrap();
    for kind in ["assistant", "summarize"] {
        tx.execute(
            "INSERT INTO ai_route (name, company_id, task_kind, provider_id, model) \
             SELECT $2, $1, $2, id, 'local-model' FROM ai_provider WHERE name = 'local'",
            &[&company, &kind],
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    drop(tx);

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fixture.key"), "synthetic-ai-fixture").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        dir.path().join("fixture.key"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let table: toml::Value = toml::from_str("[provider.local]\nkey_file='fixture.key'\n").unwrap();
    let credentials = Credentials::load(Some(&table), dir.path()).unwrap();

    let mut tx = h.begin().await.unwrap();
    let env = tx.env().with_company(company).unwrap();
    // No title route: named, and no other route is tried.
    let error = resolve_task(&env, &mut tx, &credentials, "title")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        format!("erp.ai.route_missing: task 'title' company {company}")
    );
    let selected = resolve_task(&env, &mut tx, &credentials, "summarize")
        .await
        .unwrap();
    assert_eq!(selected.provider(), "local");
    assert_eq!(selected.model(), "local-model");
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}
