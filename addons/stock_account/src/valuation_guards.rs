// SPDX-License-Identifier: MIT OR Apache-2.0
//! Generic ORM entry points on valuation models. Mutations require a dedicated engine operation.
use erp_sdk::Operation;
use erp_sdk::prelude::*;

async fn engine_only<T>(records: &Records, tx: &mut Transaction, ops: &[Operation]) -> Result<T> {
    for op in ops {
        records.check_access(tx, *op).await?;
    }
    Err(Error::user(format!(
        "Only the stock valuation engine can change {}.",
        records.model_name()
    )))
}

// The built-in upsert and copy routes have independent method chains. Guard each route
// before resolving input or defaults, even for an otherwise authorized superuser.
macro_rules! protected_model {
    ($module:ident, $source:ident, $ty:ident) => {
        mod $module {
            use super::*;
            use crate::$source::$ty;
            #[methods($ty, addon = "stock_account")]
            impl $ty {
                #[method(override)]
                async fn create(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _vals_list: Vec<RawVals>,
                    _sup: Super,
                ) -> Result<Records> {
                    engine_only(this.as_records(), tx, &[Operation::Create]).await
                }
                #[method(override)]
                async fn write(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _vals: RawVals,
                    _sup: Super,
                ) -> Result<()> {
                    engine_only(this.as_records(), tx, &[Operation::Write]).await
                }
                #[method(override)]
                async fn upsert(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _call: erp_sdk::UpsertCall,
                    _sup: Super,
                ) -> Result<erp_sdk::UpsertResult> {
                    engine_only(
                        this.as_records(),
                        tx,
                        &[Operation::Create, Operation::Write],
                    )
                    .await
                }
                #[method(override)]
                async fn copy(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _default: Option<erp_sdk::ValueMap>,
                    _sup: Super,
                ) -> Result<Records> {
                    engine_only(this.as_records(), tx, &[Operation::Create]).await
                }
                #[method(override)]
                async fn unlink(
                    this: &RecordSet<$ty>,
                    tx: &mut Transaction,
                    _sup: Super,
                ) -> Result<()> {
                    engine_only(this.as_records(), tx, &[Operation::Unlink]).await
                }
            }
        }
    };
}

protected_model!(source, valuation_01_source, ValuationSource);
protected_model!(allocation, valuation_01_source, ValuationSourceAlloc);
protected_model!(receipt, valuation_01_source, ValuationReceipt);
protected_model!(journal, valuation_02_journal, ValuationJournal);
protected_model!(remainder, valuation_02_journal, ValuationRemainder);
protected_model!(state, valuation_03_state, ValuationState);
protected_model!(control, valuation_03_state, ValuationControl);
protected_model!(checkpoint, valuation_03_state, ValuationCheckpoint);
protected_model!(layer, valuation_04_layer, ValuationLayer);
protected_model!(consumption, valuation_04_layer, ValuationLayerConsumption);
