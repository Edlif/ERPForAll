// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exact decimals throughout: every amount is quantized once, and the last
//! line takes the exact remainder, so the instalments add up to the totals.

use erp_addon_base::money::CURRENCY_ROUNDING_MODE;
use erp_sdk::money::Increment;
use erp_sdk::prelude::*;

/// How a line counts its delay from the reference date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delay {
    /// `nb_days` after the reference date.
    DaysAfter,
    /// `nb_days` after the end of the reference month.
    AfterEndOfMonth,
    /// `nb_days` after the end of the month following the reference date.
    AfterEndOfNextMonth,
    /// `nb_days` after the reference date, then the given day of the following month.
    EndOfMonthOn,
}

impl Delay {
    /// The delay of a stored selection key.
    pub(crate) fn parse(key: &str) -> Result<Self> {
        match key {
            "days_after" => Ok(Self::DaysAfter),
            "days_after_end_of_month" => Ok(Self::AfterEndOfMonth),
            "days_after_end_of_next_month" => Ok(Self::AfterEndOfNextMonth),
            "days_end_of_month_on_the" => Ok(Self::EndOfMonthOn),
            other => Err(Error::validation(format!(
                "The payment term delay type {other:?} is not one of days_after, \
                 days_after_end_of_month, days_after_end_of_next_month, days_end_of_month_on_the."
            ))),
        }
    }
}

/// One line of a payment term, as the split reads it.
#[derive(Debug, Clone)]
pub(crate) struct TermLine {
    /// True for a fixed amount, false for a percentage.
    pub fixed: bool,
    /// The percentage (0-100) or the fixed amount, unsigned.
    pub value: Decimal,
    pub delay: Delay,
    pub nb_days: i32,
    /// The day of the following month of [`Delay::EndOfMonthOn`], as stored text.
    pub days_next_month: Option<Arc<str>>,
}

/// One instalment: its due date and its amounts in the company and the document currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Instalment {
    pub date: Date,
    pub balance: Decimal,
    pub amount_currency: Decimal,
}

fn range() -> Error {
    Error::validation("A payment term amount or date exceeds the supported range.")
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

fn add_days(date: Date, days: i32) -> Result<Date> {
    date.days()
        .checked_add(days)
        .map(Date::from_days)
        .ok_or_else(range)
}

fn end_of_month(year: i32, month: u32) -> Result<Date> {
    Date::from_ymd(year, month, days_in_month(year, month)).ok_or_else(range)
}

/// The month after `(year, month)`.
fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

/// The due date of `line` for the reference date `date_ref`. A day of the
/// next month that is not a number counts as the 1st, one that is zero or below as the end of the
/// month; a day past the end of a short month is clamped.
pub(crate) fn due_date(line: &TermLine, date_ref: Date) -> Result<Date> {
    let (year, month, _) = date_ref.ymd();
    match line.delay {
        Delay::DaysAfter => add_days(date_ref, line.nb_days),
        Delay::AfterEndOfMonth => add_days(end_of_month(year, month)?, line.nb_days),
        Delay::AfterEndOfNextMonth => {
            let (y, m) = next_month(year, month);
            add_days(end_of_month(y, m)?, line.nb_days)
        }
        Delay::EndOfMonthOn => {
            let day = line
                .days_next_month
                .as_deref()
                .map_or(Some(1), |text| text.trim().parse::<i32>().ok())
                .unwrap_or(1);
            let shifted = add_days(date_ref, line.nb_days)?;
            let (y, m, _) = shifted.ymd();
            if day <= 0 {
                return end_of_month(y, m);
            }
            let (ny, nm) = next_month(y, m);
            let day = u32::try_from(day)
                .map_err(|_| range())?
                .min(days_in_month(ny, nm));
            Date::from_ymd(ny, nm, day).ok_or_else(range)
        }
    }
}

fn quantize(increment: &Increment, value: Decimal) -> Result<Decimal> {
    Ok(increment.quantize(value, CURRENCY_ROUNDING_MODE)?)
}

/// `value * numerator / denominator`, in exact decimals.
fn ratio(value: Decimal, numerator: Decimal, denominator: Decimal) -> Result<Decimal> {
    value
        .checked_mul(numerator)
        .and_then(|v| v.checked_div(denominator))
        .ok_or_else(range)
}

/// Splits `total` (company currency) and `total_currency` (document currency), both signed like
/// the receivable or payable line they come from, over `lines`.
///
/// A percent line takes its share of each total, quantized to the currency's increment. A fixed
/// line takes its amount entered unsigned and given `sign`, in the document currency, and the
/// company amount through the implied rate, the ratio of the two totals (
/// 076); that rate is undefined when either total is zero, which refuses the term
/// instead of guessing. The last line is the balance line whatever its kind
/// and takes what is left, so the instalments add up to the totals exactly.
///
/// # Errors
///
/// A user error for a term without lines, a fixed line when a rate cannot be derived, and an
/// amount or date out of range.
pub(crate) fn instalments(
    lines: &[TermLine],
    date_ref: Date,
    total: Decimal,
    total_currency: Decimal,
    sign: Decimal,
    company_increment: &Increment,
    currency_increment: &Increment,
) -> Result<Vec<Instalment>> {
    if lines.is_empty() {
        return Err(Error::validation(
            "The payment term has no lines: add at least one line.",
        ));
    }
    let hundred = Decimal::ONE_HUNDRED;
    let mut residual = total;
    let mut residual_currency = total_currency;
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let date = due_date(line, date_ref)?;
        let (balance, amount_currency) = if i + 1 == lines.len() {
            (residual, residual_currency)
        } else if line.fixed {
            if total.is_zero() || total_currency.is_zero() {
                return Err(Error::validation(
                    "A fixed payment term line needs the ratio of the two totals of the \
                     document, and one of them is zero: use percent lines for a document \
                     without an amount.",
                ));
            }
            let foreign = sign
                .checked_mul(quantize(currency_increment, line.value)?)
                .ok_or_else(range)?;
            let company = sign
                .checked_mul(quantize(
                    company_increment,
                    ratio(line.value, total.abs(), total_currency.abs())?,
                )?)
                .ok_or_else(range)?;
            (company, foreign)
        } else {
            (
                quantize(company_increment, ratio(total, line.value, hundred)?)?,
                quantize(
                    currency_increment,
                    ratio(total_currency, line.value, hundred)?,
                )?,
            )
        };
        residual = residual.checked_sub(balance).ok_or_else(range)?;
        residual_currency = residual_currency
            .checked_sub(amount_currency)
            .ok_or_else(range)?;
        out.push(Instalment {
            date,
            balance,
            amount_currency,
        });
    }
    Ok(out)
}

/// The term's own checks: the percent lines add up to 100 once rounded to `digits` decimals
/// and each percent is within 0 and 100.
pub(crate) fn check_percent_lines(lines: &[TermLine], digits: u32) -> Result<()> {
    let hundred = Decimal::ONE_HUNDRED;
    let mut sum = Decimal::ZERO;
    for line in lines.iter().filter(|l| !l.fixed) {
        if line.value < Decimal::ZERO || line.value > hundred {
            return Err(Error::validation(
                "A percentage on a payment term line must be from 0 to 100.",
            ));
        }
        sum = sum.checked_add(line.value).ok_or_else(range)?;
    }
    let grid = Increment::new(Decimal::new(1, digits))?;
    if quantize(&grid, sum)? != hundred {
        return Err(Error::validation(
            "The percentage lines of a payment term must add up to exactly 100, and there must be at least one.",
        ));
    }
    Ok(())
}

/// The day text of a line is a whole number from 0 to 31.
pub(crate) fn check_days_next_month(text: Option<&str>) -> Result<()> {
    let text = text.unwrap_or_default();
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::validation(
            "That day of the next month is not a whole number: enter a number from 0 to 31.",
        ));
    }
    match text.parse::<i32>() {
        Ok(day) if (0..=31).contains(&day) => Ok(()),
        _ => Err(Error::validation(
            "The day of the next month must be from 0 to 31.",
        )),
    }
}

#[cfg(test)]
#[path = "test_compute.rs"]
mod tests;
