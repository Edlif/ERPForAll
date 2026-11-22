// SPDX-License-Identifier: MIT OR Apache-2.0
//! The session view RPC surface (2, D1.3, D2.4, D9.5): the owner-only listing,
//! search, resume, fork, archive, delete and export of `ai.session`, the anchor verdict and the
//! source-set masking of the export (to 004, 011, 012). All events below are
//! synthetic fixture data; no provider call happens in any of these tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use erp_addon_ai_base::session::{NewEvent, NewSession, append, open_session};
use erp_sdk::{Cell, ErrorKind, FieldOp, RawVals, Value, ValueMap};
use erp_testkit::{FieldPolicy, Harness};
use serde_json::json;

const CONCEALED: &str =
    "Your access rights do not let you read 'Assistant Session' (ai.session) records.";

const HIDDEN: &str = "hidden: based on records you can no longer read";

const DELETE_PENDING: &str = "erp.ai.session_delete_pending: deleting a session arrives with \
    AI1.SESSION-RETENTION; the session stays until that slice lands.";

async fn harness() -> Harness {
    Harness::builder([&erp_addon_base::ADDON, &erp_addon_ai_base::ADDON])
        .build()
        .await
        .unwrap()
}

async fn harness_fields() -> Harness {
    Harness::builder([&erp_addon_base::ADDON, &erp_addon_ai_base::ADDON])
        .policy(FieldPolicy::new().deny("res.partner", "email", FieldOp::Read))
        .build()
        .await
        .unwrap()
}

async fn seed_company(h: &Harness, name: &str) -> i64 {
    let mut tx = h.begin().await.unwrap();
    let currency = tx.env().records("res.currency").unwrap();
    let currency_id = match currency
        .search(&mut tx, &erp_sdk::Domain::constant(true))
        .await
        .unwrap()
        .ids()
        .first()
    {
        Some(id) => *id,
        None => currency
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("USD".into()))
                        .set_cell("symbol", Cell::Str("$".into())),
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
                RawVals::new()
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("currency_id", Cell::Id(currency_id)),
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

async fn partner(tx: &mut erp_sdk::Transaction, name: &str) -> i64 {
    tx.env()
        .records("res.partner")
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("name", Cell::Str(name.into()))],
        )
        .await
        .unwrap()
        .ids()[0]
}

fn message(text: &str) -> NewEvent<'static> {
    NewEvent {
        kind: "user_message",
        version: 1,
        turn_id: None,
        payload: json!({"text": text}),
    }
}

fn assistant(text: &str) -> NewEvent<'static> {
    NewEvent {
        kind: "assistant_message",
        version: 1,
        turn_id: None,
        payload: json!({"text": text}),
    }
}

fn tool_result(model: &str, ids: Vec<i64>, field_names: Vec<&str>) -> NewEvent<'static> {
    NewEvent {
        kind: "tool_result",
        version: 1,
        turn_id: None,
        payload: json!({
            "tool": "search_records",
            "call_id": "call-1",
            "model": model,
            "ids": ids,
            "field_names": field_names,
            "row_count": 1,
            "truncated": false,
            "fingerprint": "synthetic-stored-fingerprint",
        }),
    }
}

/// The dynamic RPC surface: a model call on the (empty) `ai.session` recordset.
async fn call<A: Send + 'static>(
    tx: &mut erp_sdk::Transaction,
    method: &'static str,
    args: A,
) -> Result<Value, erp_sdk::Error> {
    let sessions = tx.env().records("ai.session").unwrap();
    sessions.call::<A, Value>(tx, method, args).await
}

fn json_of(value: Value) -> serde_json::Value {
    match value {
        Value::Json(doc) => (*doc).clone(),
        other => panic!("expected a JSON result, got {other:?}"),
    }
}

async fn export_content(tx: &mut erp_sdk::Transaction, session: i64) -> serde_json::Value {
    let exported = call(tx, "session_export", (session,)).await.unwrap();
    let doc = json_of(exported);
    assert_eq!(doc["format"], "json");
    assert_eq!(doc["filename"], format!("assistant-session-{session}.json"));
    serde_json::from_str(doc["content"].as_str().unwrap()).unwrap()
}

/// not own the session and never lists it; the administrator is covered by the same owner-only
/// store check (the shipped rule applies to `base.group_user`, which `base.group_system`
/// implies, as the store's test asserts).
#[tokio::test]
async fn every_surface_answers_concealed_for_any_other_user() {
    let h = harness().await;
    let company = seed_company(&h, "Rpc Owner").await;
    seed_user(&h, 7, company).await;
    seed_user(&h, 8, company).await;
    let mut tx = h.begin_as(7).await.unwrap();
    let mine = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    append(&mut tx, mine, &message("hello")).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = h.begin_as(8).await.unwrap();
    // the listing never includes user 7's session
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), true, None::<ValueMap>),
    )
    .await
    .unwrap();
    assert!(
        json_of(listed).as_array().unwrap().is_empty(),
        "user 8 listed another user's sessions"
    );
    // every by-id surface refuses with the concealed access error, delete included
    let error = call(&mut tx, "resume_session", (mine,)).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(error.message(), CONCEALED);
    let error = call(&mut tx, "fork_session", (mine,)).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(error.message(), CONCEALED);
    let error = call(&mut tx, "archive_session", (mine, "archived".to_owned()))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(error.message(), CONCEALED);
    let error = call(&mut tx, "session_delete", (mine,)).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(error.message(), CONCEALED);
    let error = call(&mut tx, "session_export", (mine,)).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(error.message(), CONCEALED);
    tx.rollback().await.unwrap();
    drop(tx);

    // the owner's own call is the pending-refusal path of the delete surface: the owner is
    // recognized (no concealed error) and the deletion itself names the slice that adds it
    let mut tx = h.begin_as(7).await.unwrap();
    let error = call(&mut tx, "session_delete", (mine,)).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    assert_eq!(error.message(), DELETE_PENDING);
    // the owner still resumes their own session
    let resumed = call(&mut tx, "resume_session", (mine,)).await.unwrap();
    assert_eq!(json_of(resumed)["id"], mine);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// caller's own message text, resumed at the stored sequence, and archived — the archived one
/// leaves the open list and stays readable.
#[tokio::test]
async fn sessions_list_search_resume_and_archive() {
    let h = harness().await;
    let company = seed_company(&h, "Rpc List").await;
    seed_user(&h, 7, company).await;
    // five sessions: A "Quarterly partner review", B "Vendor pricing",
    // C "Quarterly moonbeam", D "Unrelated", E "Another"
    let mut ids = Vec::new();
    for title in [
        "Quarterly partner review",
        "Vendor pricing",
        "Quarterly moonbeam",
        "Unrelated",
        "Another",
    ] {
        let mut tx = h.begin_as(7).await.unwrap();
        let session = open_session(
            &mut tx,
            &NewSession {
                company,
                title: Some(title.into()),
                anchor_action: None,
                anchor_model: None,
                anchor_res_id: 0,
            },
        )
        .await
        .unwrap();
        ids.push(session);
        tx.commit().await.unwrap();
    }
    // messages: A gets "hello moon", C gets "moonbeam thoughts"
    let mut tx = h.begin_as(7).await.unwrap();
    append(&mut tx, ids[0], &message("hello moon"))
        .await
        .unwrap();
    append(&mut tx, ids[2], &message("moonbeam thoughts"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // pin distinct last-activity stamps (bookkeeping update) so the order is deterministic:
    // D newest, then A, E, B, C oldest
    let mut tx = h.begin_as(7).await.unwrap();
    let stamps = [
        "2026-11-04 10:00:00",
        "2026-11-02 10:00:00",
        "2026-11-01 10:00:00",
        "2026-11-05 10:00:00",
        "2026-11-03 10:00:00",
    ];
    for (index, stamp) in stamps.iter().enumerate() {
        let stamp = erp_sdk::DateTime::parse_odoo(stamp).expect("a pinned activity stamp");
        tx.execute(
            "UPDATE ai_session SET last_event_at = $2 WHERE id = $1",
            &[&ids[index], &stamp],
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    // the open list is ordered by last activity: D, A, E, B, C
    let mut tx = h.begin_as(7).await.unwrap();
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), false, None::<ValueMap>),
    )
    .await
    .unwrap();
    let listed = json_of(listed);
    let order: Vec<i64> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|summary| summary["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![ids[3], ids[0], ids[4], ids[1], ids[2]]);
    // a title search finds both Quarterly sessions, ordered by activity
    let found = call(
        &mut tx,
        "search_sessions",
        ("Quarterly".to_owned(), false, None::<ValueMap>),
    )
    .await
    .unwrap();
    let found = json_of(found);
    let order: Vec<i64> = found
        .as_array()
        .unwrap()
        .iter()
        .map(|summary| summary["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![ids[0], ids[2]]);
    // a search over the caller's own message text finds the sessions whose messages match
    for (word, expected) in [("hello", vec![ids[0]]), ("moon", vec![ids[0], ids[2]])] {
        let found = call(
            &mut tx,
            "search_sessions",
            (word.to_owned(), false, None::<ValueMap>),
        )
        .await
        .unwrap();
        let found = json_of(found);
        let order: Vec<i64> = found
            .as_array()
            .unwrap()
            .iter()
            .map(|summary| summary["id"].as_i64().unwrap())
            .collect();
        assert_eq!(order, expected, "search by '{word}'");
    }
    // resuming continues the stored sequence
    let resumed = call(&mut tx, "resume_session", (ids[0],)).await.unwrap();
    let resumed = json_of(resumed);
    assert_eq!(resumed["id"], ids[0]);
    assert_eq!(resumed["last_seq"], 1);
    assert!(resumed["anchor"].is_null());
    // archiving removes the session from the open list and keeps it listed and readable
    let archived = call(&mut tx, "archive_session", (ids[0], "archived".to_owned()))
        .await
        .unwrap();
    assert_eq!(
        json_of(archived),
        json!({"id": ids[0], "state": "archived"})
    );
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), false, None::<ValueMap>),
    )
    .await
    .unwrap();
    let order: Vec<i64> = json_of(listed)
        .as_array()
        .unwrap()
        .iter()
        .map(|summary| summary["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![ids[3], ids[4], ids[1], ids[2]]);
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), true, None::<ValueMap>),
    )
    .await
    .unwrap();
    let states: Vec<(i64, String)> = json_of(listed)
        .as_array()
        .unwrap()
        .iter()
        .map(|summary| {
            (
                summary["id"].as_i64().unwrap(),
                summary["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        states,
        vec![
            (ids[3], "open".to_owned()),
            (ids[0], "archived".to_owned()),
            (ids[4], "open".to_owned()),
            (ids[1], "open".to_owned()),
            (ids[2], "open".to_owned()),
        ]
    );
    // the archived session stays readable and resumes
    let resumed = call(&mut tx, "resume_session", (ids[0],)).await.unwrap();
    assert_eq!(json_of(resumed)["last_seq"], 1);
    // restoring reuses the same call
    let restored = call(&mut tx, "archive_session", (ids[0], "open".to_owned()))
        .await
        .unwrap();
    assert_eq!(json_of(restored), json!({"id": ids[0], "state": "open"}));
    // an invalid state is refused by name
    let error = call(&mut tx, "archive_session", (ids[0], "deleted".to_owned()))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert!(
        error.message().contains("erp.ai.state_invalid"),
        "{error:?}"
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// plus the readability verdict, never a value, and the verdict flips when the anchored record
/// becomes unreadable.
#[tokio::test]
async fn an_anchor_grants_nothing() {
    let h = harness().await;
    let company = seed_company(&h, "Rpc Anchor").await;
    seed_user(&h, 7, company).await;
    let mut tx = h.begin_as(7).await.unwrap();
    let anchored = partner(&mut tx, "Anchored Partner").await;
    let session = open_session(
        &mut tx,
        &NewSession {
            company,
            title: None,
            anchor_action: None,
            anchor_model: Some("res.partner".into()),
            anchor_res_id: anchored,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = h.begin_as(7).await.unwrap();
    let resumed = call(&mut tx, "resume_session", (session,)).await.unwrap();
    let resumed = json_of(resumed);
    // the result carries the id, the sequence and the anchor — nothing else, never a value
    assert_eq!(resumed.as_object().unwrap().len(), 3);
    let anchor = resumed["anchor"].clone();
    assert_eq!(anchor.as_object().unwrap().len(), 3);
    assert_eq!(anchor["model"], "res.partner");
    assert_eq!(anchor["res_id"], anchored);
    assert_eq!(anchor["available"], true);
    // the list summary carries the same identifiers
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), false, None::<ValueMap>),
    )
    .await
    .unwrap();
    let summary = json_of(listed).as_array().unwrap()[0].clone();
    assert_eq!(summary["anchor"], anchor);

    // the owner later loses read access: the record becomes invisible to an ordinary search
    let mut tx = h.begin_as(7).await.unwrap();
    tx.execute(
        "UPDATE res_partner SET active = false, write_date = now() WHERE id = $1",
        &[&anchored],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = h.begin_as(7).await.unwrap();
    let resumed = call(&mut tx, "resume_session", (session,)).await.unwrap();
    let resumed = json_of(resumed);
    let anchor = resumed["anchor"].clone();
    assert_eq!(anchor.as_object().unwrap().len(), 3);
    assert_eq!(anchor["available"], false);
    // the identifiers are the owner's own session data; no field value ever travels with them
    assert!(resumed.to_string().find("Anchored").is_none());
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), false, None::<ValueMap>),
    )
    .await
    .unwrap();
    let summary = json_of(listed).as_array().unwrap()[0].clone();
    assert_eq!(summary["anchor"]["available"], false);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn sessions_about_a_record_are_the_callers_own() {
    let h = harness().await;
    let company = seed_company(&h, "Rpc About").await;
    seed_user(&h, 7, company).await;
    seed_user(&h, 8, company).await;
    let mut tx = h.begin_as(7).await.unwrap();
    let shared = partner(&mut tx, "Shared Partner").await;
    let mine = open_session(
        &mut tx,
        &NewSession {
            company,
            title: None,
            anchor_action: None,
            anchor_model: Some("res.partner".into()),
            anchor_res_id: shared,
        },
    )
    .await
    .unwrap();
    let _unrelated = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = h.begin_as(8).await.unwrap();
    let theirs = open_session(
        &mut tx,
        &NewSession {
            company,
            title: None,
            anchor_action: None,
            anchor_model: Some("res.partner".into()),
            anchor_res_id: shared,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut about = ValueMap::new();
    about.insert("model".to_owned(), Value::str("res.partner"));
    about.insert("res_id".to_owned(), Value::Int(shared));
    // user 7 sees only their own anchored session, not the unrelated one
    let mut tx = h.begin_as(7).await.unwrap();
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), false, Some(about.clone())),
    )
    .await
    .unwrap();
    let order: Vec<i64> = json_of(listed)
        .as_array()
        .unwrap()
        .iter()
        .map(|summary| summary["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![mine]);
    tx.rollback().await.unwrap();
    // user 8 sees only their own
    let mut tx = h.begin_as(8).await.unwrap();
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), false, Some(about.clone())),
    )
    .await
    .unwrap();
    let order: Vec<i64> = json_of(listed)
        .as_array()
        .unwrap()
        .iter()
        .map(|summary| summary["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![theirs]);
    // a malformed about filter is refused by name
    let error = call(
        &mut tx,
        "search_sessions",
        (String::new(), false, Some(ValueMap::new())),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert!(
        error.message().contains("erp.ai.about_invalid"),
        "{error:?}"
    );
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// export re-reads tool results under the owner's current rights, model-written text after the
/// first unreadable source-bearing event becomes the hidden marker and the hidden text never
/// reaches the client, a field moved behind a group the owner lacks hides too, and a hidden
/// title is masked and left out of search results. (partial): the re-read rows show
/// current values and `changed_since` for the rows the caller can still read.
#[tokio::test]
async fn export_masks_model_written_text_under_current_rights() {
    let h = harness_fields().await;
    let company = seed_company(&h, "Rpc Mask").await;
    seed_user(&h, 7, company).await;
    let mut tx = h.begin_as(7).await.unwrap();
    let p1 = partner(&mut tx, "P1 Acme").await;
    let p2 = partner(&mut tx, "P2 Globex").await;
    // session A: every source readable, everything shown
    let a = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    append(&mut tx, a, &assistant("hello plain")).await.unwrap();
    append(
        &mut tx,
        a,
        &tool_result("res.partner", vec![p1], vec!["name"]),
    )
    .await
    .unwrap();
    append(&mut tx, a, &assistant("P1 is Acme")).await.unwrap();
    // session B: a later source becomes unreadable, an earlier message stays visible
    let b = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    append(&mut tx, b, &assistant("before any sources"))
        .await
        .unwrap();
    append(
        &mut tx,
        b,
        &tool_result("res.partner", vec![p2], vec!["name"]),
    )
    .await
    .unwrap();
    append(&mut tx, b, &assistant("P2 is Globex"))
        .await
        .unwrap();
    append(
        &mut tx,
        b,
        &tool_result("res.partner", vec![p1], vec!["name", "email"]),
    )
    .await
    .unwrap();
    append(&mut tx, b, &assistant("P1 again")).await.unwrap();
    tx.batch(&format!(
        "UPDATE ai_session SET title = 'Globex secrets', \
         title_sources = '[{{\"model\":\"res.partner\",\"ids\":[{p2}],\
         \"field_names\":[\"name\"]}}]' WHERE id = {b}"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // the owner later loses read access to P2
    let mut tx = h.begin_as(7).await.unwrap();
    tx.execute(
        "UPDATE res_partner SET active = false, write_date = now() WHERE id = $1",
        &[&p2],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = h.begin_as(7).await.unwrap();
    // session A: everything visible, the re-read rows carry current values
    let a_doc = export_content(&mut tx, a).await;
    let a_content = serde_json::to_string(&a_doc).unwrap();
    assert!(a_content.contains("hello plain"), "{a_content}");
    assert!(a_content.contains("P1 is Acme"), "{a_content}");
    assert!(!a_content.contains(HIDDEN), "{a_content}");
    let rows = a_doc["events"][1]["payload"]["rows"].clone();
    assert_eq!(rows[0]["id"], p1);
    assert_eq!(rows[0]["values"]["name"], "P1 Acme");
    // the synthetic stored fingerprint differs from the recomputed one: changed since
    assert_eq!(a_doc["events"][1]["payload"]["changed_since"], true);
    // session B: the boundary at event 2 hides every model-written text after it
    let b_doc = export_content(&mut tx, b).await;
    let b_content = serde_json::to_string(&b_doc).unwrap();
    assert!(b_content.contains("before any sources"), "{b_content}");
    assert!(!b_content.contains("P2 is Globex"), "{b_content}");
    assert!(!b_content.contains("P1 again"), "{b_content}");
    assert!(!b_content.contains("Globex"), "{b_content}");
    assert_eq!(b_content.matches(HIDDEN).count(), 2, "{b_content}");
    // the unreadable record is absent from the re-read rows; the readable one keeps the
    // accessible fields only (email moved behind a group the owner lacks)
    assert_eq!(
        b_doc["events"][1]["payload"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let rows = b_doc["events"][3]["payload"]["rows"].clone();
    assert_eq!(rows[0]["id"], p1);
    assert_eq!(rows[0]["values"]["name"], "P1 Acme");
    assert!(rows[0]["values"].get("email").is_none());
    // the hidden title is masked in the listing and never matches the search
    let found = call(
        &mut tx,
        "search_sessions",
        ("Globex".to_owned(), false, None::<ValueMap>),
    )
    .await
    .unwrap();
    assert!(json_of(found).as_array().unwrap().is_empty());
    let listed = call(
        &mut tx,
        "search_sessions",
        (String::new(), false, None::<ValueMap>),
    )
    .await
    .unwrap();
    let b_summary = json_of(listed)
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["id"].as_i64().unwrap() == b)
        .unwrap()
        .clone();
    assert_eq!(b_summary["title"], "");
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}
