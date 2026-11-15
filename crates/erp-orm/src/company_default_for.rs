// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company fields that mirror a company-dependent default (supplement
//! of 2026-11-15).
//!
//! A `res.company` field declared `company_default_for = "<model>.<field>"` holds no data: the
//! company-scoped `ir.default` row of the target field is its only source.
//!
//! - **Read** ([`Transaction::company_default_cells`]): the company of each value is the id of
//!   the `res.company` row being read, which the caller already passed its read checks on. The
//!   defaults of the target are loaded for those rows in one statement, with the property lookup
//!   identity (the caller's rights are not raised), and converted as the
//!   target's own fallback is. Nothing is cached for a database row, so a change of `ir.default`
//!   is seen by the next read.
//! - **Write** ([`Transaction::store_company_defaults`]): the value becomes
//!   `ir.default(target, user = none, company = written row, condition = none)` in superuser
//!   mode, after the caller's write checks on the company rows. Never global, never per user,
//!   never another company.
//! - **Declared default** ([`apply_declared_defaults`]): written once for a company that has no
//!   company-scoped row for the target.
//!
//! Searching, sorting and grouping on such a field refuse by name
//! ([`erp_sql::company::default_for_unsupported`]) until the SQL half is delivered.

use erp_fields::{Cell, Error, RecId, Result, Value};
use erp_registry::{FieldSpec, ModelSpec};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::company_dependent::MAX_COMPANIES;
use crate::{Env, Records, Transaction};

/// The model whose fields may mirror a company default; the registry refuses any other owner.
const COMPANY_MODEL: &str = "res.company";

/// The target model and field of a company-default field.
fn target_of<'r>(
    env: &'r Env,
    owner: &ModelSpec,
    field: &FieldSpec,
) -> Result<(&'r ModelSpec, &'r FieldSpec)> {
    let target = field.company_default_for.ok_or_else(|| {
        Error::programming(format!(
            "{}.{} does not mirror a company default",
            owner.name, field.name
        ))
    })?;
    let model = env.registry().model_by_id(target.model);
    Ok((model, model.field_by_id(target.field)))
}

/// Refuse more company rows in one preparation than a company map may hold.
fn check_bound(owner: &ModelSpec, field: &FieldSpec, companies: usize) -> Result<()> {
    if companies > MAX_COMPANIES {
        return Err(Error::validation(format!(
            "{}.{}: {companies} companies in one request exceed the maximum of {MAX_COMPANIES} \
             (MAX_COMPANIES) for a field that mirrors a company default",
            owner.name, field.name
        )));
    }
    Ok(())
}

/// The value stored in `ir.default` for a written cell: an empty value is an explicit JSON
/// `false`, which keeps a global default from showing through.
fn default_value(cell: &Cell) -> Value {
    match cell {
        Cell::Null => Value::False,
        other => Value::from_cell(other),
    }
}

fn decode<T: for<'a> tokio_postgres::types::FromSql<'a>>(
    row: &tokio_postgres::Row,
    index: usize,
) -> Result<T> {
    row.try_get(index).map_err(|error| {
        Error::new(
            erp_fields::ErrorKind::Database,
            "could not decode a company default row",
        )
        .with_source(error)
    })
}

impl Transaction {
    /// The values of company-default `field` for `records` (rows of `res.company`), one per
    /// record in order.
    ///
    /// A database row projects the default of the target for that row's id: the most specific
    /// `ir.default` row among the superuser's, the company's and the global ones, as the target
    /// field's own fallback resolves it. A missing or JSON `false` default reads the kind's empty
    /// value; a many2one default whose record is gone reads empty. An in-memory record reads the
    /// value assigned to it, else the value of its origin row, else the declared default.
    ///
    /// # Errors
    ///
    /// `Validation` above `MAX_COMPANIES` rows; the conversion errors of an invalid stored
    /// default; database errors.
    pub(crate) async fn company_default_cells(
        &mut self,
        records: &Records,
        field: &FieldSpec,
    ) -> Result<Vec<Cell>> {
        let env = records.env().clone();
        let owner = records.spec();
        let (target_model, target_field) = target_of(&env, owner, field)?;
        // the company of each record: its own id, or its origin's for an in-memory record
        let mut companies: Vec<i64> = Vec::new();
        let mut seen: FxHashSet<i64> = FxHashSet::default();
        for id in records.rec_ids() {
            let company = if id.is_new() {
                if self.cache.contains(owner.id, field.id, *id) {
                    continue;
                }
                match id.origin() {
                    Some(origin) => origin.get(),
                    None => continue,
                }
            } else {
                id.get()
            };
            if seen.insert(company) {
                companies.push(company);
            }
        }
        check_bound(owner, field, companies.len())?;
        let by_company = self
            .company_defaults_of(&env, target_model, target_field, &companies)
            .await?;
        let mut out = Vec::with_capacity(records.len());
        for id in records.rec_ids() {
            let company = if id.is_new() {
                if let Some(cell) = self.cache.get(owner.id, field.id, *id) {
                    out.push(cell);
                    continue;
                }
                match id.origin() {
                    Some(origin) => origin.get(),
                    None => {
                        out.push(
                            crate::write::default_cell(records, owner, field)?
                                .unwrap_or(Cell::Null),
                        );
                        continue;
                    }
                }
            } else {
                id.get()
            };
            out.push(by_company.get(&company).cloned().unwrap_or(Cell::Null));
        }
        Ok(out)
    }

    /// The default of `target_field` for each of `companies`, as fallback cells. One statement
    /// for the defaults and one for the existence of many2one targets.
    async fn company_defaults_of(
        &mut self,
        env: &Env,
        target_model: &ModelSpec,
        target_field: &FieldSpec,
        companies: &[i64],
    ) -> Result<FxHashMap<i64, Cell>> {
        let mut out: FxHashMap<i64, Cell> = FxHashMap::default();
        if companies.is_empty() {
            return Ok(out);
        }
        let mut values: FxHashMap<i64, Value> = FxHashMap::default();
        let mut shared: Option<Value> = None;
        if let Some(defaults) = env.registry().model("ir.default") {
            // staged `ir.default` writes are part of what the next read must see
            Box::pin(self.flush_model(defaults.id)).await?;
            // The property lookup identity for a list of companies: the
            // superuser's rows before the unowned ones, a company's row before the global one.
            let sql = r#"
                SELECT d.company_id::int8, d.json_value
                FROM ir_default d
                JOIN ir_model_fields f ON d.field_id = f.id
                WHERE f.model = $1
                  AND f.name = $2
                  AND (d.user_id IS NULL OR d.user_id = $3)
                  AND (d.company_id IS NULL OR d.company_id = ANY($4::int8[]))
                  AND d.condition IS NULL
                ORDER BY d.user_id, d.company_id, d.id
            "#;
            let uid: i64 = crate::SUPERUSER_ID.into();
            let rows = self
                .query(
                    sql,
                    &[&target_model.name, &target_field.name, &uid, &companies],
                )
                .await?;
            // In that order a company's first row is its answer unless an earlier row of the
            // superuser is global; `rank` keeps the order of the single-company lookup:
            // (user rows first, then company before global).
            let mut ranked: FxHashMap<i64, (usize, Value)> = FxHashMap::default();
            let mut shared_ranked: Option<(usize, Value)> = None;
            for (rank, row) in rows.iter().enumerate() {
                let company: Option<i64> = decode(row, 0)?;
                let text: String = decode(row, 1)?;
                let json: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
                    Error::validation(format!(
                        "Invalid JSON default value for field '{}.{}': {error}",
                        target_model.name, target_field.name
                    ))
                })?;
                let value = crate::ir_default::json_to_value(&json);
                match company {
                    Some(company) => {
                        ranked.entry(company).or_insert((rank, value));
                    }
                    None => {
                        if shared_ranked.is_none() {
                            shared_ranked = Some((rank, value));
                        }
                    }
                }
            }
            for (company, (rank, value)) in ranked {
                match &shared_ranked {
                    // an earlier global row is the superuser's own: it wins, as in the
                    // single-company lookup
                    Some((shared_rank, shared_value)) if *shared_rank < rank => {
                        values.insert(company, shared_value.clone());
                    }
                    _ => {
                        values.insert(company, value);
                    }
                }
            }
            shared = shared_ranked.map(|(_, value)| value);
        }
        for company in companies {
            let value = values.get(company).or(shared.as_ref());
            let cell = self
                .property_fallback_cell(env, target_model, target_field, value)
                .await?;
            out.insert(*company, cell);
        }
        // a many2one default whose record is gone reads empty
        if let Some(comodel) = target_field.ty.comodel() {
            let mut ids: Vec<i64> = out
                .values()
                .filter_map(|cell| match cell {
                    Cell::Id(id) => Some(*id),
                    _ => None,
                })
                .collect();
            ids.sort_unstable();
            ids.dedup();
            if !ids.is_empty() {
                let live = self.existing_property_targets(env, comodel, &ids).await?;
                for cell in out.values_mut() {
                    if matches!(cell, Cell::Id(id) if !live.contains(id)) {
                        *cell = Cell::Null;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Store `cell` as the default of the target of company-default `field` for each company
    /// row of `ids`: `ir.default(target, user = none, company = row, condition = none)`.
    ///
    /// Superuser mode, scoped to the declared target and the written rows only. The caller of
    /// the write or create has already passed model access, record rules and field groups on
    /// those `res.company` rows; this function checks none of them and must only be reached
    /// from the write and create paths and from [`apply_declared_defaults`].
    ///
    /// # Errors
    ///
    /// `Programming` for an id that is not a database row; the errors of `ir_default::set`.
    pub(crate) async fn store_company_defaults(
        &mut self,
        env: &Env,
        owner: &ModelSpec,
        field: &FieldSpec,
        cell: &Cell,
        ids: &[RecId],
    ) -> Result<()> {
        let (target_model, target_field) = target_of(env, owner, field)?;
        if owner.name != COMPANY_MODEL {
            return Err(Error::programming(format!(
                "{}.{}: a company default is written from {COMPANY_MODEL} rows only",
                owner.name, field.name
            )));
        }
        check_bound(owner, field, ids.len())?;
        if let Some(id) = ids.iter().find(|id| id.is_new()) {
            return Err(Error::programming(format!(
                "{}.{}: the company default of an in-memory record ({}) cannot be stored",
                owner.name,
                field.name,
                id.get()
            )));
        }
        let value = default_value(cell);
        // the engine's own step: the scope is (declared target, company = written row)
        let elevated = env.engine_sudo();
        for id in ids {
            Box::pin(crate::ir_default::set(
                self,
                &elevated,
                target_model.name,
                target_field.name,
                &value,
                None,
                Some(*id),
                None,
            ))
            .await?;
        }
        Ok(())
    }

    /// The companies of `ids` without a company-scoped `ir.default` row (no user, no condition)
    /// for the target field. A global row does not count.
    async fn companies_without_default(
        &mut self,
        target_model: &ModelSpec,
        target_field: &FieldSpec,
        ids: &[i64],
    ) -> Result<Vec<i64>> {
        let sql = r#"
            SELECT d.company_id::int8
            FROM ir_default d
            JOIN ir_model_fields f ON d.field_id = f.id
            WHERE f.model = $1
              AND f.name = $2
              AND d.user_id IS NULL
              AND d.condition IS NULL
              AND d.company_id = ANY($3::int8[])
        "#;
        let rows = self
            .query(sql, &[&target_model.name, &target_field.name, &ids])
            .await?;
        let mut have: FxHashSet<i64> = FxHashSet::default();
        for row in &rows {
            have.insert(decode(row, 0)?);
        }
        Ok(ids
            .iter()
            .copied()
            .filter(|id| !have.contains(id))
            .collect())
    }
}

/// Write the declared default of every company-default field for each company of `companies`
/// that has no default of its own yet.
///
/// For every field of `res.company` declared `company_default_for = "<model>.<field>"` with a
/// static `default`, each given company (archived ones included) without a company-scoped
/// `ir.default` row for the target gets that default written for it. The test is an exact-scope
/// lookup: a global default does not count, and an existing company row, also an explicit empty
/// one, is kept. Calling it again changes nothing.
///
/// The base addon calls it from `res.company`'s `init` hook (all companies) and after `create`
/// (the new rows); an addon needs no call of its own. The rows are written in superuser mode,
/// scoped to the declared targets and the given companies: the caller must be allowed to
/// install a module or to have created those companies.
///
/// # Errors
///
/// `Programming` when `companies` is not a `res.company` recordset or holds an in-memory
/// record; `Validation` above `MAX_COMPANIES` companies; database errors and the errors of
/// `ir_default::set`.
pub async fn apply_declared_defaults(tx: &mut Transaction, companies: &Records) -> Result<()> {
    let env = companies.env().clone();
    let owner = companies.spec();
    if owner.name != COMPANY_MODEL {
        return Err(Error::programming(format!(
            "apply_declared_defaults takes {COMPANY_MODEL} records, not {}",
            owner.name
        )));
    }
    if companies.is_empty() || !crate::ir_default::is_available(env.registry()) {
        return Ok(());
    }
    let registry = std::sync::Arc::clone(env.registry());
    let owner = registry.model_by_id(owner.id);
    let mut ids: Vec<i64> = Vec::with_capacity(companies.len());
    for id in companies.rec_ids() {
        if id.is_new() {
            return Err(Error::programming(format!(
                "apply_declared_defaults: {COMPANY_MODEL} record {} is not in the database",
                id.get()
            )));
        }
        if !ids.contains(&id.get()) {
            ids.push(id.get());
        }
    }
    for field in owner
        .fields
        .iter()
        .filter(|f| f.company_default_for.is_some() && f.inherited.is_none())
    {
        check_bound(owner, field, ids.len())?;
        let Some(erp_registry::FieldDefault::Value(default)) = &field.default else {
            // no declared default, or a function of the environment: nothing to seed
            continue;
        };
        let cell = erp_fields::convert::to_cell(
            erp_fields::convert::FieldRef {
                model: owner.name,
                field: field.name,
                label: &field.string,
            },
            &field.ty,
            &default.to_value(),
        )?;
        let (target_model, target_field) = target_of(&env, owner, field)?;
        // `ir.default` rows staged through its own model count
        if let Some(defaults) = registry.model("ir.default") {
            Box::pin(tx.flush_model(defaults.id)).await?;
        }
        let missing = tx
            .companies_without_default(target_model, target_field, &ids)
            .await?;
        if missing.is_empty() {
            continue;
        }
        let missing: Vec<RecId> = missing
            .into_iter()
            .map(|id| {
                RecId::from_real(id).ok_or_else(|| {
                    Error::programming(format!("{COMPANY_MODEL} id {id} is not a database id"))
                })
            })
            .collect::<Result<_>>()?;
        tx.store_company_defaults(&env, owner, field, &cell, &missing)
            .await?;
    }
    Ok(())
}
