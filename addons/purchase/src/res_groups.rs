// SPDX-License-Identifier: MIT OR Apache-2.0
//! Purchase feature identities compose with the base light-group classification.

use erp_addon_base::ResGroups;
use erp_sdk::prelude::*;

#[extend(ResGroups, addon = "purchase")]
struct PurchaseGroups {}

#[methods(ResGroups, addon = "purchase")]
impl PurchaseGroups {
    #[method(override)]
    async fn _get_light_group_xmlids(
        this: &RecordSet<ResGroups>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<Vec<String>> {
        let mut groups: Vec<String> = sup.call(tx, this.as_records(), ()).await?;
        // Structural feature group identity; administrators manage membership on the group record.
        if !groups.iter().any(|id| id == crate::WARNING_GROUP) {
            groups.push(crate::WARNING_GROUP.to_owned());
        }
        Ok(groups)
    }

    /// Reclassify after the manifest has published the purchase feature group identity, including
    /// upgrades. This installation service exposes no RPC method and grants no permissions.
    #[method]
    #[data_callable(model)]
    async fn _purchase_reclassify_light_groups(
        this: &RecordSet<ResGroups>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records()
            .call::<(), ()>(tx, "_apply_group_regular", ())
            .await
    }
}
