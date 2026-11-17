// SPDX-License-Identifier: MIT OR Apache-2.0
use erp_addon_base::ResPartner;
use erp_sdk::prelude::*;
#[extend(ResPartner, addon = "account")]
struct FiscalPartner {
    /// Explicit position for the active company, checked against its hierarchy.
    #[field(
        many2one,
        comodel = "account.fiscal.position",
        company_dependent = true,
        check_company = true
    )]
    pub property_account_position_id: Option<RecId>,
}
#[methods(ResPartner, addon = "account")]
impl FiscalPartner {
    /// Address tax-identifier predicate; addons can add validation through Super dispatch.
    #[method]
    async fn _get_vat_required_valid(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
    ) -> Result<bool> {
        this.ensure_one()?.get(tx, ResPartner::HAS_VAT).await
    }
    #[method(override)]
    async fn _commercial_fields(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<Vec<String>> {
        let mut fields: Vec<String> = sup.call(tx, this.as_records(), ()).await?;
        fields.push("property_account_position_id".into());
        // the payment-term defaults of `payterm/partner.rs` follow the commercial partner too
        fields.push("property_payment_term_id".into());
        fields.push("property_supplier_payment_term_id".into());
        Ok(fields)
    }
}
