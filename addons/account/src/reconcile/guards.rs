// SPDX-License-Identifier: MIT OR Apache-2.0
//! The generic ORM entry points of the reconciliation rows. Matches, settlements and open items
//! are written only by the reconciliation service ([`super::reconcile_lines`],
//! [`super::unreconcile_partials`]) with its own statements, so every ORM create, write, upsert
//! and copy is refused, for a superuser too; deleting a match is unmatching it (`lines.rs`).
use erp_sdk::Operation;
use erp_sdk::prelude::*;

use super::{AccountFullReconcile, AccountOpenItem, AccountPartialReconcile};

/// Asks the caller's `ops` rights first (a caller without them gets the access refusal), then
/// refuses the change.
async fn service_only<T>(records: &Records, tx: &mut Transaction, ops: &[Operation]) -> Result<T> {
    for op in ops {
        records.check_access(tx, *op).await?;
    }
    Err(Error::user(format!(
        "Only matching and unmatching journal items (Reconcile, Unreconcile) change {}.",
        records.model_name()
    )))
}

// The upsert and copy routes have their own method chains: each is guarded before any input or
// default is resolved, and sealed so no later override reopens it.
macro_rules! service_owned {
    ($module:ident, $ty:ident) => {
        mod $module {
            use super::*;
            #[methods($ty, addon = "account")]
            impl $ty {
                #[method(override, sealed)]
                async fn create(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _vals_list: Vec<RawVals>,
                    _sup: Super,
                ) -> Result<Records> {
                    service_only(this.as_records(), tx, &[Operation::Create]).await
                }
                #[method(override, sealed)]
                async fn write(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _vals: RawVals,
                    _sup: Super,
                ) -> Result<()> {
                    service_only(this.as_records(), tx, &[Operation::Write]).await
                }
                #[method(override, sealed)]
                async fn upsert(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _call: erp_sdk::UpsertCall,
                    _sup: Super,
                ) -> Result<erp_sdk::UpsertResult> {
                    service_only(
                        this.as_records(),
                        tx,
                        &[Operation::Create, Operation::Write],
                    )
                    .await
                }
                #[method(override, sealed)]
                async fn copy(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _default: Option<erp_sdk::ValueMap>,
                    _sup: Super,
                ) -> Result<Records> {
                    service_only(this.as_records(), tx, &[Operation::Create]).await
                }
            }
        }
    };
}

service_owned!(partial, AccountPartialReconcile);
service_owned!(full, AccountFullReconcile);
service_owned!(open_item, AccountOpenItem);

#[methods(AccountOpenItem, addon = "account")]
impl AccountOpenItem {
    /// An open-item row goes with its item's matches: unmatching removes it.
    #[method(override, sealed)]
    async fn unlink(
        this: &RecordSet<AccountOpenItem>,
        tx: &mut Transaction,
        _sup: Super,
    ) -> Result<()> {
        service_only(this.as_records(), tx, &[Operation::Unlink]).await
    }
}
