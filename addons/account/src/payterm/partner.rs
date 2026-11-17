// SPDX-License-Identifier: MIT OR Apache-2.0
//! The default payment terms of a partner, one value per company.
use erp_addon_base::ResPartner;
use erp_sdk::prelude::*;

#[extend(ResPartner, addon = "account")]
struct PaymentTermPartner {
    /// Terms a new customer invoice of this partner starts with.
    #[field(
        many2one,
        comodel = "account.payment.term",
        company_dependent = true,
        check_company = true,
        string = "Customer Payment Terms"
    )]
    pub property_payment_term_id: Option<RecId>,
    /// Terms a new vendor bill from this partner starts with.
    #[field(
        many2one,
        comodel = "account.payment.term",
        company_dependent = true,
        check_company = true,
        string = "Vendor Payment Terms"
    )]
    pub property_supplier_payment_term_id: Option<RecId>,
}
