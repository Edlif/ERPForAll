// SPDX-License-Identifier: MIT OR Apache-2.0
//! Overview card of an operation type (dashboard card contract v1): built in batch from the
//! counter fields of `operation_type_counters.rs`, so it adds no query of its own and keeps the
//! reader's access. Each stat's domain is the exact condition its counter counts, so opening the
//! list shows the number on the card. The field is declared on the model (`warehouse.rs`): a
//! same-addon `#[extend]` in a file that sorts before the model's would compose before it exists.
use crate::warehouse::StockPickingType;
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};

/// The transfers action every link of the card opens.
const TRANSFERS: &str = "stock.action_picking_tree_all";
/// Card wording in one place. Server-side code strings have no translation mechanism yet, so these
/// are source strings (no code-term catalogue exists in the addon set).
const LABEL_READY: &str = "To Process";
const LABEL_WAITING: &str = "Waiting";
const LABEL_LATE: &str = "Late";
const LABEL_BACKORDERS: &str = "Backorders";
const LABEL_NEW: &str = "New Transfer";
/// The states the counters treat as open work.
const OPEN: [&str; 3] = ["assigned", "waiting", "confirmed"];

fn stat(label: &str, count: i32, id: i64, condition: Vec<Json>, tone: &str) -> Json {
    let mut domain = vec![json!(["picking_type_id", "=", id])];
    domain.extend(condition);
    json!({
        "label": label,
        "count": count,
        "amount": null,
        "currency_id": null,
        "action": TRANSFERS,
        "domain": domain,
        "context": {},
        "tone": tone,
    })
}

fn tone_above_zero(count: i32, tone: &'static str) -> &'static str {
    if count > 0 { tone } else { "neutral" }
}

#[methods(StockPickingType, addon = "stock")]
impl StockPickingType {
    #[compute(depends=["company_id.tz","picking_ids.state","picking_ids.scheduled_date","picking_ids.has_deadline_issue","picking_ids.backorder_id"], depends_context=["uid","allowed_company_ids"])]
    async fn _compute_dashboard_card(
        this: &RecordSet<StockPickingType>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockPickingType>,
    ) -> Result<()> {
        if this.is_empty() {
            return Ok(());
        }
        // One batched counter compute serves all four reads.
        let ready = this
            .mapped(tx, StockPickingType::COUNT_PICKING_READY)
            .await?;
        let waiting = this
            .mapped(tx, StockPickingType::COUNT_PICKING_WAITING)
            .await?;
        let late = this
            .mapped(tx, StockPickingType::COUNT_PICKING_LATE)
            .await?;
        let backorders = this
            .mapped(tx, StockPickingType::COUNT_PICKING_BACKORDERS)
            .await?;
        // Same cut-off as the late counter (operation_type_counters.rs): today's midnight.
        let cutoffs = crate::operation_type_counters::local_cutoffs(this, tx).await?;
        let open = json!(["state", "in", OPEN]);
        for (i, id) in this.as_records().ids().iter().copied().enumerate() {
            let card = json!({
                "v": 1,
                "buttons": [{
                    "label": LABEL_NEW,
                    "action": TRANSFERS,
                    "view_mode": "form",
                    "context": {"default_picking_type_id": id},
                    "primary": true,
                }],
                "stats": [
                    stat(LABEL_READY, ready[i], id, vec![json!(["state", "=", "assigned"])], "neutral"),
                    stat(
                        LABEL_WAITING,
                        waiting[i],
                        id,
                        vec![json!(["state", "in", ["confirmed", "waiting"]])],
                        "neutral",
                    ),
                    stat(
                        LABEL_LATE,
                        late[i],
                        id,
                        vec![
                            open.clone(),
                            json!("|"),
                            json!(["scheduled_date", "<", cutoffs[i].1.to_string()]),
                            json!(["has_deadline_issue", "=", true]),
                        ],
                        tone_above_zero(late[i], "danger"),
                    ),
                    stat(
                        LABEL_BACKORDERS,
                        backorders[i],
                        id,
                        vec![open.clone(), json!(["backorder_id", "!=", false])],
                        tone_above_zero(backorders[i], "warning"),
                    ),
                ],
                "chart": null,
            });
            out.set(i, StockPickingType::DASHBOARD_CARD, card.to_string())?;
        }
        Ok(())
    }
}
