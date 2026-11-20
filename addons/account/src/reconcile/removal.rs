// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dated undo inputs for historical company-currency residuals.
//! The reconciliation service writes a snapshot before the partial match disappears. Partner and
//! line identities are snapshots so deleting or reassigning a record cannot relabel the fact.

use erp_sdk::prelude::*;

#[model(
    name = "account.reconcile.removal",
    description = "Removed Partial Reconciliation",
    order = "id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        ("_partial_item_unique", "UNIQUE (original_partial_id, line_id)", "A match can be removed only once for each journal item."),
    ],
)]
struct ReconcileRemoval {
    #[field(bigint, required = true, readonly = true)]
    original_partial_id: erp_sdk::BigInt,
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        readonly = true,
        ondelete = "restrict"
    )]
    company_id: Option<RecId>,
    #[field(bigint, required = true, readonly = true)]
    account_id: erp_sdk::BigInt,
    #[field(bigint, required = true, readonly = true)]
    line_id: erp_sdk::BigInt,
    #[field(bigint, readonly = true)]
    partner_id: Option<erp_sdk::BigInt>,
    #[field(
        many2one,
        comodel = "res.currency",
        required = true,
        readonly = true,
        ondelete = "restrict"
    )]
    currency_id: Option<RecId>,
    #[field(
        monetary,
        currency_field = "currency_id",
        required = true,
        readonly = true
    )]
    matched_delta: Decimal,
    #[field(date, required = true, readonly = true)]
    matched_on: Option<Date>,
    #[field(date, required = true, readonly = true)]
    removed_on: Option<Date>,
}

fn service_only() -> Error {
    Error::user("Only unmatching posted journal items creates a reconciliation removal.")
}

// Raw SQL inside the authorized unmatch is the only writer. The independent ORM mutation routes
// also reject superuser calls. No public reporting ACL is granted by this input slice.
#[methods(ReconcileRemoval, addon = "account")]
impl ReconcileRemoval {
    #[method(override)]
    async fn create(
        _this: &RecordSet<ReconcileRemoval>,
        _tx: &mut Transaction,
        _vals_list: Vec<RawVals>,
        _sup: Super,
    ) -> Result<Records> {
        Err(service_only())
    }

    #[method(override)]
    async fn write(
        _this: &RecordSet<ReconcileRemoval>,
        _tx: &mut Transaction,
        _vals: RawVals,
        _sup: Super,
    ) -> Result<()> {
        Err(service_only())
    }

    #[method(override)]
    async fn upsert(
        _this: &RecordSet<ReconcileRemoval>,
        _tx: &mut Transaction,
        _call: erp_sdk::UpsertCall,
        _sup: Super,
    ) -> Result<erp_sdk::UpsertResult> {
        Err(service_only())
    }

    #[method(override)]
    async fn copy(
        _this: &RecordSet<ReconcileRemoval>,
        _tx: &mut Transaction,
        _default: Option<erp_sdk::ValueMap>,
        _sup: Super,
    ) -> Result<Records> {
        Err(service_only())
    }

    #[method(override)]
    async fn unlink(
        _this: &RecordSet<ReconcileRemoval>,
        _tx: &mut Transaction,
        _sup: Super,
    ) -> Result<()> {
        Err(service_only())
    }
}
