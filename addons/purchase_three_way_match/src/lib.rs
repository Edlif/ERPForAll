// SPDX-License-Identifier: MIT OR Apache-2.0
//! Receipt-based advisory matching for saved native vendor bills.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_account::moves::{AccountMove, AccountMoveLine};
use erp_sdk::prelude::*;

erp_sdk::addon!(
    name = "purchase_three_way_match",
    depends = ["base", "purchase"]
);

mod changes;
mod review;

#[extend(AccountMove, addon = "purchase_three_way_match")]
struct Bill {
    #[field(
        char,
        compute = "_compute_purchase_match",
        compute_sudo = false,
        readonly = true,
        copy = false,
        groups = "purchase.group_purchase_user",
        string = "Purchase Match"
    )]
    purchase_match_status: Option<Arc<str>>,
}

#[methods(AccountMove, addon = "purchase_three_way_match")]
impl Bill {
    // The explicit source-change hooks handle protected fields and cross-bill dependencies.
    #[compute(depends = ["move_type"], depends_context = ["uid","allowed_company_ids"])]
    async fn _compute_purchase_match(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountMove>,
    ) -> Result<()> {
        let records = this.as_records();
        let results = review::evaluate(tx, records.env(), &records.origin().ids()).await?;
        for (index, record) in records.rec_ids().iter().enumerate() {
            let value = record.as_real().and_then(|id| match &results {
                review::Evaluation::Complete(bills) => {
                    bills.get(&id.get()).map(|bill| bill.summary())
                }
                review::Evaluation::Unverified(reason) => Some(*reason),
            });
            out.set(index, Bill::PURCHASE_MATCH_STATUS, value.map(Into::into))?;
        }
        Ok(())
    }
}

#[extend(AccountMoveLine, addon = "purchase_three_way_match")]
struct BillItem {
    #[field(
        char,
        compute = "_compute_purchase_item_match",
        compute_sudo = false,
        readonly = true,
        copy = false,
        groups = "purchase.group_purchase_user",
        string = "Purchase Match"
    )]
    purchase_match_status: Option<Arc<str>>,
}

#[methods(AccountMoveLine, addon = "purchase_three_way_match")]
impl BillItem {
    #[compute(depends = ["move_id"], depends_context = ["uid","allowed_company_ids"])]
    async fn _compute_purchase_item_match(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountMoveLine>,
    ) -> Result<()> {
        let records = this.as_records();
        let saved = records.origin();
        let moves = saved.cells(tx, "move_id").await?;
        let mut ids = BTreeSet::new();
        for cell in moves {
            if let Cell::Id(id) = cell {
                ids.insert(id);
            }
        }
        let bills =
            review::evaluate(tx, records.env(), &ids.into_iter().collect::<Vec<_>>()).await?;
        let (items, unavailable): (BTreeMap<_, _>, _) = match bills {
            review::Evaluation::Complete(bills) => (
                bills.into_values().flat_map(|bill| bill.items).collect(),
                None,
            ),
            review::Evaluation::Unverified(reason) => (BTreeMap::new(), Some(reason)),
        };
        for (index, record) in records.rec_ids().iter().enumerate() {
            let value = record
                .as_real()
                .and_then(|id| items.get(&id.get()))
                .map(|advice| advice.label())
                .or(unavailable);
            out.set(
                index,
                BillItem::PURCHASE_MATCH_STATUS,
                value.map(Into::into),
            )?;
        }
        Ok(())
    }
}
