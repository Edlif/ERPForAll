// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dropship operation type and transfer dropship indicator.

use erp_addon_stock::{StockPicking, StockPickingType};
use erp_sdk::prelude::*;

#[extend(StockPickingType, addon = "stock_dropshipping")]
pub struct DropshipPickingType {
    #[field(
        selection,
        selection_add = [("dropship", "Dropship")],
        ondelete = [("dropship", "cascade")]
    )]
    pub code: Option<Arc<str>>,
}

#[extend(StockPicking, addon = "stock_dropshipping")]
pub struct DropshipPicking {
    /// True when the transfer moves directly from a vendor to a customer.
    #[field(
        boolean,
        compute = "_compute_is_dropship",
        store = true,
        string = "Is a Dropship"
    )]
    pub is_dropship: bool,
}

#[methods(StockPicking, addon = "stock_dropshipping")]
impl DropshipPicking {
    #[compute(depends = ["location_id.usage", "location_dest_id.usage"])]
    async fn _compute_is_dropship(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockPicking>,
    ) -> Result<()> {
        let sources = this.mapped(tx, StockPicking::LOCATION_ID).await?;
        let destinations = this.mapped(tx, StockPicking::LOCATION_DEST_ID).await?;
        let mut loc_ids = Vec::new();
        for loc in sources.iter().chain(&destinations).flatten() {
            loc_ids.push(loc.get());
        }
        loc_ids.sort_unstable();
        loc_ids.dedup();
        let usage_map: std::collections::HashMap<i64, Arc<str>> = if !loc_ids.is_empty() {
            let loc_records = this.env().records("stock.location")?.browse(loc_ids)?;
            let usages = loc_records.cells(tx, "usage").await?;
            loc_records
                .ids()
                .into_iter()
                .zip(usages)
                .filter_map(|(id, cell)| match cell {
                    Cell::Str(s) => Some((id, s)),
                    _ => None,
                })
                .collect()
        } else {
            std::collections::HashMap::new()
        };

        for (index, (src, dest)) in sources.into_iter().zip(destinations).enumerate() {
            let is_dropship = match (src, dest) {
                (Some(s), Some(d)) => {
                    usage_map.get(&s.get()).map(|v| v.as_ref()) == Some("supplier")
                        && usage_map.get(&d.get()).map(|v| v.as_ref()) == Some("customer")
                }
                _ => false,
            };
            out.set(index, DropshipPicking::IS_DROPSHIP, is_dropship)?;
        }
        Ok(())
    }
}
