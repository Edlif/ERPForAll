// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company-owned configuration for advisory valuation books.

use erp_sdk::Operation;
use erp_sdk::prelude::*;

pub(crate) const MODEL: &str = "stock.cost.book";

#[model(
    name = "stock.cost.book",
    description = "Stock Cost Book",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        ("_company_name", "UNIQUE (company_id, name)", "A company already has a cost book with this name."),
        ("_name_present", "CHECK (btrim(name) <> '')", "Enter a cost book name."),
        ("_management_only", "CHECK (purpose = 'management')", "Only management cost books can be configured until statutory posting is supported."),
        ("_supported_method", "CHECK (method IN ('fifo', 'lifo'))", "Management cost books support FIFO or perpetual LIFO."),
    ],
)]
struct StockCostBook {
    #[field(char, required = true)]
    name: Option<Arc<str>>,
    #[field(
        many2one,
        comodel = "res.company",
        ondelete = "restrict",
        required = true
    )]
    company_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "res.currency",
        ondelete = "restrict",
        required = true
    )]
    currency_id: Option<RecId>,
    #[field(selection, selection = [("management", "Management"), ("statutory", "Statutory")], required = true)]
    purpose: Option<Arc<str>>,
    #[field(selection, selection = [("fifo", "FIFO"), ("lifo", "Perpetual LIFO")], required = true)]
    method: Option<Arc<str>>,
    #[field(date, required = true)]
    effective_from: Option<Date>,
    #[field(boolean, default = true)]
    active: bool,
}

#[methods(StockCostBook, addon = "stock_account")]
impl StockCostBook {
    #[method(override)]
    async fn upsert(
        this: &RecordSet<StockCostBook>,
        tx: &mut Transaction,
        _call: erp_sdk::UpsertCall,
        _sup: Super,
    ) -> Result<erp_sdk::UpsertResult> {
        let records = this.as_records();
        records.check_access(tx, Operation::Create).await?;
        records.check_access(tx, Operation::Write).await?;
        Err(Error::validation(
            "Create or edit cost books explicitly. Updating a cost book by key is not supported.",
        ))
    }
}
