// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exact-company key and revision persistence. The company generation serializes publishers.

use erp_sdk::ErrorKind;
use erp_sdk::prelude::*;

use super::decode;

pub(super) async fn resolve(
    tx: &mut Transaction,
    company: i64,
    role: i64,
    date: Date,
) -> Result<Option<i64>> {
    let rows = tx
        .query(
            "SELECT s.account_id FROM account_default_key k JOIN account_default_segment s \
             ON s.key_id = k.id AND s.company_id = k.company_id \
             WHERE k.company_id = $1 AND k.role_id = $2 AND s.effective_from <= $3 \
             ORDER BY s.effective_from DESC, s.generation DESC LIMIT 1",
            &[&company, &role, &date],
        )
        .await?;
    match rows.first() {
        Some(row) => row.try_get(0).map_err(decode),
        None => Ok(None),
    }
}

pub(super) struct GenerationWriter {
    company: i64,
    generation: i64,
    owns_generation: bool,
}

/// The chart's first seed or the first manual configuration for an uncharted company creates
/// the fence. This transaction holds it through commit or rollback.
pub(super) async fn lock_generation(
    tx: &mut Transaction,
    company: i64,
) -> Result<GenerationWriter> {
    tx.execute(
        "INSERT INTO account_default_generation (company_id, generation) VALUES ($1, 0) \
         ON CONFLICT (company_id) DO NOTHING",
        &[&company],
    )
    .await?;
    let rows = tx
        .query(
            "SELECT generation, writer_xid IS NOT DISTINCT FROM pg_current_xact_id() \
             FROM account_default_generation WHERE company_id = $1 FOR UPDATE",
            &[&company],
        )
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| Error::invariant("Company accounting generation is missing"))?;
    Ok(GenerationWriter {
        company,
        generation: row.try_get(0).map_err(decode)?,
        owns_generation: row.try_get(1).map_err(decode)?,
    })
}

pub(super) struct Key {
    id: i64,
    version: i64,
}

pub(super) async fn key_for_write(
    tx: &mut Transaction,
    company: i64,
    role: i64,
) -> Result<Option<Key>> {
    let rows = tx
        .query(
            "SELECT id, version FROM account_default_key \
             WHERE company_id = $1 AND role_id = $2 FOR UPDATE",
            &[&company, &role],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Key {
        id: row.try_get(0).map_err(decode)?,
        version: row.try_get(1).map_err(decode)?,
    }))
}

impl GenerationWriter {
    pub(super) async fn advance(&mut self, tx: &mut Transaction) -> Result<()> {
        if self.owns_generation {
            return Ok(());
        }
        let next = self.generation.checked_add(1).ok_or_else(|| {
            Error::new(
                ErrorKind::ResourceExhausted,
                "Accounting configuration generation exhausted",
            )
        })?;
        tx.execute(
            "UPDATE account_default_generation SET generation = $2, writer_xid = pg_current_xact_id() \
             WHERE company_id = $1",
            &[&self.company, &next],
        )
        .await?;
        self.generation = next;
        Ok(())
    }

    pub(super) async fn write(
        &self,
        tx: &mut Transaction,
        role: i64,
        key: Option<Key>,
        date: Date,
        account: Option<i64>,
    ) -> Result<()> {
        let state = if account.is_some() {
            "account"
        } else {
            "empty"
        };
        let key_id = if let Some(key) = key {
            // An earlier call in this transaction may have inserted a revision for this key.
            // Keep its original start and version. Committed rows are never rewritten here.
            if self.owns_generation {
                let replaced = tx
                    .execute(
                        "UPDATE account_default_segment SET account_id = $3, state = $4 \
                         WHERE key_id = $1 AND generation = $2",
                        &[&key.id, &self.generation, &account, &state],
                    )
                    .await?;
                if replaced == 1 {
                    return Ok(());
                }
                if replaced > 1 {
                    return Err(Error::validation("Duplicate accounting mapping segment"));
                }
            }
            let next = key.version.checked_add(1).ok_or_else(|| {
                Error::new(
                    ErrorKind::ResourceExhausted,
                    "Accounting configuration version exhausted",
                )
            })?;
            tx.execute(
                "UPDATE account_default_key SET version = $2 WHERE id = $1",
                &[&key.id, &next],
            )
            .await?;
            key.id
        } else {
            let rows = tx
                .query(
                    "INSERT INTO account_default_key (company_id, role_id, version) \
                     VALUES ($1, $2, 1) RETURNING id",
                    &[&self.company, &role],
                )
                .await?;
            rows.first()
                .ok_or_else(|| Error::invariant("No company default key was inserted"))?
                .try_get(0)
                .map_err(decode)?
        };
        let effective_from = if tx
            .query(
                "SELECT 1 FROM account_default_segment WHERE key_id = $1 LIMIT 1",
                &[&key_id],
            )
            .await?
            .is_empty()
        {
            None
        } else {
            Some(date)
        };
        tx.execute(
            "INSERT INTO account_default_segment              (key_id, company_id, generation, effective_from, account_id, state) \
             VALUES ($1, $2, $3, COALESCE($4::date, '-infinity'::date), $5, $6)",
            &[&key_id, &self.company, &self.generation, &effective_from, &account, &state],
        )
        .await?;
        Ok(())
    }
}
