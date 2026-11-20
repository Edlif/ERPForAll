// SPDX-License-Identifier: MIT OR Apache-2.0
//! Trusted lower bound for using retained matches to reconstruct historical residuals.
use erp_sdk::prelude::*;

#[model(
    name = "account.reconcile.coverage",
    description = "Reconciliation History Start",
    constraints = [
        ("_singleton", "UNIQUE (name)", "Record reconciliation coverage once."),
        ("_key", "CHECK (name = 'partial_matches')", "Unknown reconciliation coverage series."),
    ],
)]
struct ReconcileCoverage {
    #[field(char, required = true, readonly = true)]
    name: Option<Arc<str>>,
    #[field(date, readonly = true)]
    safe_from: Option<Date>,
}

#[methods(ReconcileCoverage, addon = "account")]
impl ReconcileCoverage {
    /// Fresh installations with no ledger can account for every future match. An upgrade cannot
    /// recover old deletions. Civil dates in all supported zones are at most one day ahead of UTC.
    #[method(override, sealed)]
    async fn init(
        this: &RecordSet<ReconcileCoverage>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        sup.call(tx, this.as_records(), ()).await?;
        tx.execute(include_str!("coverage.sql"), &[]).await?;
        Ok(())
    }

    #[method(override, sealed)]
    async fn create(
        _this: &RecordSet<ReconcileCoverage>,
        _tx: &mut Transaction,
        _vals: Vec<RawVals>,
        _sup: Super,
    ) -> Result<Records> {
        Err(Error::user(
            "Only the accounting lifecycle can establish historical report coverage.",
        ))
    }
    #[method(override, sealed)]
    async fn upsert(
        _this: &RecordSet<ReconcileCoverage>,
        _tx: &mut Transaction,
        _call: erp_sdk::UpsertCall,
        _sup: Super,
    ) -> Result<erp_sdk::UpsertResult> {
        Err(Error::user(
            "Only the accounting lifecycle can establish historical report coverage.",
        ))
    }
    #[method(override, sealed)]
    async fn copy(
        _this: &RecordSet<ReconcileCoverage>,
        _tx: &mut Transaction,
        _defaults: Option<erp_sdk::ValueMap>,
        _sup: Super,
    ) -> Result<Records> {
        Err(Error::user(
            "Only the accounting lifecycle can establish historical report coverage.",
        ))
    }
}
