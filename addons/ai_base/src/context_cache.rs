// SPDX-License-Identifier: MIT OR Apache-2.0
//! The assistant context cache (1): the exact bytes one provider exchange sent
//! and received, per session, turn and round, together with the SHA-256 hash of the turn's
//! stable prefix (D4.2). This is the only store that keeps record values the assistant saw
//! (D3.3), so nothing may read it except the owner-filtered bookkeeping read [`read_exchange`]:
//! the model ships no `ir.access` row (no group — not even administrators — reaches it through
//! RPC), and the runtime role's grants (published by [`publish_runtime_grants`]) are exactly
//! `SELECT` and `INSERT`. Rows are deleted with their session by the retention functions of
//! `AI1.SESSION-RETENTION` and retire with the daily partitions as the backstop.
//!
//! [`record_exchange`] is called by the turn loop (`erp-cli` assistant turn) once per provider
//! round trip, with the request and response bodies exactly as sent and received — never
//! re-serialized, because a re-serialization would not be the bytes that were sent (D3.1).
//! [`read_exchange`] serves context assembly (D4.3), which resends the stored bytes unchanged
//! while the user can still read them. Both run only for an ordinary, unimpersonated user and
//! scope every statement to the session's owner (D8). No provider call ever
//! happens here.

use erp_sdk::{prelude::*, settings::ParameterDecl};
use sha2::{Digest, Sha256};
use std::fmt;

// ---------------------------------------------------------------------------
// Declared limits (amendment L1): the cache size caps and the retention are system parameters
// with the design's proposed numbers as defaults, a floor and a hard bound, refused by name
// outside them. Zero never means unlimited. The retention sweep itself is
// `AI1.SESSION-RETENTION`; the hard bound below keeps the cache at or below the session's
// default retention (Q1: sessions 30 days, cache 7 days).
// ---------------------------------------------------------------------------

/// Provider exchanges recorded per session; past it a record answers `erp.ai.context_cache_full`.
pub const CACHE_ENTRIES: &str = "ai_base.context_cache_entries";
/// Recorded request plus response bytes per exchange; past it a record answers
/// `erp.ai.context_cache_too_large`. Refuse, never truncate.
pub const CACHE_BYTES: &str = "ai_base.context_cache_bytes";
/// Days the recorded exchanges are kept; rows are never kept longer than their session.
pub const CACHE_RETENTION_DAYS: &str = "ai_base.context_cache_retention_days";

fn parameters() -> Result<Vec<ParameterDecl>> {
    Ok(vec![
        ParameterDecl::integer(
            CACHE_ENTRIES,
            "Assistant: context cache exchanges per session",
            1200,
            (1, 100_000),
        )?,
        ParameterDecl::integer(
            CACHE_BYTES,
            "Assistant: context cache bytes per exchange",
            2_097_152,
            (4_096, 16_777_216),
        )?,
        ParameterDecl::integer(
            CACHE_RETENTION_DAYS,
            "Assistant: context cache retention in days",
            7,
            (1, 30),
        )?,
    ])
}
erp_sdk::system_parameters!(parameters);

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// One recorded provider exchange of a session (1): the request and response
/// bodies as `bytea`, exactly as sent and received (never `jsonb`, which does not keep bytes as
/// sent), the SHA-256 hash of the turn's stable prefix (D4.2) and the token counts. Daily
/// partitions with a 40-day backstop; rows die with their session through the
/// retention functions and retire with the partitions. No `ir.access` row grants any group
/// model access, so no RPC method can return a request or response body.
#[model(
    name = "ai.context.cache",
    description = "Assistant Context Cache",
    addon = "ai_base",
    partition = partition(interval = "day", premake = 2, retain = 40),
    company_access = unrestricted(reason = "the cache holds the bytes one session's provider exchanges sent and received; rows are reachable only through the owner-filtered bookkeeping read, and no access row grants any group direct model access"),
    constraints = [
        (
            "_round_positive",
            "CHECK (round >= 1)",
            "The exchange round is positive."
        ),
        (
            "_prefix_hash_shape",
            "CHECK (prefix_hash ~ '^[0-9a-f]{64}$')",
            "The prefix hash is 64 lowercase hexadecimal digits."
        )
    ],
    indexes = [("_session_turn_round", "(session_id, turn_id, round)")]
)]
pub struct AiContextCache {
    /// The owning session; rows die with it.
    #[field(
        many2one,
        comodel = "ai.session",
        required = true,
        ondelete = "cascade"
    )]
    pub session_id: Option<RecId>,
    /// The turn that ran the exchange; one id per turn, shared with the event log's `turn_id`
    /// (1).
    #[field(uuid, required = true)]
    pub turn_id: Option<Uuid>,
    /// The exchange's position in the turn, 1-based (1).
    #[field(integer, required = true)]
    pub round: i32,
    /// SHA-256 over the turn's stable prefix bytes (2), 64 lowercase hex digits.
    #[field(char, required = true, size = 64)]
    pub prefix_hash: Option<Arc<str>>,
    /// The request body exactly as sent to the provider, including record values the
    /// assistant saw (D3.3).
    #[field(binary, required = true)]
    pub request: Option<Blob>,
    /// The response body exactly as received; `NULL` when the caller cannot provide it (the
    /// raw-body capture lands with the turn wiring, `AI1.ASSIST-TURN`).
    #[field(binary)]
    pub response: Option<Blob>,
    /// The provider-reported input tokens of this exchange, when it reported any.
    #[field(bigint)]
    pub input_tokens: erp_sdk::BigInt,
    /// The provider-reported output tokens of this exchange, when it reported any.
    #[field(bigint)]
    pub output_tokens: erp_sdk::BigInt,
}

// ---------------------------------------------------------------------------
// Store operations. Every one runs only for an ordinary, unimpersonated user and
// scopes its statements to the session's owner, so a missing or foreign session answers the
// same concealed access error as the session store (2).
// ---------------------------------------------------------------------------

/// What [`record_exchange`] records for one provider exchange (1). The request and
/// response are stored exactly as given, byte-identical; only the hash of the prefix is stored,
/// so the prefix bytes themselves never leave the caller. `Debug` is deliberate: it shows only
/// lengths and counts, never bytes, so a logged value cannot leak record contents (D3.3).
pub struct Exchange<'a> {
    /// The running turn; one id per turn, shared with the event log's `turn_id` (1).
    pub turn_id: Uuid,
    /// The exchange's position in the turn, 1-based.
    pub round: i32,
    /// The turn's stable prefix bytes (2); recorded as their SHA-256 hash.
    pub prefix: &'a [u8],
    /// The request body exactly as sent to the provider.
    pub request: &'a [u8],
    /// The response body exactly as received, when the caller can provide it.
    pub response: Option<&'a [u8]>,
    /// The provider-reported input tokens, when known.
    pub input_tokens: Option<i64>,
    /// The provider-reported output tokens, when known.
    pub output_tokens: Option<i64>,
}

impl fmt::Debug for Exchange<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exchange")
            .field("turn_id", &self.turn_id)
            .field("round", &self.round)
            .field("prefix_bytes", &self.prefix.len())
            .field("request_bytes", &self.request.len())
            .field("response_bytes", &self.response.map_or(0, <[u8]>::len))
            .field("input_tokens", &self.input_tokens)
            .field("output_tokens", &self.output_tokens)
            .finish()
    }
}

/// One recorded exchange read back by the owner-filtered [`read_exchange`]: the exact bytes as
/// sent and received, the recorded prefix hash and the token counts. `Debug` shows only lengths
/// and counts, never the bytes themselves (3).
#[derive(Clone)]
pub struct CachedExchange {
    /// The turn that ran the exchange.
    pub turn_id: Uuid,
    /// The exchange's position in the turn, 1-based.
    pub round: i32,
    /// SHA-256 over the prefix bytes the turn recorded (2), 64 hex digits.
    pub prefix_hash: String,
    /// The request body exactly as sent.
    pub request: Vec<u8>,
    /// The response body exactly as received, when it was recorded.
    pub response: Option<Vec<u8>>,
    /// The provider-reported input tokens, when recorded.
    pub input_tokens: Option<i64>,
    /// The provider-reported output tokens, when recorded.
    pub output_tokens: Option<i64>,
}

impl fmt::Debug for CachedExchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedExchange")
            .field("turn_id", &self.turn_id)
            .field("round", &self.round)
            .field("prefix_hash", &self.prefix_hash)
            .field("request_bytes", &self.request.len())
            .field(
                "response_bytes",
                &self.response.as_ref().map_or(0, Vec::len),
            )
            .field("input_tokens", &self.input_tokens)
            .field("output_tokens", &self.output_tokens)
            .finish()
    }
}

fn require_user(tx: &Transaction) -> Result<i64> {
    if !crate::ordinary_user(&tx.env()) {
        return Err(Error::access("An interactive session is required."));
    }
    Ok(tx.env().user_id().get())
}

/// The concealed access error for a session the caller does not own or that does not exist: the
/// same wording as the session store's, either way, so existence is never revealed (
/// D1.2).
fn concealed() -> Error {
    Error::access(
        "Your access rights do not let you read 'Assistant Session' (ai.session) records.",
    )
}

/// SHA-256 over the stable prefix bytes, as 64 lowercase hex digits (2). Recording
/// the hash through this function — never from a caller-supplied string — keeps the invariant
/// : the recorded hash always equals the hash of the prefix bytes, so identical
/// prefix bytes record identical hashes between turns.
///
/// # Errors
/// `Internal` when the digest input cannot be processed (a defect, not input).
pub fn prefix_hash(prefix: &[u8]) -> Result<String> {
    let digest: [u8; 32] = Sha256::digest(prefix).into();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A fresh version-4 turn correlation id for one assistant turn (RFC 9562 §5.4). The turn loop
/// generates one per turn and uses the same id for the event log and for the cache rows, so a
/// turn's events and its recorded exchanges correlate (1, D3.1). Generation lives
/// beside the store that consumes it until `AI1.ASSIST-TURN` wires the turn loop; that slice
/// may move it.
///
/// # Errors
/// `Internal` when the operating system cannot provide 16 random bytes.
pub fn new_turn_id() -> Result<Uuid> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        Error::internal(format!("Could not generate an assistant turn id: {error}"))
    })?;
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // RFC 9562 variant
    Ok(Uuid::from_bytes(bytes))
}

/// Record one provider exchange of the caller's own session (1): the request and
/// response bodies exactly as given, the hash of the prefix bytes and the token counts. The
/// declared entry and byte caps (amendment L1) are refused by name and nothing is recorded,
/// never truncated.
///
/// # Errors
/// `Access` (concealed) for a missing or foreign session or a non-interactive caller;
/// `Validation` `erp.ai.context_cache_round_invalid` for a round below 1; `User`
/// `erp.ai.context_cache_full` naming the entry cap, or
/// `erp.ai.context_cache_too_large` naming the byte cap.
pub async fn record_exchange(
    tx: &mut Transaction,
    session: i64,
    exchange: &Exchange<'_>,
) -> Result<()> {
    let uid = require_user(tx)?;
    if exchange.round < 1 {
        return Err(Error::validation(
            "erp.ai.context_cache_round_invalid: the exchange round must be at least 1.",
        ));
    }
    let owned = tx
        .query(
            "SELECT 1 FROM ai_session WHERE id = $1 AND owner_user_id = $2",
            &[&session, &uid],
        )
        .await?;
    if owned.is_empty() {
        return Err(concealed());
    }
    let entry_cap = crate::catalog::limit(tx, CACHE_ENTRIES).await?;
    let entries = tx
        .query(
            "SELECT count(*)::int8 FROM ai_context_cache WHERE session_id = $1",
            &[&session],
        )
        .await?[0]
        .get::<_, i64>(0);
    let entry_cap = i64::try_from(entry_cap)
        .map_err(|_| Error::validation("Invalid assistant context-cache entry limit."))?;
    if entries >= entry_cap {
        return Err(Error::user(format!(
            "erp.ai.context_cache_full: the session reached its context-cache entry limit \
             ({entry_cap}); nothing was recorded."
        )));
    }
    let stored = exchange.request.len() + exchange.response.map_or(0, <[u8]>::len);
    let byte_cap = crate::catalog::limit(tx, CACHE_BYTES).await?;
    if stored > byte_cap {
        return Err(Error::user(format!(
            "erp.ai.context_cache_too_large: the exchange exceeds {byte_cap} recorded bytes; \
             nothing was recorded."
        )));
    }
    let hash = prefix_hash(exchange.prefix)?;
    tx.execute(
        "INSERT INTO ai_context_cache (create_date, write_date, session_id, turn_id, round, \
         prefix_hash, request, response, input_tokens, output_tokens) \
         VALUES ((now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC'), $1, $2::text::uuid, \
         $3, $4, $5, $6, $7, $8)",
        &[
            &session,
            &exchange.turn_id.to_string(),
            &exchange.round,
            &hash,
            &exchange.request,
            &exchange.response,
            &exchange.input_tokens,
            &exchange.output_tokens,
        ],
    )
    .await?;
    Ok(())
}

/// The owner-filtered bookkeeping read of the context cache (1): the recorded
/// exchange — the exact bytes as sent and received, the prefix hash and the token counts — for
/// the caller's own session, and nothing for another user's. This is application code only and
/// is never exposed over RPC (the model has no access row). A row the retention already removed
/// is `None`, which context assembly replaces per D4.3.
///
/// # Errors
/// `Access` (concealed) for a missing or foreign session or a non-interactive caller.
pub async fn read_exchange(
    tx: &mut Transaction,
    session: i64,
    turn_id: Uuid,
    round: i32,
) -> Result<Option<CachedExchange>> {
    let uid = require_user(tx)?;
    let owned = tx
        .query(
            "SELECT 1 FROM ai_session WHERE id = $1 AND owner_user_id = $2",
            &[&session, &uid],
        )
        .await?;
    if owned.is_empty() {
        return Err(concealed());
    }
    let rows = tx
        .query(
            "SELECT turn_id::text, round, prefix_hash::text, request, response, input_tokens, \
             output_tokens \
             FROM ai_context_cache \
             WHERE session_id = $1 AND turn_id = $2::text::uuid AND round = $3",
            &[&session, &turn_id.to_string(), &round],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(CachedExchange {
        turn_id: Uuid::parse(&row.get::<_, String>(0))?,
        round: row.get(1),
        prefix_hash: row.get(2),
        request: row.get(3),
        response: row.get(4),
        input_tokens: row.get(5),
        output_tokens: row.get(6),
    }))
}

// ---------------------------------------------------------------------------
// Runtime role grants: recorded exchanges are append-only and never rewritten (1,
// D9.2); deletion goes through the session retention functions only.
// ---------------------------------------------------------------------------

/// Publish the runtime role's rights on the context-cache table (1): exactly
/// `SELECT` and `INSERT`, never `UPDATE` or `DELETE`. Idempotent like the session store's
/// grants: revoke everything the role held, then grant the policy on the parent and on every
/// existing daily partition.
///
/// The composition root (`erp-cli` human publication) applies this beside the session store's
/// grants; until that one call is wired there, the runtime role's blanket CRUD on this table
/// stays in force (tracked as a follow-up of this slice, recorded in its report).
///
/// # Errors
/// An invalid role name, or the database's errors when the grants cannot be applied.
pub async fn publish_runtime_grants(tx: &mut Transaction, role: &str) -> Result<()> {
    if role.is_empty() || role.len() > 63 || role.chars().any(char::is_control) {
        return Err(Error::validation(
            "invalid assistant context cache runtime role",
        ));
    }
    let quoted = format!("\"{}\"", role.replace('"', "\"\""));
    tx.batch(&format!(
        "REVOKE ALL ON TABLE ai_context_cache FROM {quoted}; \
         GRANT SELECT, INSERT ON TABLE ai_context_cache TO {quoted}; \
         DO $$ DECLARE p text; BEGIN \
           FOR p IN SELECT c.relname::text FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
                    WHERE i.inhparent = 'ai_context_cache'::regclass \
           LOOP \
             EXECUTE 'REVOKE ALL ON TABLE ' || quote_ident(p) || ' FROM {quoted}'; \
             EXECUTE 'GRANT SELECT, INSERT ON TABLE ' || quote_ident(p) || ' TO {quoted}'; \
           END LOOP; \
         END $$;"
    ))
    .await
}
