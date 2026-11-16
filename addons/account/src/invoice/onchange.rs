// SPDX-License-Identifier: MIT OR Apache-2.0

use erp_sdk::prelude::*;

use super::{is_invoice, is_sale};
use crate::moves::AccountMoveLine;

fn amount(cell: &Cell) -> Result<Decimal> {
    match cell {
        Cell::Null | Cell::Bool(false) => Ok(Decimal::ZERO),
        Cell::Decimal(value) => Ok(*value),
        Cell::Float(value) => Decimal::try_from(*value)
            .map_err(|e| Error::validation(format!("The product price {value} is not exact: {e}"))),
        other => Err(Error::invariant(format!(
            "a product price holds {other:?} instead of an amount"
        ))),
    }
}

/// The invoice type of the line's entry: the `default_move_type` the invoice actions put in the
/// context, else the entry's own type; `None` outside invoices.
async fn invoice_type(
    this: &RecordSet<AccountMoveLine>,
    tx: &mut Transaction,
) -> Result<Option<Arc<str>>> {
    if let Some(Value::Str(kind)) = this.env().context().get("default_move_type") {
        return Ok(Some(kind.clone()).filter(|k| is_invoice(k)));
    }
    let Some(entry) = this.get(tx, AccountMoveLine::MOVE_ID).await? else {
        return Ok(None);
    };
    let cell = this
        .env()
        .records("account.move")?
        .browse_ids([entry])
        .cell(tx, "move_type")
        .await?;
    Ok(match cell {
        Cell::Str(kind) if is_invoice(&kind) => Some(kind),
        _ => None,
    })
}

#[methods(AccountMoveLine, addon = "account")]
impl AccountMoveLine {
    /// Proposes the product's name, unit, sale or purchase price and the product's sale or purchase
    /// taxes of the active company (partial: no unit conversion, no
    /// fiscal position, no company default tax).
    #[onchange("product_id")]
    async fn _onchange_product_invoice(
        this: &RecordSet<AccountMoveLine>,
        tx: &mut Transaction,
    ) -> Result<OnchangeResult> {
        let Some(product) = this.get(tx, AccountMoveLine::PRODUCT_ID).await? else {
            return Ok(OnchangeResult::none());
        };
        let Some(move_type) = invoice_type(this, tx).await? else {
            return Ok(OnchangeResult::none());
        };
        let env = this.env().clone();
        let sale = is_sale(&move_type);
        let record = env.records("product.product")?.browse_ids([product]);
        if let Cell::Str(name) = record.cell(tx, "display_name").await? {
            this.set(tx, AccountMoveLine::NAME, Some(name)).await?;
        }
        if let Cell::Id(uom) = record.cell(tx, "uom_id").await?
            && let Some(uom) = RecId::from_real(uom)
        {
            this.set(tx, AccountMoveLine::PRODUCT_UOM_ID, Some(uom))
                .await?;
        }
        let price_field = if sale { "lst_price" } else { "standard_price" };
        let price = amount(&record.cell(tx, price_field).await?)?;
        this.set(tx, AccountMoveLine::PRICE_UNIT, price).await?;
        let taxes_field = if sale {
            "taxes_id"
        } else {
            "supplier_taxes_id"
        };
        let proposed: Vec<i64> = match record.cell(tx, taxes_field).await? {
            Cell::Ids(ids) => ids
                .iter()
                .filter_map(|id| id.as_real().map(|id| id.get()))
                .collect(),
            _ => Vec::new(),
        };
        let taxes = match env.company_id()? {
            Some(company) if !proposed.is_empty() => env
                .records("account.tax")?
                .search(
                    tx,
                    &Domain::and([
                        Domain::condition("id", Operator::In, proposed)?,
                        Domain::condition("company_id", Operator::ParentOf, company)?,
                    ]),
                )
                .await?
                .ids(),
            _ => Vec::new(),
        };
        let taxes: Vec<RecId> = taxes.into_iter().filter_map(RecId::from_real).collect();
        this.set(tx, AccountMoveLine::TAX_IDS, taxes).await?;
        Ok(OnchangeResult::none())
    }
}
