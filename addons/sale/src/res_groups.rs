// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Sales feature groups join the light-group classification of base, catalog and inventory.

use erp_addon_base::ResGroups;
use erp_sdk::prelude::*;

pub const FEATURE_GROUPS: [&str; 5] = [
    "sale.group_auto_done_setting",
    "sale.group_discount_per_so_line",
    "sale.group_warning_sale",
    "sale.group_proforma_sales",
    "sale.group_services_and_material",
];

#[extend(ResGroups, addon = "sale")]
struct SaleGroups {}

#[methods(ResGroups, addon = "sale")]
impl SaleGroups {
    #[method(override)]
    async fn _get_light_group_xmlids(
        this: &RecordSet<ResGroups>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<Vec<String>> {
        let mut groups: Vec<String> = sup.call(tx, this.as_records(), ()).await?;
        for xmlid in FEATURE_GROUPS {
            if !groups.iter().any(|id| id == xmlid) {
                groups.push(xmlid.to_owned());
            }
        }
        Ok(groups)
    }

    /// Run after the security data publishes the group identities, on install and upgrade.
    /// Data-callable only; no RPC method and no permission.
    #[method]
    #[data_callable(model)]
    async fn _sale_reclassify_light_groups(
        this: &RecordSet<ResGroups>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records()
            .call::<(), ()>(tx, "_apply_group_regular", ())
            .await
    }
}
