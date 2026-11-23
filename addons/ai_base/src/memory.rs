// SPDX-License-Identifier: MIT OR Apache-2.0
//! Assistant memory: explicit, visible, editable entries in three scopes that
//! reach the model only as `Data`, never as `Instruction`. An entry belongs to exactly one
//! scope: **user** (preferences, owner only), **company** (glossary and policies, read by the
//! company's users and written by the administrator group) and **addon** (addon data, read-only
//! at runtime). Each entry stores a short key, its text (at most 500 characters), a state and
//! its origin; an entry accepted from a `memory_proposed` event keeps the soft
//! `(session, seq)` reference of that event and a copy of the references of the source-bearing
//! events before it (3), because the entry outlives its session.
//!
//! The assistant never writes memory on its own: [`propose_memory`] only shapes
//! a proposal bound like — a digest over scope, key and text, the owner and
//! session, an expiry — and [`confirm_memory`] applies it exactly once, after the same checks.
//! Appending the `memory_proposed` event and showing the card are `AI1.ASSIST-TURN` and
//! `AI1.MEMORY.WEB`; the prefix wiring (labelled `Data` parts) is `AI1.PREFIX-WIRING`. The
//! scoped read helpers return the entries those slices render. No provider call ever happens
//! here.

use erp_sdk::{prelude::*, settings::ParameterDecl};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Declared limits (amendment L1): every cap marks "proposed" is a system parameter
// with the ADR's number as its default, a floor and a hard bound, refused by name outside them.
// Zero never means unlimited. The per-entry byte cap may only be lowered (its hard bound is the
// 500-character column limit.1); the addon-scope character cap is checked at
// addon install by the data loader slice, which reads it here.
// ---------------------------------------------------------------------------

/// Active entries in one user's memory; past it a save answers `erp.ai.memory_full` naming the
/// user scope (2).
pub const MEMORY_MAX_ENTRIES: &str = "ai_base.memory_max_entries";
/// Active entries in one company's memory; past it `erp.ai.memory_full` names the company scope.
pub const MEMORY_COMPANY_ENTRIES: &str = "ai_base.memory_company_entries";
/// UTF-8 bytes of one entry's text (1: 500 characters, enforced as bytes; the
/// varchar(500) column is the hard backstop, so the parameter may only lower the effective cap).
pub const MEMORY_ENTRY_BYTES: &str = "ai_base.memory_entry_bytes";
/// Total characters of one user's active entries (2).
pub const MEMORY_USER_CHARS: &str = "ai_base.memory_user_chars";
/// Total characters of one company's active entries (2).
pub const MEMORY_COMPANY_CHARS: &str = "ai_base.memory_company_chars";
/// Total characters of one addon's entries, checked at install (2).
pub const MEMORY_ADDON_CHARS: &str = "ai_base.memory_addon_chars";
/// Record ids one accepted user entry may reference (3); more answers
/// `erp.ai.memory_full`.
pub const MEMORY_REF_IDS: &str = "ai_base.memory_ref_ids";
/// Seconds a memory proposal stays acceptable (3). The hard bound keeps
/// it below the idempotency replay window the confirmation endpoint must refuse to start past
/// (that startup check lands with the slice that wires the confirm endpoint).
pub const MEMORY_PROPOSAL_EXPIRY_SECS: &str = "ai_base.memory_proposal_expiry_secs";

fn parameters() -> Result<Vec<ParameterDecl>> {
    Ok(vec![
        ParameterDecl::integer(
            MEMORY_MAX_ENTRIES,
            "Assistant: active memory entries per user",
            20,
            (1, 100_000),
        )?,
        ParameterDecl::integer(
            MEMORY_COMPANY_ENTRIES,
            "Assistant: active memory entries per company",
            50,
            (1, 100_000),
        )?,
        ParameterDecl::integer(
            MEMORY_ENTRY_BYTES,
            "Assistant: memory entry text bytes",
            500,
            (1, 500),
        )?,
        ParameterDecl::integer(
            MEMORY_USER_CHARS,
            "Assistant: user memory characters",
            4_000,
            (1, 1_000_000),
        )?,
        ParameterDecl::integer(
            MEMORY_COMPANY_CHARS,
            "Assistant: company memory characters",
            8_000,
            (1, 1_000_000),
        )?,
        ParameterDecl::integer(
            MEMORY_ADDON_CHARS,
            "Assistant: addon memory characters",
            4_000,
            (1, 1_000_000),
        )?,
        ParameterDecl::integer(
            MEMORY_REF_IDS,
            "Assistant: referenced ids per memory entry",
            5_000,
            (1, 1_000_000),
        )?,
        ParameterDecl::integer(
            MEMORY_PROPOSAL_EXPIRY_SECS,
            "Assistant: memory proposal expiry in seconds",
            600,
            (1, 600),
        )?,
    ])
}
erp_sdk::system_parameters!(parameters);

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// One memory entry (1). The record rules of `security/ir.access.csv` give each
/// scope its access: a user reads and writes their own user entries only, reads the company
/// entries of their companies and every addon entry; the administrator group writes company
/// entries; no group writes addon entries (the module loader does). The soft origin reference
/// deliberately has no foreign key: the entry outlives its session, and a dangling pair is fine
/// (5).
#[model(
    name = "ai.memory",
    description = "Assistant Memory",
    order = "id",
    company_access = unrestricted(reason = "memory is scoped by its own record rules: user entries owner-only, company entries by the company's users and the administrator group, addon entries read-only"),
    constraints = [
        (
            "_scope_closed",
            "CHECK (scope IN ('user','company','addon'))",
            "The memory scope is user, company or addon."
        ),
        (
            "_state_closed",
            "CHECK (state IN ('active','archived'))",
            "The memory entry is active or archived."
        ),
        (
            "_origin_closed",
            "CHECK (origin IN ('typed','proposal','addon'))",
            "The memory origin is typed, proposal or addon."
        ),
        (
            "_scope_shape",
            "CHECK ((scope = 'user' AND owner_user_id IS NOT NULL AND company_id IS NULL AND module IS NULL) OR (scope = 'company' AND company_id IS NOT NULL AND owner_user_id IS NULL AND module IS NULL) OR (scope = 'addon' AND module IS NOT NULL AND owner_user_id IS NULL AND company_id IS NULL))",
            "An entry names exactly the owner its scope names."
        ),
        (
            "_origin_shape",
            "CHECK ((origin = 'proposal' AND origin_session IS NOT NULL AND origin_seq >= 1) OR (origin IN ('typed','addon') AND origin_session IS NULL AND origin_seq IS NULL))",
            "An accepted proposal names the event it came from; a typed or addon entry names none."
        ),
        (
            "_key_shape",
            "CHECK (key <> '')",
            "The memory key is not empty."
        )
    ],
    indexes = [
        ("_scope_owner_state", "(scope, owner_user_id, state)"),
        ("_scope_company_state", "(scope, company_id, state)")
    ],
    unique_indexes = [(
        "_origin_once",
        "(origin_session, origin_seq) WHERE origin_session IS NOT NULL",
        "One accepted entry per memory_proposed event: a confirmation never writes twice."
    )]
)]
pub struct AiMemory {
    /// `user`, `company` or `addon` (1).
    #[field(
        selection,
        required = true,
        selection = [("user", "User"), ("company", "Company"), ("addon", "Addon")]
    )]
    pub scope: Option<Arc<str>>,
    /// The owning user of a user entry; the only reader of it (1).
    #[field(many2one, comodel = "res.users", ondelete = "cascade")]
    pub owner_user_id: Option<RecId>,
    /// The company a company entry belongs to; read by its users (1).
    #[field(many2one, comodel = "res.company", ondelete = "cascade", index = true)]
    pub company_id: Option<RecId>,
    /// The addon an addon entry was shipped with (1).
    #[field(char, size = 255)]
    pub module: Option<Arc<str>>,
    /// A short key; the memory page shows key and text together (1).
    #[field(char, required = true, size = 128)]
    pub key: Option<Arc<str>>,
    /// The entry text, at most 500 characters (1). Plain data, never an
    /// instruction: it is stored and returned exactly as written.
    #[field(char, required = true, size = 500)]
    pub text: Option<Arc<str>>,
    /// `active` or `archived`; the caps count active entries only (1, D8.2).
    #[field(
        selection,
        required = true,
        default = "active",
        selection = [("active", "Active"), ("archived", "Archived")]
    )]
    pub state: Option<Arc<str>>,
    /// How the entry was made: typed by the user, accepted from a `memory_proposed` event, or
    /// addon data (1).
    #[field(
        selection,
        required = true,
        selection = [("typed", "Typed"), ("proposal", "Proposal"), ("addon", "Addon")]
    )]
    pub origin: Option<Arc<str>>,
    /// The soft session half of the accepted proposal's event (1); no foreign key,
    /// the session may be gone.
    #[field(bigint)]
    pub origin_session: Option<erp_sdk::BigInt>,
    /// The soft sequence half of the accepted proposal's event (1).
    #[field(integer)]
    pub origin_seq: Option<i32>,
    /// The copied references of the source-bearing events before the accepted proposal's event
    /// (1, D2.3); the reader withholds the entry once they cannot re-read them. A
    /// company entry never holds any (`erp.ai.memory_sourced`).
    #[field(json)]
    pub refs: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Store operations. Every one runs only for an ordinary, unimpersonated user.
// ---------------------------------------------------------------------------

/// The concealed access error for a memory entry the caller may not read (3): the
/// same wording whether the entry does not exist or belongs to someone else.
fn concealed() -> Error {
    Error::access("Your access rights do not let you read 'Assistant Memory' (ai.memory) records.")
}

/// The concealed access error for a session the caller does not own or that does not exist: the
/// session store's wording, shared by the other session-bound stores (2).
fn session_concealed() -> Error {
    Error::access(
        "Your access rights do not let you read 'Assistant Session' (ai.session) records.",
    )
}

fn require_user(tx: &Transaction) -> Result<i64> {
    if !crate::ordinary_user(&tx.env()) {
        return Err(Error::access("An interactive session is required."));
    }
    Ok(tx.env().user_id().get())
}

/// The caller's current company memberships, the table the principal resolver reads (
/// D1.4): the company read helper checks the requested company against it.
async fn company_memberships(tx: &mut Transaction, uid: i64) -> Result<Vec<i64>> {
    let rows = tx
        .query(
            "SELECT cid::int8 FROM res_company_users_rel WHERE user_id = $1 ORDER BY 1",
            &[&uid],
        )
        .await?;
    Ok(rows.iter().map(|row| row.get(0)).collect())
}

/// Whether the user holds the administrator group (`base.group_system`), implications included:
/// the group the ADR names for the company scope (1, D8.3).
async fn is_administrator(tx: &mut Transaction, uid: i64) -> Result<bool> {
    let Some(system) =
        erp_addon_base::group_graph::group_by_xmlid(tx, erp_addon_base::group_graph::GROUP_SYSTEM)
            .await?
    else {
        return Ok(false);
    };
    let direct = erp_addon_base::group_graph::direct_groups(tx, &[uid]).await?;
    let Some(groups) = direct.get(&uid) else {
        return Ok(false);
    };
    Ok(erp_addon_base::group_graph::Graph::load(tx)
        .await?
        .up(groups.iter().copied())
        .contains(&system))
}

/// The stored references of the session's source-bearing events before `seq` (3):
/// a `tool_result` or a `context_loaded` event's `source_refs`, in sequence order.
async fn source_refs(tx: &mut Transaction, session: i64, before_seq: i32) -> Result<Vec<Value>> {
    let rows = tx
        .query(
            "SELECT source_refs::text FROM ai_session_event \
             WHERE session_id = $1 AND seq < $2 \
               AND kind IN ('tool_result','context_loaded') AND source_refs IS NOT NULL \
             ORDER BY seq",
            &[&session, &before_seq],
        )
        .await?;
    rows.iter()
        .map(|row| {
            serde_json::from_str(&row.get::<_, String>(0)).map_err(|error| {
                Error::internal(format!(
                    "Could not read the stored references of a source-bearing event: {error}"
                ))
            })
        })
        .collect()
}

/// The record ids named by a set of copied references, the count.3 caps at
/// `ai_base.memory_ref_ids`.
fn ref_id_count(refs: &[Value]) -> usize {
    let mut total = 0;
    for item in refs {
        if let Some(ids) = item.get("ids").and_then(Value::as_array) {
            total += ids.len();
        } else if let Some(records) = item.get("records").and_then(Value::as_array) {
            for record in records {
                if let Some(ids) = record.get("ids").and_then(Value::as_array) {
                    total += ids.len();
                }
            }
        }
    }
    total
}

/// Whether the caller holds an active user entry with copied references: the prefix condition
/// that blocks a company proposal even in a fresh session (3).
async fn has_referenced_user_entries(tx: &mut Transaction, uid: i64) -> Result<bool> {
    let rows = tx
        .query(
            "SELECT 1 FROM ai_memory WHERE scope = 'user' AND owner_user_id = $1 \
             AND state = 'active' AND refs IS NOT NULL LIMIT 1",
            &[&uid],
        )
        .await?;
    Ok(!rows.is_empty())
}

/// SHA-256 over RFC 8785 canonical JSON of `scope`, `key` and `text`: the digest the
/// confirmation re-derives, so a changed text can never be accepted (3,
/// D5). The object is built in a `BTreeMap`, whose keys serialize in sorted order — the
/// canonical order for this shape.
///
/// # Errors
/// `Internal` when the canonical input cannot be serialized (a defect, not input).
pub fn memory_digest(scope: &str, key: &str, text: &str) -> Result<String> {
    let mut map = BTreeMap::new();
    map.insert("key", Value::String(key.to_owned()));
    map.insert("scope", Value::String(scope.to_owned()));
    map.insert("text", Value::String(text.to_owned()));
    let bytes = serde_json::to_vec(&map).map_err(|error| {
        Error::internal(format!("Could not serialize the memory digest: {error}"))
    })?;
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

// ---------------------------------------------------------------------------
// Propose and confirm. The assistant proposes only through [`propose_memory`], a tool with a
// fixed target (3): it shapes a proposal bound like and
// writes nothing. [`confirm_memory`] applies the proposal once, after the ownership, session,
// digest, expiry, group, provenance and cap checks; appending the `memory_proposed` event and
// showing the card are `AI1.ASSIST-TURN` and `AI1.MEMORY.WEB`.
// ---------------------------------------------------------------------------

/// What the assistant may propose (3): a scope, a short key and the exact text.
pub struct MemoryPropose<'a> {
    /// `user` or `company`; the addon scope is addon data and never proposed.
    pub scope: &'a str,
    /// The short key, 1 to 128 characters.
    pub key: &'a str,
    /// The exact text the card shows; at most `ai_base.memory_entry_bytes` bytes.
    pub text: &'a str,
}

/// A shaped memory proposal (3): everything the confirmation re-checks. It is
/// returned by [`propose_memory`] and applied once by [`confirm_memory`]; the turn appends the
/// `memory_proposed` event at [`MemoryProposal::origin_seq`] and the client keeps the proposal
/// for the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryProposal {
    /// The scope, as proposed.
    pub scope: String,
    /// The key, as proposed.
    pub key: String,
    /// The exact text, as proposed.
    pub text: String,
    /// The proposing user; a different user confirming is refused with the concealed error.
    pub owner: i64,
    /// The proposing session; a different session confirming is refused with the concealed
    /// error (3).
    pub session: i64,
    /// The proposing session's company, recorded with the proposal (1): the entry
    /// a company proposal makes belongs to it.
    pub company: i64,
    /// The sequence the `memory_proposed` event will take (`last_seq + 1`); the turn appends it
    /// there and the entry keeps the pair as its origin.
    pub origin_seq: i32,
    /// The copied references of the source-bearing events before the proposal (user scope
    /// only; a company proposal with any is refused with `erp.ai.memory_sourced`).
    pub refs: Option<Value>,
    /// SHA-256 over scope, key and text; a changed text never matches.
    pub digest: String,
    /// When the proposal stops being acceptable, by the database clock.
    pub expires_at: DateTime,
}

/// Shape a memory proposal in the caller's own session (3). Nothing is written; the
/// entry exists only after [`confirm_memory`]. The `memory_proposed` event is appended by the
/// turn that dispatches the tool, at the returned `origin_seq`.
///
/// # Errors
/// `Access` for a non-interactive caller, a session the caller does not own (concealed), or a
/// company proposal by a caller outside the administrator group (`erp.ai.memory_company_scope`);
/// `Validation` `erp.ai.memory_scope_invalid` or `erp.ai.memory_key_invalid`; `User`
/// `erp.ai.memory_full` for text over the entry byte cap or references over the id cap, or
/// `erp.ai.memory_sourced` for a company proposal from a session that read records or while the
/// caller's user memory holds referenced entries.
pub async fn propose_memory(
    tx: &mut Transaction,
    session: i64,
    input: &MemoryPropose<'_>,
) -> Result<MemoryProposal> {
    let uid = require_user(tx)?;
    let scope = match input.scope {
        "user" | "company" => input.scope,
        _ => {
            return Err(Error::validation(
                "erp.ai.memory_scope_invalid: the memory scope must be 'user' or 'company'.",
            ));
        }
    };
    let key_chars = input.key.chars().count();
    if key_chars == 0 || key_chars > 128 {
        return Err(Error::validation(
            "erp.ai.memory_key_invalid: the memory key must be 1 to 128 characters.",
        ));
    }
    let entry_cap = crate::catalog::limit(tx, MEMORY_ENTRY_BYTES).await?;
    if input.text.len() > entry_cap {
        return Err(Error::user(format!(
            "erp.ai.memory_full: the entry text exceeds {entry_cap} bytes; nothing was saved."
        )));
    }
    let owned = tx
        .query(
            "SELECT last_seq, company_id::int8 FROM ai_session WHERE id = $1 AND owner_user_id = $2",
            &[&session, &uid],
        )
        .await?;
    let Some(row) = owned.first() else {
        return Err(session_concealed());
    };
    let company: i64 = row.get(1);
    let origin_seq = row.get::<_, i32>(0) + 1;
    let refs = if scope == "company" {
        if !is_administrator(tx, uid).await? {
            return Err(Error::access(
                "erp.ai.memory_company_scope: the company memory scope is offered only to the \
                 administrator group.",
            ));
        }
        if !source_refs(tx, session, origin_seq).await?.is_empty()
            || has_referenced_user_entries(tx, uid).await?
        {
            return Err(Error::user(
                "erp.ai.memory_sourced: company memory must not hold record data, so a proposal \
                 from a session that read records, or made while your user memory holds \
                 referenced entries, is refused; propose a user entry instead.",
            ));
        }
        None
    } else {
        let sources = source_refs(tx, session, origin_seq).await?;
        let ids = ref_id_count(&sources);
        let cap = crate::catalog::limit(tx, MEMORY_REF_IDS).await?;
        if ids > cap {
            return Err(Error::user(format!(
                "erp.ai.memory_full: the proposal references {ids} record ids, more than the \
                 {cap} allowed; nothing was saved."
            )));
        }
        (!sources.is_empty()).then_some(Value::Array(sources))
    };
    let expiry_secs = i64::try_from(crate::catalog::limit(tx, MEMORY_PROPOSAL_EXPIRY_SECS).await?)
        .map_err(|_| Error::validation("Invalid assistant memory proposal expiry."))?;
    let expires_at = tx
        .query(
            "SELECT (now() AT TIME ZONE 'UTC') + make_interval(secs => $1::float8)",
            &[&(expiry_secs as f64)],
        )
        .await?[0]
        .get(0);
    Ok(MemoryProposal {
        scope: scope.to_owned(),
        key: input.key.to_owned(),
        text: input.text.to_owned(),
        owner: uid,
        session,
        company,
        origin_seq,
        refs,
        digest: memory_digest(scope, input.key, input.text)?,
        expires_at,
    })
}

/// Apply an accepted memory proposal exactly once (3): the checks of
/// the proposal's confirmation run here, before the one insert. A confirmation that already
/// succeeded returns the recorded entry id and never writes again; the partial unique index on
/// the origin is the database backstop. The confirmation surface (endpoint, idempotency key,
/// card) is a later slice and calls this function.
///
/// # Errors
/// `Access` (concealed) for a non-interactive caller, a different owner or session, a changed
/// text (digest), and — for a company proposal — a caller no longer in the administrator group
/// (`erp.ai.memory_company_scope`); `User` `erp.ai.proposal_expired` after the expiry,
/// `erp.ai.memory_sourced` for a company proposal that gained sources since, or
/// `erp.ai.memory_full` naming the scope at its entry or character cap. No refusal writes
/// anything.
pub async fn confirm_memory(
    tx: &mut Transaction,
    proposal: &MemoryProposal,
    session: i64,
) -> Result<i64> {
    let uid = require_user(tx)?;
    if proposal.owner != uid {
        return Err(concealed());
    }
    if proposal.session != session {
        return Err(concealed());
    }
    if memory_digest(&proposal.scope, &proposal.key, &proposal.text)? != proposal.digest {
        return Err(concealed());
    }
    // One confirmation per proposal: the advisory lock serializes concurrent confirms of the
    // same event, so the stored-outcome read and the insert below never race into two entries.
    tx.query(
        "SELECT pg_advisory_xact_lock(hashtextextended('ai.memory.confirm.' || $1::bigint::text \
         || '.' || $2::int::text, 0))",
        &[&proposal.session, &proposal.origin_seq],
    )
    .await?;
    let stored = tx
        .query(
            "SELECT id::int8 FROM ai_memory WHERE origin = 'proposal' \
             AND origin_session = $1 AND origin_seq = $2",
            &[&proposal.session, &proposal.origin_seq],
        )
        .await?;
    if let Some(row) = stored.first() {
        return Ok(row.get(0));
    }
    let now: DateTime = tx.query("SELECT (now() AT TIME ZONE 'UTC')", &[]).await?[0].get(0);
    if proposal.expires_at <= now {
        return Err(Error::user(
            "erp.ai.proposal_expired: the memory proposal expired; propose it again.",
        ));
    }
    if proposal.scope == "company" {
        if !is_administrator(tx, uid).await? {
            return Err(Error::access(
                "erp.ai.memory_company_scope: the company memory scope is offered only to the \
                 administrator group.",
            ));
        }
        if !source_refs(tx, proposal.session, proposal.origin_seq)
            .await?
            .is_empty()
            || has_referenced_user_entries(tx, uid).await?
        {
            return Err(Error::user(
                "erp.ai.memory_sourced: company memory must not hold record data, so a proposal \
                 from a session that read records, or made while your user memory holds \
                 referenced entries, is refused; propose a user entry instead.",
            ));
        }
    }
    // The caps (2), over the active entries of the scope, refused by name.
    let (count, chars) = if proposal.scope == "company" {
        let rows = tx
            .query(
                "SELECT count(*)::int8, COALESCE(sum(char_length(text)), 0)::int8 \
                 FROM ai_memory WHERE scope = 'company' AND company_id = $1 AND state = 'active'",
                &[&proposal.company],
            )
            .await?;
        (rows[0].get::<_, i64>(0), rows[0].get::<_, i64>(1))
    } else {
        let rows = tx
            .query(
                "SELECT count(*)::int8, COALESCE(sum(char_length(text)), 0)::int8 \
                 FROM ai_memory WHERE scope = 'user' AND owner_user_id = $1 AND state = 'active'",
                &[&uid],
            )
            .await?;
        (rows[0].get::<_, i64>(0), rows[0].get::<_, i64>(1))
    };
    let (entries_key, chars_key, scope_name) = if proposal.scope == "company" {
        (MEMORY_COMPANY_ENTRIES, MEMORY_COMPANY_CHARS, "company")
    } else {
        (MEMORY_MAX_ENTRIES, MEMORY_USER_CHARS, "user")
    };
    let entries_cap = i64::try_from(crate::catalog::limit(tx, entries_key).await?)
        .map_err(|_| Error::validation("Invalid assistant memory entry limit."))?;
    if count + 1 > entries_cap {
        return Err(Error::user(format!(
            "erp.ai.memory_full: the {scope_name} memory holds {count} active entries \
             ({entries_cap} allowed); nothing was saved."
        )));
    }
    let chars_cap = i64::try_from(crate::catalog::limit(tx, chars_key).await?)
        .map_err(|_| Error::validation("Invalid assistant memory character limit."))?;
    let added = i64::try_from(proposal.text.chars().count())
        .map_err(|_| Error::validation("Invalid assistant memory text length."))?;
    if chars + added > chars_cap {
        return Err(Error::user(format!(
            "erp.ai.memory_full: the {scope_name} memory would exceed {chars_cap} characters; \
             nothing was saved."
        )));
    }
    let refs = proposal
        .refs
        .as_ref()
        .map(|refs| {
            serde_json::to_string(refs).map_err(|error| {
                Error::internal(format!("Could not serialize memory references: {error}"))
            })
        })
        .transpose()?;
    let owner: Option<i64> = (proposal.scope != "company").then_some(proposal.owner);
    let company: Option<i64> = (proposal.scope == "company").then_some(proposal.company);
    let inserted = tx
        .query(
            "INSERT INTO ai_memory (create_date, write_date, scope, owner_user_id, company_id, \
             module, key, text, state, origin, origin_session, origin_seq, refs) \
             VALUES ((now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC'), $1, $2, $3, NULL, \
             $4, $5, 'active', 'proposal', $6, $7, $8::text::jsonb) \
             RETURNING id::int8",
            &[
                &proposal.scope,
                &owner,
                &company,
                &proposal.key,
                &proposal.text,
                &proposal.session,
                &proposal.origin_seq,
                &refs,
            ],
        )
        .await?;
    Ok(inserted[0].get(0))
}

// ---------------------------------------------------------------------------
// Scoped reads. The prefix wiring (AI1.PREFIX-WIRING) labels these entries as `Data` parts
// with scope and id (4); the memory page reads through the ordinary
// record rules. An entry with references is withheld by the caller once the user can no longer
// re-read them (3, D2.4), which is why [`MemoryEntry`] carries the references.
// ---------------------------------------------------------------------------

/// One entry as read back for the prefix and the memory page (4): plain data —
/// scope, id, key and the exact text — never an instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    /// The entry id, the label of its `Data` part.
    pub id: i64,
    /// The scope, the label of its `Data` part.
    pub scope: String,
    /// The short key.
    pub key: String,
    /// The exact stored text.
    pub text: String,
    /// How the entry was made.
    pub origin: String,
    /// The soft session of an accepted proposal's event.
    pub origin_session: Option<i64>,
    /// The soft sequence of an accepted proposal's event.
    pub origin_seq: Option<i32>,
    /// The copied references the caller withholds on when unreadable (3).
    pub refs: Option<Value>,
}

/// The shared row shape of the scoped reads, mapped to [`MemoryEntry`].
fn entries_of(rows: &[erp_sdk::Row]) -> Result<Vec<MemoryEntry>> {
    rows.iter()
        .map(|row| {
            let refs: Option<String> = row.get(7);
            let refs = refs
                .map(|refs| {
                    serde_json::from_str(&refs).map_err(|error| {
                        Error::internal(format!("Could not read stored memory references: {error}"))
                    })
                })
                .transpose()?;
            Ok(MemoryEntry {
                id: row.get(0),
                scope: row.get(1),
                key: row.get(2),
                text: row.get(3),
                origin: row.get(4),
                origin_session: row.get(5),
                origin_seq: row.get(6),
                refs,
            })
        })
        .collect()
}

/// The active user entries of the caller: the owner-only scope (1). A user always
/// reads their own memory, so this is scoped, never refused.
///
/// # Errors
/// `Access` for a non-interactive caller.
pub async fn read_user_memory(tx: &mut Transaction) -> Result<Vec<MemoryEntry>> {
    let uid = require_user(tx)?;
    let rows = tx
        .query(
            "SELECT id::int8, scope::text, key::text, text::text, origin::text, \
             origin_session::int8, origin_seq::int4, refs::text FROM ai_memory \
             WHERE scope = 'user' AND owner_user_id = $1 AND state = 'active' ORDER BY id",
            &[&uid],
        )
        .await?;
    entries_of(&rows)
}

/// The active company entries of `company`, which must be among the caller's current companies
/// (1): the scope every user of the company reads.
///
/// # Errors
/// `Access` for a non-interactive caller or a company outside the caller's current set.
pub async fn read_company_memory(tx: &mut Transaction, company: i64) -> Result<Vec<MemoryEntry>> {
    let uid = require_user(tx)?;
    let memberships = company_memberships(tx, uid).await?;
    if !memberships.contains(&company) {
        return Err(Error::access("Select a company."));
    }
    let rows = tx
        .query(
            "SELECT id::int8, scope::text, key::text, text::text, origin::text, \
             origin_session::int8, origin_seq::int4, refs::text FROM ai_memory \
             WHERE scope = 'company' AND company_id = $1 AND state = 'active' ORDER BY id",
            &[&company],
        )
        .await?;
    entries_of(&rows)
}

/// The active addon entries, readable by every runtime user (1).
///
/// # Errors
/// `Access` for a non-interactive caller.
pub async fn read_addon_memory(tx: &mut Transaction) -> Result<Vec<MemoryEntry>> {
    let _uid = require_user(tx)?;
    let rows = tx
        .query(
            "SELECT id::int8, scope::text, key::text, text::text, origin::text, \
             origin_session::int8, origin_seq::int4, refs::text FROM ai_memory \
             WHERE scope = 'addon' AND state = 'active' ORDER BY module, key, id",
            &[],
        )
        .await?;
    entries_of(&rows)
}
