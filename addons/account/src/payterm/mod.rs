// SPDX-License-Identifier: MIT OR Apache-2.0
//! The arithmetic lives in [`compute`]; the invoice line sync (`invoice/sync.rs`) turns a
//! term into one receivable or payable line per instalment, and [`partner`] holds the
//! customer and vendor defaults.
//!
//! Not in this slice (each refused, never guessed): early payment discount and its tax handling,
//! cash rounding of instalments, and the instalment preview of the form.

pub(crate) mod compute;
mod partner;

use std::collections::BTreeMap;

use compute::{Delay, TermLine};
use erp_addon_base::decimal_precision::precision_defined;
use erp_sdk::prelude::*;

/// The decimal precision usage that sets how finely percentages are compared with 100.
pub const PRECISION_USAGE: &str = "Payment Terms";

/// The digits used when no `Payment Terms` precision row exists; the shipped data sets 6.
const DEFAULT_DIGITS: u32 = 6;

/// A rule set that splits an invoice total over due dates.
#[model(name = "account.payment.term", description = "Payment Terms", order = "sequence, id", company_access = direct(field = "company_id", branch = "ancestor", empty = "allow"), company_match = "parent_of")]
pub struct AccountPaymentTerm {
    /// Name offered when choosing the terms of an invoice.
    #[field(char, required = true, translate = true, string = "Payment Terms")]
    pub name: Option<Arc<str>>,
    /// Archived terms stay on old documents and are no longer offered.
    #[field(boolean, default = true)]
    pub active: bool,
    /// Text printed on invoices that use the terms.
    #[field(html, translate = true, string = "Description on the Invoice")]
    pub note: Option<Arc<str>>,
    /// The instalments, in order; the last one takes whatever the others leave.
    #[field(
        one2many,
        comodel = "account.payment.term.line",
        inverse_name = "payment_id",
        copy = true,
        string = "Terms"
    )]
    pub line_ids: Vec<RecId>,
    /// Restricts the terms to one company and its branches; empty means every company.
    #[field(many2one, comodel = "res.company", index = true, ondelete = "restrict")]
    pub company_id: Option<RecId>,
    /// Order in the list of terms.
    #[field(integer, default = 10)]
    pub sequence: i32,
    /// Show the instalment dates and amounts on the printed invoice.
    #[field(boolean, default = true, string = "Show instalments on the invoice")]
    pub display_on_invoice: bool,
}

/// One instalment rule of a payment term.
#[model(
    name = "account.payment.term.line",
    description = "Payment Terms Line",
    order = "id",
    company_access = direct(field = "company_id", branch = "ancestor", empty = "allow", parent = "payment_id"),
    company_match = "parent_of",
    constraints = [(
        "_percent_range",
        "CHECK (value <> 'percent' OR value_amount BETWEEN 0 AND 100)",
        "A percentage on a payment term line must be from 0 to 100."
    )],
)]
pub struct AccountPaymentTermLine {
    /// A share of the total or a fixed amount.
    #[field(
        selection,
        selection = [("percent", "Percent"), ("fixed", "Fixed")],
        required = true,
        default = "percent"
    )]
    pub value: Option<Arc<str>>,
    /// The percentage (0 to 100) or the fixed amount.
    #[field(float, digits = "Payment Terms", default = 0, string = "Due")]
    pub value_amount: Decimal,
    /// How the delay is counted.
    #[field(
        selection,
        selection = [
            ("days_after", "Days after invoice date"),
            ("days_after_end_of_month", "Days after month end"),
            ("days_after_end_of_next_month", "Days after next month end"),
            ("days_end_of_month_on_the", "Fixed day of the next month")
        ],
        required = true,
        default = "days_after"
    )]
    pub delay_type: Option<Arc<str>>,
    /// Day of the following month used by "Days end of month on the"; 0 means the month's end.
    #[field(char, size = 2, default = "10", string = "Day of the next month")]
    pub days_next_month: Option<Arc<str>>,
    /// Days of delay.
    #[field(integer, default = 0, string = "Days")]
    pub nb_days: i32,
    /// The owning payment term.
    #[field(
        many2one,
        comodel = "account.payment.term",
        required = true,
        index = true,
        ondelete = "cascade",
        string = "Payment Terms"
    )]
    pub payment_id: Option<RecId>,
    /// The company of the owning term, for company access.
    #[field(
        many2one,
        comodel = "res.company",
        related = "payment_id.company_id",
        store = true
    )]
    pub company_id: Option<RecId>,
}

fn text(cell: &Cell) -> Option<Arc<str>> {
    match cell {
        Cell::Str(text) => Some(text.clone()),
        _ => None,
    }
}

fn amount(cell: &Cell) -> Result<Decimal> {
    match cell {
        Cell::Null | Cell::Bool(false) => Ok(Decimal::ZERO),
        Cell::Decimal(value) => Ok(*value),
        Cell::Int(value) => Ok(Decimal::from(*value)),
        Cell::Float(value) => Decimal::try_from(*value).map_err(|e| {
            Error::validation(format!(
                "A payment term line amount {value} is not exact: {e}"
            ))
        }),
        other => Err(Error::invariant(format!(
            "a payment term line amount holds {other:?} instead of an amount"
        ))),
    }
}

fn whole(cell: &Cell) -> Result<i32> {
    match cell {
        Cell::Null | Cell::Bool(false) => Ok(0),
        Cell::Int(value) => Ok(*value),
        other => Err(Error::invariant(format!(
            "a payment term line delay holds {other:?} instead of a whole number"
        ))),
    }
}

/// The lines of each of `term_ids`, in line order. A term without lines is present with an empty
/// list, so the caller tells "unknown term" from "term without lines".
pub(crate) async fn load_terms(
    tx: &mut Transaction,
    env: &Env,
    term_ids: &[i64],
) -> Result<BTreeMap<i64, Vec<TermLine>>> {
    let mut by_term: BTreeMap<i64, Vec<TermLine>> =
        term_ids.iter().map(|id| (*id, Vec::new())).collect();
    if term_ids.is_empty() {
        return Ok(by_term);
    }
    let found = env
        .records("account.payment.term.line")?
        .search(
            tx,
            &Domain::condition("payment_id", Operator::In, term_ids.to_vec())?,
        )
        .await?;
    let owners = found.cells(tx, "payment_id").await?;
    let values = found.cells(tx, "value").await?;
    let amounts = found.cells(tx, "value_amount").await?;
    let delays = found.cells(tx, "delay_type").await?;
    let days = found.cells(tx, "nb_days").await?;
    let nexts = found.cells(tx, "days_next_month").await?;
    for i in 0..owners.len() {
        let short = || Error::invariant("a payment term line column is short");
        let owner = match owners.get(i).ok_or_else(short)? {
            Cell::Id(id) => *id,
            other => {
                return Err(Error::invariant(format!(
                    "a payment term line has the owner {other:?}"
                )));
            }
        };
        let delay = text(delays.get(i).ok_or_else(short)?)
            .ok_or_else(|| Error::invariant("a payment term line has no delay type"))?;
        by_term.entry(owner).or_default().push(TermLine {
            fixed: text(values.get(i).ok_or_else(short)?).as_deref() == Some("fixed"),
            value: amount(amounts.get(i).ok_or_else(short)?)?,
            delay: Delay::parse(&delay)?,
            nb_days: whole(days.get(i).ok_or_else(short)?)?,
            days_next_month: text(nexts.get(i).ok_or_else(short)?),
        });
    }
    Ok(by_term)
}

/// The digits the percentages are compared at (setting: the `Payment Terms` decimal precision).
pub(crate) async fn percent_digits(tx: &mut Transaction, env: &Env) -> Result<u32> {
    Ok(precision_defined(env, tx, PRECISION_USAGE)
        .await?
        .unwrap_or(DEFAULT_DIGITS))
}

#[methods(AccountPaymentTerm, addon = "account")]
impl AccountPaymentTerm {
    /// The percent lines add up to 100 and each is a valid percentage (
    /// 025..027); a term needs at least one percent line.
    #[constrains("name", "line_ids")]
    async fn _check_lines(
        this: &RecordSet<AccountPaymentTerm>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let digits = percent_digits(tx, this.env()).await?;
        let terms = load_terms(tx, this.env(), &this.ids()).await?;
        for lines in terms.values() {
            compute::check_percent_lines(lines, digits)?;
        }
        Ok(())
    }
}

#[methods(AccountPaymentTermLine, addon = "account")]
impl AccountPaymentTermLine {
    /// A percentage is between 0 and 100.
    #[constrains("value", "value_amount")]
    async fn _check_percent(
        this: &RecordSet<AccountPaymentTermLine>,
        tx: &mut Transaction,
    ) -> Result<()> {
        for line in this.iter() {
            let kind = line.get(tx, AccountPaymentTermLine::VALUE).await?;
            let value = line.get(tx, AccountPaymentTermLine::VALUE_AMOUNT).await?;
            if kind.as_deref() == Some("percent")
                && (value < Decimal::ZERO || value > Decimal::ONE_HUNDRED)
            {
                return Err(Error::validation(
                    "A percentage on a payment term line must be from 0 to 100.",
                ));
            }
        }
        Ok(())
    }

    /// The day of the next month is a number from 0 to 31.
    #[constrains("days_next_month")]
    async fn _check_days_next_month(
        this: &RecordSet<AccountPaymentTermLine>,
        tx: &mut Transaction,
    ) -> Result<()> {
        for line in this.iter() {
            let day = line
                .get(tx, AccountPaymentTermLine::DAYS_NEXT_MONTH)
                .await?;
            compute::check_days_next_month(day.as_deref())?;
        }
        Ok(())
    }
}
