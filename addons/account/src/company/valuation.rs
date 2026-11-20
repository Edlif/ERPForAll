// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company view of the dated valuation account shared with chart activation.
use erp_sdk::prelude::*;

use crate::mapping::{CompanyDefault, DefaultRole, ResolutionDate};

pub(crate) const FIELD: &str = "account_stock_valuation_id";

#[extend(ResCompany, addon = "account")]
struct CompanyValuation {
    /// Company-level fallback for stock valuation. Its dated relation is the source of truth.
    #[field(
        many2one,
        comodel = "account.account",
        compute = "_compute_stock_valuation_account",
        inverse = "_inverse_stock_valuation_account",
        search_method = "_search_stock_valuation_account",
        compute_sudo = false,
        groups = "account.group_account_readonly",
        copy = false,
        string = "Stock valuation account"
    )]
    pub account_stock_valuation_id: Option<RecId>,
}

#[methods(ResCompany, addon = "account")]
impl CompanyValuation {
    #[compute(depends = ["tz"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_stock_valuation_account(
        this: &RecordSet<ResCompany>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ResCompany>,
    ) -> Result<()> {
        for (index, id) in this.rec_ids().iter().enumerate() {
            let account = if let Some(company) = id.as_real().or(id.origin()) {
                CompanyDefault::new(company.get(), DefaultRole::StockValuation)?
                    .resolve_as(tx, this.env(), ResolutionDate::Current)
                    .await?
                    .map(|id| {
                        RecId::from_real(id)
                            .ok_or_else(|| Error::invariant("Invalid stock valuation account ID"))
                    })
                    .transpose()?
            } else {
                None
            };
            out.set(index, CompanyValuation::ACCOUNT_STOCK_VALUATION_ID, account)?;
        }
        Ok(())
    }

    #[method]
    async fn _inverse_stock_valuation_account(
        this: &RecordSet<ResCompany>,
        tx: &mut Transaction,
    ) -> Result<()> {
        // Company generation rows are locked until commit. Acquire them in the same
        // order for concurrent batch writes, regardless of the caller's ID order.
        let mut ids = this.ids();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let account = this.as_records().browse([id])?.cell(tx, FIELD).await?;
            let account = match account {
                Cell::Id(id) => Some(id),
                Cell::Null => None,
                _ => {
                    return Err(Error::validation(
                        "Save the stock valuation account before selecting it.",
                    ));
                }
            };
            CompanyDefault::new(id, DefaultRole::StockValuation)?
                .set_as(tx, this.env(), account)
                .await?;
        }
        Ok(())
    }

    #[search_method]
    fn _search_stock_valuation_account(
        _req: &FieldSearchRequest<'_>,
        _ctx: &mut FieldSearchContext<'_>,
    ) -> Result<Domain> {
        Err(Error::user(
            "Searching account_stock_valuation_id needs a later release.",
        ))
    }
}
