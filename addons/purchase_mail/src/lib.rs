// SPDX-License-Identifier: MIT OR Apache-2.0
//! Purchase mail bridge: gives `purchase.order` the message thread and activities, the chatter on
//! its form, and tracks the fields the reference tracks
//! (the fields declared with `tracking=True`),
//! as far as our model has them.
//!
//! A bridge rather than a `mail` dependency of `purchase`, the same choice as `product_mail`.
//! The fields are re-declared here with only
//! `tracking`; every other attribute stays the purchase addon's.

erp_sdk::addon!(
    name = "purchase_mail",
    depends = ["base", "purchase", "mail"]
);

use erp_addon_mail::{MailActivityMixin, MailThread};
use erp_addon_purchase::order::PurchaseOrder;
use erp_sdk::Operation;
use erp_sdk::prelude::*;

/// `purchase.order` with the thread and activity mixins and its tracked fields.
#[extend(PurchaseOrder, addon = "purchase_mail", inherit = [MailThread, MailActivityMixin])]
pub struct PurchaseOrderThread {
    /// Vendor.
    #[field(many2one, comodel = "res.partner", tracking = true)]
    pub partner_id: Option<RecId>,
    /// Status.
    #[field(selection, tracking = true)]
    pub state: Option<Arc<str>>,
    /// Locked.
    #[field(boolean, tracking = true)]
    pub locked: bool,
    /// Untaxed amount.
    #[field(monetary, tracking = true)]
    pub amount_untaxed: Decimal,
    /// Buyer.
    #[field(many2one, comodel = "res.users", tracking = true)]
    pub user_id: Option<RecId>,
}

#[methods(PurchaseOrder, addon = "purchase_mail")]
impl PurchaseOrderThread {
    /// Posting on an order needs read access, as in the reference.
    #[method(override, sealed)]
    async fn _mail_post_access(
        _this: &RecordSet<PurchaseOrder>,
        _tx: &mut Transaction,
        _sup: Super,
    ) -> Result<Operation> {
        Ok(Operation::Read)
    }
}
