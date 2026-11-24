// SPDX-License-Identifier: MIT OR Apache-2.0
//! Non-blocking transfer warning messages computed from the transfer's contact and parent company.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_base::warning_text::{non_empty_text, optional_rec_id, readable_ids};
use erp_sdk::prelude::*;

use crate::StockPicking;
use crate::res_partner::WARNING_GROUP;

struct PartnerTransferWarning {
    parent_id: Option<RecId>,
    warn_msg: Option<Arc<str>>,
}

async fn read_partner_warnings(
    env: &Env,
    tx: &mut Transaction,
    ids: Vec<RecId>,
) -> Result<BTreeMap<RecId, PartnerTransferWarning>> {
    // elevated read, but a contact the caller's own rights cannot read contributes no message
    let ids = readable_ids(tx, env, "res.partner", ids).await?;
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let partners = env.records("res.partner")?.browse_ids(ids);
    let rec_ids = partners.rec_ids().to_vec();
    let parents = partners.values(tx, "parent_id").await?;
    let warnings = partners.values(tx, "picking_warn_msg").await?;
    let mut map = BTreeMap::new();
    for ((&id, parent), warn) in rec_ids.iter().zip(parents).zip(warnings) {
        map.insert(
            id,
            PartnerTransferWarning {
                parent_id: optional_rec_id("res.partner.parent_id", parent)?,
                warn_msg: non_empty_text("res.partner.picking_warn_msg", warn)?,
            },
        );
    }
    Ok(map)
}

#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    /// Combined contact and parent company transfer warnings, empty when the transfer warnings
    /// feature is disabled for the caller.
    #[compute(
        depends = [
            "partner_id",
            "partner_id.picking_warn_msg",
            "partner_id.parent_id",
            "partner_id.parent_id.picking_warn_msg"
        ],
        depends_context = ["uid"]
    )]
    async fn _compute_picking_warning_text(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockPicking>,
    ) -> Result<()> {
        if !tx.has_group(this.env(), WARNING_GROUP).await? {
            for index in 0..this.len() {
                out.set(index, StockPicking::PICKING_WARNING_TEXT, None)?;
            }
            return Ok(());
        }
        let env = this.env();
        let partner_ids = this.mapped(tx, StockPicking::PARTNER_ID).await?;
        let unique_partners: Vec<RecId> = partner_ids
            .iter()
            .copied()
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut partners = read_partner_warnings(env, tx, unique_partners).await?;
        let missing_parents: Vec<RecId> = partners
            .values()
            .filter_map(|info| info.parent_id)
            .filter(|id| !partners.contains_key(id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !missing_parents.is_empty() {
            partners.extend(read_partner_warnings(env, tx, missing_parents).await?);
        }
        for (index, partner_id) in partner_ids.into_iter().enumerate() {
            let mut entries: Vec<String> = Vec::new();
            let mut seen: BTreeSet<String> = BTreeSet::new();
            if let Some(pid) = partner_id
                && let Some(partner) = partners.get(&pid)
            {
                if let Some(msg) = &partner.warn_msg
                    && seen.insert(msg.as_ref().to_owned())
                {
                    entries.push(msg.as_ref().to_owned());
                }
                if let Some(parent_id) = partner.parent_id
                    && let Some(parent) = partners.get(&parent_id)
                    && let Some(parent_msg) = &parent.warn_msg
                    && seen.insert(parent_msg.as_ref().to_owned())
                {
                    entries.push(parent_msg.as_ref().to_owned());
                }
            }
            let combined = (!entries.is_empty()).then(|| Arc::<str>::from(entries.join("\n")));
            out.set(index, StockPicking::PICKING_WARNING_TEXT, combined)?;
        }
        Ok(())
    }
}
