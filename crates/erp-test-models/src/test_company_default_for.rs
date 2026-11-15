// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixtures of the company fields that mirror a company-dependent default (ADR
//! 0163 supplement of 2026-11-15): a model with one company-dependent field per admitted kind,
//! and the `res.company` fields declared `company_default_for` over them. Load with `base`.

use erp_sdk::prelude::*;

erp_sdk::addon!(name = "test_company_default_for", depends = ["base"]);

/// The target model (the reference's `test_orm.company_default_for`).
#[model(
    name = "test_orm.company_default_for",
    company_access = unrestricted(reason = "company default fixture; record access is not under test"),
    description = "Company Default For",
    addon = "test_company_default_for"
)]
pub struct CompanyDefaultFor {
    #[field(char)]
    pub name: Option<Arc<str>>,
    #[field(float, company_dependent = true)]
    pub credit_limit: f64,
    #[field(many2one, comodel = "res.partner", company_dependent = true)]
    pub partner_id: Option<RecId>,
    #[field(selection, selection = [("a", "First"), ("b", "Second")], company_dependent = true)]
    pub choice: Option<Arc<str>>,
    #[field(boolean, company_dependent = true)]
    pub flag: bool,
    #[field(integer, company_dependent = true)]
    pub count: i32,
    #[field(char, company_dependent = true)]
    pub label: Option<Arc<str>>,
    /// The company whose setting `company_credit_limit` shows (the Enterprise settings pattern).
    #[field(many2one, comodel = "res.company")]
    pub company_id: Option<RecId>,
    #[field(float, related = "company_id.credit_limit")]
    pub company_credit_limit: f64,
}

/// The company settings that mirror the defaults of [`CompanyDefaultFor`].
#[extend(ResCompany, addon = "test_company_default_for")]
pub struct CompanyDefaults {
    #[field(
        float,
        default = 1000.0,
        company_default_for = "test_orm.company_default_for.credit_limit"
    )]
    pub credit_limit: f64,
    #[field(
        many2one,
        comodel = "res.partner",
        company_default_for = "test_orm.company_default_for.partner_id"
    )]
    pub default_partner_id: Option<RecId>,
    #[field(
        selection,
        selection = [("a", "First"), ("b", "Second")],
        company_default_for = "test_orm.company_default_for.choice"
    )]
    pub default_choice: Option<Arc<str>>,
    #[field(boolean, company_default_for = "test_orm.company_default_for.flag")]
    pub default_flag: bool,
    #[field(integer, company_default_for = "test_orm.company_default_for.count")]
    pub default_count: i32,
    #[field(char, company_default_for = "test_orm.company_default_for.label")]
    pub default_label: Option<Arc<str>>,
    /// A field with a method inverse: the inverse copies the credit limit it reads at that
    /// moment into `probe_seen`, which shows whether the company default of the same write was
    /// already stored.
    #[field(char, inverse = "_inverse_probe")]
    pub probe: Option<Arc<str>>,
    #[field(float)]
    pub probe_seen: f64,
}

#[methods(ResCompany, addon = "test_company_default_for")]
impl CompanyDefaults {
    #[method]
    async fn _inverse_probe(this: &RecordSet<ResCompany>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        for id in records.ids() {
            let record = records.browse([id])?;
            let seen = record.cell(tx, "credit_limit").await?;
            record
                .write(tx, RawVals::new().set_cell("probe_seen", seen))
                .await?;
        }
        Ok(())
    }
}
