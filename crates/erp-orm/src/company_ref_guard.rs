// SPDX-License-Identifier: MIT OR Apache-2.0
//! `ondelete = "restrict"` of a company-dependent many2one under concurrency (supplement
//! of 2026-11-15).
//!
//! A reference held in a company map is not a foreign key, so the deletion of its target is
//! refused by a scan of the holder maps ([`crate::unlink`]). On its own that scan loses against a
//! write running at the same time: the writer may commit its reference between the scan and the
//! `DELETE`, and the scan cannot see a reference committed after the deleter's snapshot at all.
//! This module makes the two sides conflict in PostgreSQL, the way a foreign key does:
//!
//! - the **writer** of a reference takes `FOR KEY SHARE` on the target row, makes sure the
//!   target's guard row exists and upserts one link row per `(holder, company)` entry; the link
//!   rows of the entries it removes are deleted ([`Transaction::guard_company_references`]);
//! - the **deleter** takes `FOR UPDATE` on the rows it deletes before the scan
//!   ([`lock_targets`]), and after the refusals inserts and deletes their guard rows
//!   ([`retire`]). The link rows go with their guard row (`ON DELETE CASCADE`).
//!
//! Both sides run at `REPEATABLE READ`: the record API refuses a `READ COMMITTED` transaction
//! (`Transaction::require_record_api`). The row locks make the two wait for each other; the rows
//! make the conflict visible past the snapshot. PostgreSQL raises `40001` for the deleter when a
//! guard row it cannot see exists (the `INSERT … ON CONFLICT DO NOTHING`) or when a link row was
//! inserted or rewritten after its snapshot (the cascade's cross-check): the retried unit of work
//! sees the reference and is refused by the scan. It raises `40001` for the writer when the target
//! was deleted after the writer's snapshot (the `FOR KEY SHARE`).
//!
//! The map stays the only source of the value and of the refusal. A link row that is missing (a
//! map written by SQL outside the ORM or by `migrate_company_dependent`) means that write is not
//! protected against a concurrent deletion. A link row left behind (its holder row went by a
//! database cascade) is harmless and goes when its target is deleted. A target the writer's
//! snapshot does not show gets no lock but does get its guard and link rows: the row may have been
//! created after that snapshot, and an id without any row may still be assigned.

use erp_fields::{Cell, Error, OnDelete, RecId, Result};
use erp_registry::{FieldSpec, ModelSpec};
use serde_json::Value as Json;

use crate::Transaction;

/// The two tables. `comodel`, `holder_model` and `field` are registry names; the ids have no
/// foreign key to the models' tables (any model can be a comodel or a holder). The link key
/// starts with the holder so the rows of deleted holders are found by a prefix.
const DDL: &str = "\
CREATE TABLE IF NOT EXISTS erp_company_ref_guard (
    comodel text NOT NULL,
    target_id int8 NOT NULL CHECK (target_id > 0),
    PRIMARY KEY (comodel, target_id)
);
CREATE TABLE IF NOT EXISTS erp_company_ref_link (
    holder_model text NOT NULL,
    holder_id int8 NOT NULL CHECK (holder_id > 0),
    field text NOT NULL,
    company_id int8 NOT NULL CHECK (company_id > 0),
    comodel text NOT NULL,
    target_id int8 NOT NULL,
    PRIMARY KEY (holder_model, holder_id, field, company_id),
    FOREIGN KEY (comodel, target_id)
        REFERENCES erp_company_ref_guard (comodel, target_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS erp_company_ref_link_target
    ON erp_company_ref_link (comodel, target_id)";

/// The guard and link tables, installed by `Orm::sync_schema` like the other core tables of
/// `erp-orm` (listed in `core_ddl::ORM_DDL`).
pub(crate) const COMPANY_REF_GUARD_TABLES: crate::CoreDdl = crate::CoreDdl {
    owner: "erp-orm",
    name: "erp_company_ref_guard",
    purpose: "the restrict guard of company-dependent many2one references",
    tables: &["erp_company_ref_guard", "erp_company_ref_link"],
    sql: DDL,
    after_models: None,
};

/// What one statement does to the entries of one guarded field: the entries it sets to a target
/// id, and the entries it removes or sets to no record.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// Holder row, company, target of each entry set to an id.
    set: Vec<(i64, i64, i64)>,
    /// Holder row and company of each entry removed or set to no record.
    cleared: Vec<(i64, i64)>,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.set.is_empty() && self.cleared.is_empty()
    }
}

/// Whether references stored in `field` are guarded: a company-dependent many2one of its own
/// model (never an inherited copy) with `ondelete = "restrict"`.
pub(crate) fn guards(field: &FieldSpec) -> bool {
    field.company_dependent
        && field.inherited.is_none()
        && field.ty.comodel().is_some()
        && field.ty.ondelete() == Some(OnDelete::Restrict)
}

fn shape(what: &str) -> Error {
    Error::programming(format!(
        "company-dependent many2one {what}: not a map of company ids to record ids"
    ))
}

fn company_of(key: &str) -> Result<i64> {
    key.parse::<i64>()
        .ok()
        .filter(|company| *company > 0 && company.to_string() == key)
        .ok_or_else(|| shape("company key"))
}

/// The target a stored many2one value names: `None` for JSON null (no record).
fn target_of(value: &Json) -> Result<Option<i64>> {
    match value {
        Json::Null => Ok(None),
        Json::Number(number) => number
            .as_i64()
            .filter(|id| *id > 0)
            .map(Some)
            .ok_or_else(|| shape("value")),
        _ => Err(shape("value")),
    }
}

fn holder_id(holder: RecId) -> Result<i64> {
    holder.as_real().map(|id| id.get()).ok_or_else(|| {
        Error::programming(
            "a company-dependent reference of an unsaved record reached the database",
        )
    })
}

/// Add what the company patch `patch` of `holder` does to `out`
/// ([`crate::company_dependent::delta`]: `[value]` sets a key, `[]` removes it).
///
/// # Errors
/// `Programming` for a patch that is not a many2one company patch.
pub(crate) fn patch_changes(holder: RecId, patch: &Cell, out: &mut Changes) -> Result<()> {
    let Cell::Json(patch) = patch else {
        return Err(shape("patch"));
    };
    let Json::Object(patch) = patch.as_ref() else {
        return Err(shape("patch"));
    };
    for (key, operation) in patch {
        let Json::Array(operation) = operation else {
            return Err(shape("patch"));
        };
        let target = match operation.as_slice() {
            [] => None,
            [value] => target_of(value)?,
            _ => return Err(shape("patch")),
        };
        let (holder, company) = (holder_id(holder)?, company_of(key)?);
        match target {
            Some(target) => out.set.push((holder, company, target)),
            None => out.cleared.push((holder, company)),
        }
    }
    Ok(())
}

/// Add the entries of the stored company map `map` of the new row `holder` that name an id to
/// `out` (the map an `INSERT` carries; a new row has no link row to remove). SQL NULL is the
/// empty map.
///
/// # Errors
/// `Programming` for a cell that is not a many2one company map.
pub(crate) fn map_changes(holder: RecId, map: &Cell, out: &mut Changes) -> Result<()> {
    let map = match map {
        Cell::Null => return Ok(()),
        Cell::Json(map) => map,
        _ => return Err(shape("map")),
    };
    let Json::Object(map) = map.as_ref() else {
        return Err(shape("map"));
    };
    for (key, value) in map {
        if let Some(target) = target_of(value)? {
            out.set.push((holder_id(holder)?, company_of(key)?, target));
        }
    }
    Ok(())
}

/// The writer's statement for the target table `table` (already quoted). `$1`, `$2`, `$3`: holder
/// ids, company ids and target ids of the entries set, aligned; `$4`: comodel; `$5`: holder model;
/// `$6`: field; `$7`, `$8`: holder ids and company ids of the entries cleared, aligned.
///
/// - `locked` takes `FOR KEY SHARE` on the targets this snapshot shows, in id order. The guard
///   insert reads its row count so the lock is taken whatever the planner does with the rest.
/// - Every target gets its guard row, also one the snapshot does not show: it may have been
///   created after the snapshot, and a deleter that sees it must still meet this reference.
/// - The link is rewritten even when it already names the target: a new row version is what a
///   deleter with an older snapshot conflicts with.
fn writer_sql(table: &str) -> String {
    format!(
        "WITH entry AS (\
             SELECT * FROM unnest($1::int8[], $2::int8[], $3::int8[]) AS e(holder_id, company_id, target_id)\
         ), locked AS (\
             SELECT t.\"id\" FROM {table} AS t WHERE t.\"id\" = ANY($3::int8[]) ORDER BY t.\"id\" FOR KEY SHARE\
         ), guarded AS (\
             INSERT INTO erp_company_ref_guard (comodel, target_id) \
             SELECT DISTINCT $4::text, entry.target_id FROM entry \
             WHERE (SELECT count(*) FROM locked) >= 0 \
             ORDER BY entry.target_id \
             ON CONFLICT DO NOTHING\
         ), cleared AS (\
             DELETE FROM erp_company_ref_link AS link \
             USING unnest($7::int8[], $8::int8[]) AS c(holder_id, company_id) \
             WHERE link.holder_model = $5::text AND link.holder_id = c.holder_id \
               AND link.field = $6::text AND link.company_id = c.company_id\
         ) \
         INSERT INTO erp_company_ref_link (holder_model, holder_id, field, company_id, comodel, target_id) \
         SELECT $5::text, entry.holder_id, $6::text, entry.company_id, $4::text, entry.target_id \
         FROM entry \
         ON CONFLICT (holder_model, holder_id, field, company_id) \
         DO UPDATE SET comodel = EXCLUDED.comodel, target_id = EXCLUDED.target_id"
    )
}

impl Transaction {
    /// The writer's side for what a statement does to `field` (a field [`guards`] accepts) of
    /// `holder`: one statement, in the transaction that writes the holder rows. It waits for a
    /// deletion of a target in flight (the caller's `lock_timeout` bounds the wait).
    ///
    /// # Errors
    /// `Concurrency` (`40001`) when a target was deleted, or its first guard row committed, after
    /// this transaction's snapshot; the statement's errors otherwise.
    pub(crate) async fn guard_company_references(
        &mut self,
        holder: &ModelSpec,
        field: &FieldSpec,
        changes: &Changes,
    ) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let comodel = field.ty.comodel().ok_or_else(|| {
            Error::programming(format!(
                "{}.{}: a guarded company-dependent field has no comodel",
                holder.name, field.name
            ))
        })?;
        let sql = {
            let target = self
                .registry()
                .model(comodel)
                .ok_or_else(|| Error::programming(format!("unknown property comodel {comodel}")))?;
            writer_sql(&erp_sql::quote(&target.table))
        };
        let holders: Vec<i64> = changes.set.iter().map(|entry| entry.0).collect();
        let companies: Vec<i64> = changes.set.iter().map(|entry| entry.1).collect();
        let targets: Vec<i64> = changes.set.iter().map(|entry| entry.2).collect();
        let cleared_holders: Vec<i64> = changes.cleared.iter().map(|entry| entry.0).collect();
        let cleared_companies: Vec<i64> = changes.cleared.iter().map(|entry| entry.1).collect();
        self.execute_orm(
            &sql,
            &[
                &holders,
                &companies,
                &targets,
                &comodel,
                &holder.name,
                &field.name,
                &cleared_holders,
                &cleared_companies,
            ],
        )
        .await?;
        Ok(())
    }
}

const FORGET_HOLDERS: &str = "DELETE FROM erp_company_ref_link WHERE holder_model = $1::text AND holder_id = ANY($2::int8[])";

/// The link rows of holder rows about to be deleted, when `spec` has a field [`guards`] accepts:
/// their references end with them. Holder rows a database cascade deletes keep their link rows
/// until the target goes.
///
/// # Errors
/// The statement's errors.
pub(crate) async fn forget_holders(
    tx: &mut Transaction,
    spec: &ModelSpec,
    ids: &[RecId],
) -> Result<()> {
    if ids.is_empty() || !spec.fields.iter().any(guards) {
        return Ok(());
    }
    let values: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    tx.execute_orm(FORGET_HOLDERS, &[&spec.name, &values])
        .await?;
    Ok(())
}

/// The deleter's first step, before the refusals: when `guarded` (the model is the comodel of a
/// field [`guards`] accepts), lock the rows `ids` of `spec` `FOR UPDATE`, in id order. Writers of
/// a reference to one of them that are in flight finish first; later ones wait for this
/// transaction.
///
/// # Errors
/// `Concurrency` (`40001`) when a row changed after the snapshot, as the `DELETE` itself would
/// raise; the statement's errors otherwise.
pub(crate) async fn lock_targets(
    tx: &mut Transaction,
    spec: &ModelSpec,
    ids: &[RecId],
    guarded: bool,
) -> Result<()> {
    if !guarded || ids.is_empty() {
        return Ok(());
    }
    // one row back, however many are locked
    let sql = format!(
        "SELECT count(*) FROM (SELECT \"id\" FROM {} WHERE \"id\" = ANY($1::int8[]) \
         ORDER BY \"id\" FOR UPDATE) AS locked",
        erp_sql::quote(&spec.table)
    );
    let values: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    tx.query(&sql, &[&values]).await?;
    Ok(())
}

const CLAIM_GUARDS: &str = "INSERT INTO erp_company_ref_guard (comodel, target_id) \
     SELECT $1::text, target FROM unnest($2::int8[]) AS t(target) ORDER BY target \
     ON CONFLICT DO NOTHING";
const DROP_GUARDS: &str =
    "DELETE FROM erp_company_ref_guard WHERE comodel = $1::text AND target_id = ANY($2::int8[])";

/// The deleter's last step, after the refusals passed and before the `DELETE` of the rows: when
/// `guarded`, the guard rows of `ids` are inserted where missing and then deleted, with their link
/// rows. Both statements are how the deleter learns of a reference committed after its snapshot
/// (the module documentation); without one they only remove the rows.
///
/// # Errors
/// `Concurrency` (`40001`) as above; the statements' errors otherwise.
pub(crate) async fn retire(
    tx: &mut Transaction,
    spec: &ModelSpec,
    ids: &[RecId],
    guarded: bool,
) -> Result<()> {
    if !guarded || ids.is_empty() {
        return Ok(());
    }
    let values: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    tx.execute_orm(CLAIM_GUARDS, &[&spec.name, &values]).await?;
    tx.execute_orm(DROP_GUARDS, &[&spec.name, &values]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use std::sync::Arc;

    use erp_fields::ErrorKind;
    use serde_json::json;

    use super::*;

    fn cell(value: serde_json::Value) -> Cell {
        Cell::Json(Arc::new(value))
    }
    fn holder() -> RecId {
        RecId::from_real(7).unwrap()
    }

    #[test]
    fn patch_changes_separates_ids_set_from_entries_cleared() {
        let mut out = Changes::default();
        let patch = cell(json!({"1": [5], "2": [], "3": [null], "4": [9]}));
        patch_changes(holder(), &patch, &mut out).unwrap();
        out.set.sort_unstable();
        out.cleared.sort_unstable();
        assert_eq!(out.set, vec![(7, 1, 5), (7, 4, 9)]);
        assert_eq!(out.cleared, vec![(7, 2), (7, 3)]);
        assert!(!out.is_empty());
        assert!(Changes::default().is_empty());
    }

    #[test]
    fn map_changes_keeps_the_ids_a_map_names() {
        let mut out = Changes::default();
        map_changes(holder(), &Cell::Null, &mut out).unwrap();
        assert!(out.is_empty());
        map_changes(holder(), &cell(json!({"2": 8, "3": null})), &mut out).unwrap();
        assert_eq!(out.set, vec![(7, 2, 8)]);
        assert!(out.cleared.is_empty());
    }

    #[test]
    fn other_shapes_are_refused() {
        const PATCH: &str =
            "company-dependent many2one patch: not a map of company ids to record ids";
        const VALUE: &str =
            "company-dependent many2one value: not a map of company ids to record ids";
        const KEY: &str =
            "company-dependent many2one company key: not a map of company ids to record ids";
        let cases = [
            (json!([1]), PATCH),
            (json!({"1": 5}), PATCH),
            (json!({"1": [5, 6]}), PATCH),
            (json!({"1": ["5"]}), VALUE),
            (json!({"1": [0]}), VALUE),
            (json!({"01": [5]}), KEY),
            (json!({"-1": [5]}), KEY),
            (json!({"x": []}), KEY),
        ];
        for (patch, message) in cases {
            let error = patch_changes(holder(), &cell(patch), &mut Changes::default()).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Programming);
            assert_eq!(error.message(), message);
        }
        let error =
            map_changes(holder(), &cell(json!({"1": [5]})), &mut Changes::default()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Programming);
        assert_eq!(error.message(), VALUE);
        let error = map_changes(holder(), &Cell::Int(1), &mut Changes::default()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Programming);
        assert_eq!(
            error.message(),
            "company-dependent many2one map: not a map of company ids to record ids"
        );
    }

    #[test]
    fn an_unsaved_holder_is_refused() {
        let unsaved = RecId::new_unique(1).unwrap();
        let error =
            patch_changes(unsaved, &cell(json!({"1": [5]})), &mut Changes::default()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Programming);
        assert_eq!(
            error.message(),
            "a company-dependent reference of an unsaved record reached the database"
        );
    }
}
