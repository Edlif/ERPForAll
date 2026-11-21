// SPDX-License-Identifier: MIT OR Apache-2.0
//! Interactive backorder choices are scoped to one real transfer and one validation call.

use std::error::Error as _;

use erp_sdk::ValueMap;
use erp_sdk::prelude::*;

use crate::error::StockError;
use crate::picking::StockPicking;

#[derive(Default)]
pub(super) struct BackorderDecision(pub Option<(i64, bool)>);

fn needs_decision(error: &Error) -> bool {
    matches!(
        error
            .source()
            .and_then(|cause| cause.downcast_ref::<StockError>()),
        Some(StockError::BackorderNotAvailable { .. })
    )
}

async fn submit(records: &Records, tx: &mut Transaction, keep: bool) -> Result<Value> {
    let id = records.ensure_one()?.int()?;
    let previous = tx.extension::<BackorderDecision>().0.replace((id, keep));
    // Dispatch the original method, including installed Valuation and other addon overrides.
    let validated = records.call::<(), ()>(tx, "button_validate", ()).await;
    tx.extension::<BackorderDecision>().0 = previous;
    validated?;
    Ok(Value::Map(
        ValueMap::from([("type".into(), Value::str("ir.actions.act_window_close"))]).into(),
    ))
}

#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    /// Validate normally or open the decision for this one incomplete transfer.
    #[method]
    #[rpc(effect=write)]
    pub async fn action_validate_transfer(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        let id = records.ensure_one()?.int()?;
        match records.call::<(), ()>(tx, "button_validate", ()).await {
            Ok(()) => return Ok(Value::False),
            Err(error) if needs_decision(&error) => {}
            Err(error) => return Err(error),
        }
        let rows = tx.query(
            "SELECT res_id::bigint FROM ir_model_data WHERE model='ir.ui.view' AND module='stock' AND name='view_picking_backorder_choice'",
            &[],
        ).await?;
        let [row] = rows.as_slice() else {
            return Err(Error::invariant("The backorder choice form is missing."));
        };
        let view = row
            .try_get::<_, i64>(0)
            .map_err(|e| Error::db("Cannot read the backorder choice form.").with_source(e))?;
        Ok(Value::Map(
            ValueMap::from([
                ("type".into(), Value::str("ir.actions.act_window")),
                ("name".into(), Value::str("Remaining demand")),
                ("res_model".into(), Value::str("stock.picking")),
                ("res_id".into(), Value::Int(id)),
                (
                    "views".into(),
                    Value::list([Value::list([Value::Int(view), Value::str("form")])]),
                ),
                ("target".into(), Value::str("new")),
            ])
            .into(),
        ))
    }

    /// Validate an incomplete Ask transfer and carry its remaining demand forward.
    #[method]
    #[rpc(effect=write)]
    pub async fn action_keep_backorder(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        submit(this.as_records(), tx, true).await
    }

    /// Validate an incomplete Ask transfer and close its remaining demand.
    #[method]
    #[rpc(effect=write)]
    pub async fn action_close_remainder(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        submit(this.as_records(), tx, false).await
    }
}
