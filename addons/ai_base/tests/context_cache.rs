// SPDX-License-Identifier: MIT OR Apache-2.0
//! The context cache (1): bytes exactly as sent, the recorded prefix hash, the
//! owner-filtered bookkeeping read, the declared caps and the runtime-role grants
//! (to 018). Every exchange below is synthetic fixture data; no provider call
//! happens in any of these tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use erp_addon_ai_base::context_cache::{
    CACHE_BYTES, CACHE_ENTRIES, Exchange, new_turn_id, prefix_hash, publish_runtime_grants,
    read_exchange, record_exchange,
};
use erp_addon_ai_base::session::{NewSession, open_session};
use erp_sdk::{ErrorKind, Uuid};
use erp_testkit::{Harness, tokio_postgres::error::SqlState};
use sha2::{Digest, Sha256};

const CONCEALED: &str =
    "Your access rights do not let you read 'Assistant Session' (ai.session) records.";

const CACHE_FULL: &str = "erp.ai.context_cache_full: the session reached its context-cache entry \
    limit (2); nothing was recorded.";

const CACHE_TOO_LARGE: &str = "erp.ai.context_cache_too_large: the exchange exceeds 4096 \
    recorded bytes; nothing was recorded.";

const ROUND_INVALID: &str =
    "erp.ai.context_cache_round_invalid: the exchange round must be at least 1.";

const INTERACTIVE: &str = "An interactive session is required.";

fn turn(n: &str) -> Uuid {
    Uuid::parse(n).unwrap()
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

fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A minimal recorded exchange with fixed token counts; response bytes stay absent.
fn exchange<'a>(turn_id: Uuid, round: i32, prefix: &'a [u8], request: &'a [u8]) -> Exchange<'a> {
    Exchange {
        turn_id,
        round,
        prefix,
        request,
        response: None,
        input_tokens: Some(12),
        output_tokens: Some(1),
    }
}

/// with the prefix hash for the owner's session and the concealed refusal for another user's,
/// and the shipped access file grants the cache model to no group so no RPC method can return
/// a request or response body; the exhaustive per-method RPC sweep of the ai models rides on
/// AI1.ASSIST-TURN's endpoint tests, and the administrator half on base.group_system implying
/// base.group_user as)
#[tokio::test]
async fn the_cache_keeps_bytes_as_sent_and_is_not_readable_by_rpc() {
    let h = harness().await;
    let company = seed_company(&h, "Cache Sessions").await;
    seed_user(&h, 7, company).await;
    seed_user(&h, 8, company).await;
    let turn_id = turn("6f25f1ba-1eb2-4a19-8b16-5e07ea1a1c93");
    let prefix = b"stable prefix fixture".to_vec();
    let request = br#"{"type":"tool_result","tool_use_id":"call-1","content":"{\"data\":{\"model\":\"res.partner\",\"ids\":[101]}}"}"#.to_vec();
    let response =
        br#"{"object":"chat.completion","choices":[{"message":{"content":"done"}}]}"#.to_vec();
    let mut tx = h.begin_as(7).await.unwrap();
    let session = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    record_exchange(
        &mut tx,
        session,
        &Exchange {
            turn_id,
            round: 1,
            prefix: &prefix,
            request: &request,
            response: Some(&response),
            input_tokens: Some(40),
            output_tokens: Some(3),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // the owner's bookkeeping read returns the exact bytes as sent and the recorded hash
    let mut tx = h.begin_as(7).await.unwrap();
    let cached = read_exchange(&mut tx, session, turn_id, 1)
        .await
        .unwrap()
        .expect("the owner's recorded exchange");
    assert_eq!(cached.turn_id, turn_id);
    assert_eq!(cached.round, 1);
    assert_eq!(cached.request, request);
    assert_eq!(cached.response, Some(response));
    assert_eq!(cached.input_tokens, Some(40));
    assert_eq!(cached.output_tokens, Some(3));
    assert_eq!(cached.prefix_hash, sha256_hex(&prefix));
    // a round that was never recorded is None, the retention fallback.3
    assert!(
        read_exchange(&mut tx, session, turn_id, 2)
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
    drop(tx);

    // another user's read returns nothing: the concealed refusal, for a missing row as well
    let mut tx = h.begin_as(8).await.unwrap();
    let error = read_exchange(&mut tx, session, turn_id, 1)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(error.message(), CONCEALED);
    // and the other user cannot record into the session either
    let foreign = Exchange {
        turn_id,
        round: 1,
        prefix: &prefix,
        request: &request,
        response: None,
        input_tokens: None,
        output_tokens: None,
    };
    let error = record_exchange(&mut tx, session, &foreign)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(error.message(), CONCEALED);
    tx.rollback().await.unwrap();
    drop(tx);

    // the store itself refuses a non-interactive caller
    let mut tx = h.begin().await.unwrap();
    let elevated = Exchange {
        turn_id,
        round: 1,
        prefix: &prefix,
        request: &request,
        response: None,
        input_tokens: None,
        output_tokens: None,
    };
    let error = record_exchange(&mut tx, session, &elevated)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access, "{error:?}");
    assert_eq!(error.message(), INTERACTIVE);
    tx.rollback().await.unwrap();
    drop(tx);

    // the shipped access file grants the cache model to no group: no RPC method can read it
    let csv = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("security/ir.access.csv"),
    )
    .unwrap();
    assert!(
        !csv.lines()
            .any(|line| line.contains("model_ai_context_cache")),
        "the cache model must have no access row: {csv}"
    );
    h.cleanup().await.unwrap();
}

/// recorded prefix hashes equal to the independent SHA-256 of the bytes, and different prefix
/// bytes change the hash; assembling the real prefix across a live turn and the "adding a
/// memory entry changes both" half are AI1.ASSIST-TURN and AI1.MEMORY)
#[tokio::test]
async fn identical_prefix_bytes_record_identical_hashes_between_turns() {
    let h = harness().await;
    let company = seed_company(&h, "Prefix Sessions").await;
    seed_user(&h, 7, company).await;
    let turn_one = turn("6f25f1ba-1eb2-4a19-8b16-5e07ea1a1c93");
    let turn_two = turn("6f25f1ba-1eb2-4a19-8b16-5e07ea1a1c94");
    let turn_three = turn("6f25f1ba-1eb2-4a19-8b16-5e07ea1a1c95");
    let prefix = b"the stable prefix bytes".to_vec();
    let mut tx = h.begin_as(7).await.unwrap();
    let session = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    record_exchange(
        &mut tx,
        session,
        &exchange(turn_one, 1, &prefix, b"turn one"),
    )
    .await
    .unwrap();
    record_exchange(
        &mut tx,
        session,
        &exchange(turn_two, 1, &prefix, b"turn two"),
    )
    .await
    .unwrap();
    let first = read_exchange(&mut tx, session, turn_one, 1)
        .await
        .unwrap()
        .unwrap();
    let second = read_exchange(&mut tx, session, turn_two, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.prefix_hash, second.prefix_hash);
    // the recorded hash equals the independent SHA-256 of the prefix bytes
    assert_eq!(first.prefix_hash, sha256_hex(&prefix));
    // a different prefix (a memory entry later) changes the recorded hash
    let changed = b"the stable prefix bytes + a memory entry".to_vec();
    record_exchange(
        &mut tx,
        session,
        &exchange(turn_three, 1, &changed, b"turn three"),
    )
    .await
    .unwrap();
    let third = read_exchange(&mut tx, session, turn_three, 1)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(third.prefix_hash, first.prefix_hash);
    assert_eq!(third.prefix_hash, sha256_hex(&changed));
    // rounds of one turn share the turn's prefix hash
    record_exchange(
        &mut tx,
        session,
        &exchange(turn_one, 2, &prefix, b"turn one"),
    )
    .await
    .unwrap();
    let round_two = read_exchange(&mut tx, session, turn_one, 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(round_two.prefix_hash, first.prefix_hash);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// The prefix hash is SHA-256, pinned against known vectors so a drift cannot go unnoticed.
#[test]
fn the_prefix_hash_matches_sha256_known_vectors() {
    assert_eq!(
        prefix_hash(b"").unwrap(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        prefix_hash(b"abc").unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// The generated turn id is a valid version-4 UUID, fresh on every call.
#[test]
fn a_turn_id_is_a_fresh_version_4_uuid() {
    let first = new_turn_id().unwrap();
    let second = new_turn_id().unwrap();
    assert_ne!(first, second);
    for id in [first, second] {
        let text = id.to_string();
        assert_eq!(text.len(), 36);
        assert_eq!(&text[14..15], "4", "{text}");
        assert!(matches!(&text[19..20], "8" | "9" | "a" | "b"), "{text}");
    }
}

/// so a later turn can resend them as sent; resending them into the next turn's assembled
/// context and the `content withheld: access changed` replacement are AI1.ASSIST-TURN)
#[tokio::test]
async fn recorded_bytes_read_back_byte_identical_for_resend() {
    let h = harness().await;
    let company = seed_company(&h, "Resend Sessions").await;
    seed_user(&h, 7, company).await;
    let turn_id = turn("6f25f1ba-1eb2-4a19-8b16-5e07ea1a1c93");
    let prefix = b"prefix for resend".to_vec();
    let request = br#"{"type":"tool_result","tool_use_id":"call-1","content":"{\"data\":{\"models\":[{\"name\":\"res.partner\",\"description\":\"Contact\"}]},\"has_more\":false}"}"#.to_vec();
    let response =
        br#"{"object":"chat.completion","usage":{"prompt_tokens":15,"completion_tokens":2}}"#
            .to_vec();
    let mut tx = h.begin_as(7).await.unwrap();
    let session = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    record_exchange(
        &mut tx,
        session,
        &Exchange {
            turn_id,
            round: 1,
            prefix: &prefix,
            request: &request,
            response: Some(&response),
            input_tokens: Some(15),
            output_tokens: Some(2),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // the stored copy is the exact bytes, unchanged across repeated reads
    let mut tx = h.begin_as(7).await.unwrap();
    for _ in 0..2 {
        let cached = read_exchange(&mut tx, session, turn_id, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached.request, request);
        assert_eq!(cached.response, Some(response.clone()));
    }
    // a response the caller could not provide stays absent, not invented
    record_exchange(
        &mut tx,
        session,
        &Exchange {
            turn_id,
            round: 2,
            prefix: &prefix,
            request: &request,
            response: None,
            input_tokens: Some(15),
            output_tokens: None,
        },
    )
    .await
    .unwrap();
    let without = read_exchange(&mut tx, session, turn_id, 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(without.request, request);
    assert!(without.response.is_none());
    assert!(without.output_tokens.is_none());
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// never truncate)
#[tokio::test]
async fn the_cache_caps_refuse_by_name_and_record_nothing() {
    let h = harness().await;
    let company = seed_company(&h, "Capped Caches").await;
    seed_user(&h, 7, company).await;
    set_parameter(&h, CACHE_ENTRIES, 2).await;
    set_parameter(&h, CACHE_BYTES, 4096).await;
    let turn_id = turn("6f25f1ba-1eb2-4a19-8b16-5e07ea1a1c93");
    let prefix = b"prefix".to_vec();
    let mut tx = h.begin_as(7).await.unwrap();
    let session = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    let small = |round: i32| Exchange {
        turn_id,
        round,
        prefix: &prefix,
        request: b"small request".as_slice(),
        response: None,
        input_tokens: None,
        output_tokens: None,
    };
    record_exchange(&mut tx, session, &small(1)).await.unwrap();
    // the byte cap: request plus response together exceed it, nothing is recorded
    let oversized = Exchange {
        turn_id,
        round: 2,
        prefix: &prefix,
        request: b"request".as_slice(),
        response: Some(&vec![b'x'; 5000]),
        input_tokens: None,
        output_tokens: None,
    };
    let error = record_exchange(&mut tx, session, &oversized)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    assert_eq!(error.message(), CACHE_TOO_LARGE);
    // the entry cap: the second exchange fits, the third is refused by name
    record_exchange(&mut tx, session, &small(3)).await.unwrap();
    let error = record_exchange(&mut tx, session, &small(4))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    assert_eq!(error.message(), CACHE_FULL);
    let entries: i64 = tx
        .query(
            "SELECT count(*)::int8 FROM ai_context_cache WHERE session_id = $1",
            &[&session],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(entries, 2);
    // the round below 1 is a validation refusal
    let error = record_exchange(&mut tx, session, &small(0))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert_eq!(error.message(), ROUND_INVALID);
    assert_eq!(entries, 2);
    tx.rollback().await.unwrap();
    h.cleanup().await.unwrap();
}

/// INSERT and refuse UPDATE and DELETE, so no runtime path can rewrite or remove a recorded
/// exchange; the composition-root call that applies these grants in production is a
/// follow-up of this slice, recorded in its report)
#[tokio::test]
async fn the_runtime_role_cannot_update_or_delete_cache_rows() {
    let h = harness().await;
    let company = seed_company(&h, "Granted Caches").await;
    seed_user(&h, 7, company).await;
    let mut tx = h.begin_as(7).await.unwrap();
    let session = open_session(&mut tx, &NewSession::new(company))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // a real login role narrowed to exactly the published policy (1)
    let config = h.read_only_role().await.unwrap();
    let role = config.get_user().unwrap().to_owned();
    let mut tx = h.begin().await.unwrap();
    tx.batch(&format!(
        "REVOKE ALL ON ALL TABLES IN SCHEMA public FROM \"{role}\""
    ))
    .await
    .unwrap();
    publish_runtime_grants(&mut tx, &role).await.unwrap();
    tx.commit().await.unwrap();
    let (runtime, connection) = config
        .connect(erp_testkit::tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);

    // INSERT works (the store's own write path)
    runtime
        .execute(
            "INSERT INTO ai_context_cache (create_date, write_date, session_id, turn_id, \
             round, prefix_hash, request) \
             VALUES ((now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC'), $1, \
             '6f25f1ba-1eb2-4a19-8b16-5e07ea1a1c93', 1, \
             'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', '')",
            &[&session],
        )
        .await
        .expect("the runtime role inserts cache rows");
    // SELECT works (the owner-filtered bookkeeping read)
    let seen: i64 = runtime
        .query_one("SELECT count(*)::int8 FROM ai_context_cache", &[])
        .await
        .expect("the runtime role reads cache rows")
        .get(0);
    assert_eq!(seen, 1);
    // UPDATE and DELETE are refused by grants
    for sql in [
        "UPDATE ai_context_cache SET round = round",
        "DELETE FROM ai_context_cache",
        "TRUNCATE ai_context_cache",
    ] {
        let error = runtime.batch_execute(sql).await.expect_err(sql);
        assert_eq!(
            error.code(),
            Some(&SqlState::INSUFFICIENT_PRIVILEGE),
            "{sql}: {error:?}"
        );
    }
    drop(runtime);
    h.cleanup().await.unwrap();
}
