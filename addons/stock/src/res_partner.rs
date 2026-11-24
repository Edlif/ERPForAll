// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company-local stock endpoints and transfer warning messages of a contact.
use erp_addon_base::ResPartner;
use erp_sdk::prelude::*;

/// The feature group controlling transfer warning messages on contacts and transfers.
pub const WARNING_GROUP: &str = "stock.group_warning_stock";

#[extend(ResPartner, addon = "stock", check_company_auto = true)]
pub(crate) struct StockPartner {
    /// Destination used when shipping goods to this contact in the active company.
    #[field(
        many2one,
        comodel = "stock.location",
        company_dependent = true,
        check_company = true,
        string = "Customer location"
    )]
    pub property_stock_customer: Option<RecId>,
    /// Source used when receiving goods from this contact in the active company.
    #[field(
        many2one,
        comodel = "stock.location",
        company_dependent = true,
        check_company = true,
        string = "Vendor location"
    )]
    pub property_stock_supplier: Option<RecId>,
    /// Non-blocking notice shown on stock transfers for this contact. Transfers show it only to a
    /// caller who can read this contact.
    #[field(
        text,
        groups = "stock.group_warning_stock",
        string = "Transfer notice"
    )]
    pub picking_warn_msg: Option<Arc<str>>,
}

#[methods(ResPartner, addon = "stock")]
impl StockPartner {
    #[constrains("picking_warn_msg")]
    async fn _check_picking_warn_msg(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let values = this.as_records().values(tx, "picking_warn_msg").await?;
        erp_addon_base::warning_text::check_length(tx, &crate::ADDON, "Transfer notice", values)
            .await
    }
}
