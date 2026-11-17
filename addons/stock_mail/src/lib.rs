// SPDX-License-Identifier: MIT OR Apache-2.0
//! Inventory mail bridge: gives `stock.picking` (transfers) the message thread and activities, the
//! chatter on its form, and tracks the fields the reference tracks
//! (the fields declared with `tracking=True`).
//!
//! A bridge rather than a `mail` dependency of `stock`, the same choice as `product_mail`.
//! The fields are re-declared here with only
//! `tracking`; every other attribute stays the inventory addon's. Posting keeps the thread's
//! default access (write), as in the reference, which does not change it for transfers.
//!
//! Tracking captures changes made through the ORM; the transfer
//! status the database derives from its moves is reported by the inventory status operations
//! (confirm, check availability, validate, cancel) through `Transaction::track_external`.

erp_sdk::addon!(name = "stock_mail", depends = ["base", "stock", "mail"]);

use erp_addon_mail::{MailActivityMixin, MailThread};
use erp_addon_stock::picking::StockPicking;
use erp_sdk::prelude::*;

/// `stock.picking` with the thread and activity mixins and its tracked fields.
#[extend(StockPicking, addon = "stock_mail", inherit = [MailThread, MailActivityMixin])]
pub struct StockPickingThread {
    /// Status.
    #[field(selection, tracking = true)]
    pub state: Option<Arc<str>>,
    /// Scheduled date.
    #[field(datetime, tracking = true)]
    pub scheduled_date: Option<DateTime>,
    /// Date of transfer.
    #[field(datetime, tracking = true)]
    pub date_done: Option<DateTime>,
    /// Operation type.
    #[field(many2one, comodel = "stock.picking.type", tracking = true)]
    pub picking_type_id: Option<RecId>,
    /// Contact.
    #[field(many2one, comodel = "res.partner", tracking = true)]
    pub partner_id: Option<RecId>,
    /// Responsible.
    #[field(many2one, comodel = "res.users", tracking = true)]
    pub user_id: Option<RecId>,
}
