// SPDX-License-Identifier: MIT OR Apache-2.0
//! A correction on a linked bill can change the cumulative result of another bill.

use erp_addon_account::moves::{AccountMove, AccountMoveLine};
use erp_addon_purchase::line::PurchaseOrderLine;
use erp_sdk::prelude::*;

async fn invalidate(tx: &mut Transaction) -> Result<()> {
    for model in ["account.move", "account.move.line"] {
        tx.invalidate_model(model, Some(&["purchase_match_status"]))
            .await?;
    }
    Ok(())
}

#[extend(PurchaseOrderLine, addon = "purchase_three_way_match")]
struct SourceChanges {}

#[methods(PurchaseOrderLine, addon = "purchase_three_way_match")]
impl SourceChanges {
    #[method(override)]
    async fn _invalidate_purchase_bill_review(
        this: &RecordSet<PurchaseOrderLine>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let () = sup.call(tx, this.as_records(), ()).await?;
        invalidate(tx).await
    }
}

struct DocumentChanges;

#[methods(AccountMove, addon = "purchase_three_way_match")]
impl DocumentChanges {
    #[method(override)]
    async fn write(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let affects = [
            "state",
            "move_type",
            "invoice_line_ids",
            "line_ids",
            "currency_id",
            "company_id",
        ]
        .iter()
        .any(|f| vals.contains(f));
        let () = sup.call(tx, this.as_records(), (vals,)).await?;
        if affects {
            invalidate(tx).await?;
        }
        Ok(())
    }

    #[method(override)]
    async fn unlink(this: &RecordSet<AccountMove>, tx: &mut Transaction, sup: Super) -> Result<()> {
        let () = sup.call(tx, this.as_records(), ()).await?;
        invalidate(tx).await
    }
}

struct ItemChanges;

#[methods(AccountMoveLine, addon = "purchase_three_way_match")]
impl ItemChanges {
    #[method(override)]
    async fn write(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let affects = [
            "quantity",
            "discount",
            "price_unit",
            "product_uom_id",
            "product_id",
            "currency_id",
            "move_id",
            "display_type",
        ]
        .iter()
        .any(|f| vals.contains(f));
        let () = sup.call(tx, this.as_records(), (vals,)).await?;
        if affects {
            invalidate(tx).await?;
        }
        Ok(())
    }

    #[method(override)]
    async fn unlink(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let () = sup.call(tx, this.as_records(), ()).await?;
        invalidate(tx).await
    }
}
