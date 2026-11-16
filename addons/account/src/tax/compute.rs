// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure: no database access. The caller loads the taxes into [`TaxDef`]s and gets one
//! [`TaxAmount`] per evaluated tax plus the line totals. What is covered: percent, fixed and
//! division taxes, groups of taxes (replaced by their children at the group's place), price
//! included taxes, taxes that affect the base of later ones, the evaluation order by sequence then
//! id, and rounding per line on the document currency increment (HalfAwayFromZero, no
//! binary floats). Python code taxes and the special modes are refused naming [`KERNEL_TASK`].
//! The company has no "round globally" choice yet; that mode comes with [`ROUNDING_TASK`].
//!
//! How it evaluates, in our words: the taxes are flattened and sorted, then grouped into batches
//! of neighbours that share amount type, price-included flag and base-affecting flag (walking from
//! the last tax back). Fixed taxes are evaluated first, then price-included taxes from last to
//! first (a batch shares one divisor), then price-excluded taxes from first to last. Each amount is
//! rounded at once and then moves the base of the taxes it affects (an included tax lowers the base
//! of the others, an excluded base-affecting tax raises the base of later base-affected taxes).

use erp_addon_base::money::CURRENCY_ROUNDING_MODE;
use erp_sdk::money::Increment;
use erp_sdk::prelude::*;

/// The task that adds the remaining tax kernel features (Python code taxes, special modes,
/// reverse charge).
pub const KERNEL_TASK: &str = "a later release";

/// The task that adds the company's "round globally" choice.
pub const ROUNDING_TASK: &str = "a later release";

/// How a simple tax computes its amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountType {
    /// A percentage of the base.
    Percent,
    /// A fixed amount per unit.
    Fixed,
    /// A percentage of the price including the tax.
    Division,
}

/// A tax as the computation needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct TaxDef {
    /// The `account.tax` id; with `sequence` it orders the evaluation.
    pub id: i64,
    /// The tax sequence.
    pub sequence: i32,
    /// A simple tax or a group of taxes.
    pub kind: TaxKind,
    /// The rate in percent, or the fixed amount per unit.
    pub amount: Decimal,
    /// Whether the price already includes the tax.
    pub price_include: bool,
    /// Whether the tax amount enlarges the base of later base-affected taxes.
    pub include_base_amount: bool,
    /// Whether an earlier base-affecting tax enlarges this tax's base.
    pub is_base_affected: bool,
}

/// A simple tax or a group of child taxes.
#[derive(Debug, Clone, PartialEq)]
pub enum TaxKind {
    /// A tax that computes its own amount.
    Simple(AmountType),
    /// A group of taxes: its children are evaluated at the group's place.
    Group(Vec<TaxDef>),
}

impl AmountType {
    /// The amount type of an `account.tax.amount_type` key other than `group`.
    ///
    /// # Errors
    ///
    /// A user error naming [`KERNEL_TASK`] for any other key.
    pub fn parse(key: &str) -> Result<Self> {
        match key {
            "percent" => Ok(Self::Percent),
            "fixed" => Ok(Self::Fixed),
            "division" => Ok(Self::Division),
            other => Err(Error::user(format!(
                "Taxes of the kind {other:?} cannot be computed yet: they come with task \
                 {KERNEL_TASK}. Use a percentage, fixed or percentage-of-price tax."
            ))),
        }
    }
}

/// One line to tax: its unit price, quantity and discount, and the document currency increment.
#[derive(Debug, Clone)]
pub struct BaseLine<'a> {
    /// Unit price before the discount.
    pub price_unit: Decimal,
    /// Quantity.
    pub quantity: Decimal,
    /// Discount in percent.
    pub discount: Decimal,
    /// The line's taxes, in any order.
    pub taxes: &'a [TaxDef],
    /// The rounding increment of the document currency.
    pub increment: &'a Increment,
}

/// The base and amount of one evaluated tax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxAmount {
    /// The evaluated (simple) tax.
    pub tax_id: i64,
    /// The group the tax was taken from, if any.
    pub group_id: Option<i64>,
    /// The base the amount was computed on.
    pub base: Decimal,
    /// The tax amount, rounded on the document currency.
    pub amount: Decimal,
}

/// The taxes of a line and its totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineTaxes {
    /// Amount without taxes (the subtotal).
    pub total_excluded: Decimal,
    /// Amount with taxes (the total).
    pub total_included: Decimal,
    /// One entry per evaluated tax, in evaluation order.
    pub taxes: Vec<TaxAmount>,
}

fn too_large() -> Error {
    Error::validation(
        "A tax computation exceeds the range of amounts: check the line's price and quantity.",
    )
}

fn add(a: Decimal, b: Decimal) -> Result<Decimal> {
    a.checked_add(b).ok_or_else(too_large)
}

fn sub(a: Decimal, b: Decimal) -> Result<Decimal> {
    a.checked_sub(b).ok_or_else(too_large)
}

fn mul(a: Decimal, b: Decimal) -> Result<Decimal> {
    a.checked_mul(b).ok_or_else(too_large)
}

fn div(a: Decimal, b: Decimal) -> Result<Decimal> {
    a.checked_div(b).ok_or_else(too_large)
}

/// A simple tax in evaluation order, with the group it came from.
struct Flat<'a> {
    tax: &'a TaxDef,
    kind: AmountType,
    group: Option<i64>,
}

/// The taxes sorted by (sequence, id), each group replaced by its children sorted the same way;
/// a tax listed twice is evaluated once.
fn flatten(taxes: &[TaxDef]) -> Result<Vec<Flat<'_>>> {
    fn push<'a>(flat: &mut Vec<Flat<'a>>, tax: &'a TaxDef, kind: AmountType, group: Option<i64>) {
        if !flat.iter().any(|f| f.tax.id == tax.id) {
            flat.push(Flat { tax, kind, group });
        }
    }
    let mut sorted: Vec<&TaxDef> = taxes.iter().collect();
    sorted.sort_by_key(|t| (t.sequence, t.id));
    let mut flat = Vec::with_capacity(sorted.len());
    for tax in sorted {
        match &tax.kind {
            TaxKind::Simple(kind) => push(&mut flat, tax, *kind, None),
            TaxKind::Group(children) => {
                let mut children: Vec<&TaxDef> = children.iter().collect();
                children.sort_by_key(|t| (t.sequence, t.id));
                for child in children {
                    match &child.kind {
                        TaxKind::Simple(kind) => push(&mut flat, child, *kind, Some(tax.id)),
                        TaxKind::Group(_) => {
                            return Err(Error::validation(format!(
                                "Tax {} is a group inside group {}: groups of taxes cannot be nested.",
                                child.id, tax.id
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(flat)
}

/// The batch index of each flat tax: neighbours (walking back from the last tax) that share the
/// amount type, the price-included and base-affecting flags, where a base-affecting tax never
/// joins a batch whose next tax is base-affected.
fn batches(flat: &[Flat<'_>]) -> Vec<usize> {
    let mut batch_of = vec![0; flat.len()];
    let mut current: Vec<usize> = Vec::new();
    let mut batch = 0;
    let mut next_affected = false;
    for (i, f) in flat.iter().enumerate().rev() {
        if let Some(head) = current.first().and_then(|first| flat.get(*first)) {
            let same = f.kind == head.kind
                && f.tax.price_include == head.tax.price_include
                && f.tax.include_base_amount == head.tax.include_base_amount
                && (!f.tax.include_base_amount || !next_affected);
            if !same {
                for j in current.drain(..) {
                    batch_of[j] = batch;
                }
                batch += 1;
            }
        }
        next_affected = f.tax.is_base_affected;
        current.push(i);
    }
    for j in current {
        batch_of[j] = batch;
    }
    batch_of
}

#[derive(Clone, Copy, Default)]
struct Slot {
    amount: Option<Decimal>,
    /// Added to the base this tax is computed on (only while it is not computed).
    extra_for_tax: Decimal,
    /// Added to the base reported for this tax.
    extra_for_base: Decimal,
}

/// Moves the bases of the other taxes after tax `i` got its amount.
fn propagate(flat: &[Flat<'_>], batch_of: &[usize], slots: &mut [Slot], i: usize) -> Result<()> {
    let amount = slots[i]
        .amount
        .ok_or_else(|| Error::invariant("a tax propagated before it was computed"))?;
    let tax = flat[i].tax;
    let in_batch = |j: usize| batch_of[j] == batch_of[i];
    let before: Vec<usize> = (0..flat.len()).take_while(|j| !in_batch(*j)).collect();
    let after: Vec<usize> = (0..flat.len())
        .rev()
        .take_while(|j| !in_batch(*j))
        .collect();
    let mut shift = |j: usize, positive: bool| -> Result<()> {
        let delta = if positive { amount } else { -amount };
        let slot = &mut slots[j];
        if slot.amount.is_none() {
            slot.extra_for_tax = add(slot.extra_for_tax, delta)?;
        }
        slot.extra_for_base = add(slot.extra_for_base, delta)?;
        Ok(())
    };
    if tax.price_include {
        for j in after {
            if !tax.include_base_amount || !flat[j].tax.is_base_affected {
                shift(j, false)?;
            }
        }
        for j in before {
            shift(j, false)?;
        }
    } else if tax.include_base_amount {
        for j in after {
            if flat[j].tax.is_base_affected {
                shift(j, true)?;
            }
        }
    }
    Ok(())
}

/// The unrounded amount of tax `i` on `base`, for the pass the tax belongs to.
fn raw_amount(
    flat: &[Flat<'_>],
    batch_of: &[usize],
    i: usize,
    base: Decimal,
    line: &BaseLine<'_>,
    price: Decimal,
) -> Result<Decimal> {
    let tax = flat[i].tax;
    let hundred = Decimal::ONE_HUNDRED;
    let batch_rates = flat
        .iter()
        .enumerate()
        .filter(|(j, _)| batch_of[*j] == batch_of[i])
        .try_fold(Decimal::ZERO, |sum, (_, f)| add(sum, f.tax.amount))?;
    match (flat[i].kind, tax.price_include) {
        (AmountType::Fixed, _) => {
            let sign = if price.is_sign_negative() && !price.is_zero() {
                -Decimal::ONE
            } else {
                Decimal::ONE
            };
            mul(mul(sign, line.quantity)?, tax.amount)
        }
        (AmountType::Percent, true) => {
            let divisor = add(hundred, batch_rates)?;
            if divisor.is_zero() {
                Ok(Decimal::ZERO)
            } else {
                div(mul(base, tax.amount)?, divisor)
            }
        }
        (AmountType::Division, true) | (AmountType::Percent, false) => {
            div(mul(base, tax.amount)?, hundred)
        }
        (AmountType::Division, false) => {
            let divisor = if batch_rates == hundred {
                hundred
            } else {
                sub(hundred, batch_rates)?
            };
            div(mul(base, tax.amount)?, divisor)
        }
    }
}

/// The taxes and totals of `line`.
///
/// # Errors
///
/// A validation error for a nested group or an amount out of range.
pub fn compute(line: &BaseLine<'_>) -> Result<LineTaxes> {
    let round = |amount: Decimal| -> Result<Decimal> {
        Ok(line.increment.quantize(amount, CURRENCY_ROUNDING_MODE)?)
    };
    let hundred = Decimal::ONE_HUNDRED;
    let price = div(mul(line.price_unit, sub(hundred, line.discount)?)?, hundred)?;
    let raw_base = round(mul(line.quantity, price)?)?;
    let flat = flatten(line.taxes)?;
    let batch_of = batches(&flat);
    let mut slots = vec![Slot::default(); flat.len()];
    let evaluate = |slots: &mut [Slot], i: usize| -> Result<()> {
        if slots[i].amount.is_some() {
            return Ok(());
        }
        let base = add(raw_base, slots[i].extra_for_tax)?;
        let amount = round(raw_amount(&flat, &batch_of, i, base, line, price)?)?;
        slots[i].amount = Some(amount);
        propagate(&flat, &batch_of, slots, i)
    };
    // fixed taxes, then price-included ones from the last, then price-excluded ones from the first
    for (i, f) in flat.iter().enumerate().rev() {
        if f.kind == AmountType::Fixed {
            evaluate(&mut slots, i)?;
        }
    }
    for (i, f) in flat.iter().enumerate().rev() {
        if f.tax.price_include {
            evaluate(&mut slots, i)?;
        }
    }
    for (i, f) in flat.iter().enumerate() {
        if !f.tax.price_include {
            evaluate(&mut slots, i)?;
        }
    }
    let mut taxes = Vec::with_capacity(flat.len());
    for (i, f) in flat.iter().enumerate() {
        let amount = slots[i]
            .amount
            .ok_or_else(|| Error::invariant("a tax was left out of the computation"))?;
        let mut base = add(raw_base, slots[i].extra_for_base)?;
        if f.tax.price_include {
            for (j, slot) in slots.iter().enumerate() {
                if batch_of[j] == batch_of[i] {
                    base = sub(base, slot.amount.unwrap_or_default())?;
                }
            }
        }
        taxes.push(TaxAmount {
            tax_id: f.tax.id,
            group_id: f.group,
            base,
            amount,
        });
    }
    let total_excluded = taxes.first().map_or(raw_base, |t| t.base);
    let total_included = taxes
        .iter()
        .try_fold(total_excluded, |sum, t| add(sum, t.amount))?;
    Ok(LineTaxes {
        total_excluded,
        total_included,
        taxes,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn d(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    fn cent() -> Increment {
        Increment::new(d("0.01")).unwrap()
    }

    fn tax(id: i64, sequence: i32, kind: AmountType, amount: &str) -> TaxDef {
        TaxDef {
            id,
            sequence,
            kind: TaxKind::Simple(kind),
            amount: d(amount),
            price_include: false,
            include_base_amount: false,
            is_base_affected: true,
        }
    }

    fn included(mut t: TaxDef) -> TaxDef {
        t.price_include = true;
        t
    }

    fn affecting(mut t: TaxDef) -> TaxDef {
        t.include_base_amount = true;
        t
    }

    fn run(price: &str, quantity: &str, discount: &str, taxes: &[TaxDef]) -> LineTaxes {
        let increment = cent();
        compute(&BaseLine {
            price_unit: d(price),
            quantity: d(quantity),
            discount: d(discount),
            taxes,
            increment: &increment,
        })
        .unwrap()
    }

    fn amounts(result: &LineTaxes) -> Vec<Decimal> {
        result.taxes.iter().map(|t| t.amount).collect()
    }

    #[test]
    fn percent_excluded() {
        let r = run("100", "1", "0", &[tax(1, 1, AmountType::Percent, "20")]);
        assert_eq!(amounts(&r), [d("20")]);
        assert_eq!((r.total_excluded, r.total_included), (d("100"), d("120")));
    }

    #[test]
    fn percent_included() {
        let r = run(
            "120",
            "1",
            "0",
            &[included(tax(1, 1, AmountType::Percent, "20"))],
        );
        assert_eq!(amounts(&r), [d("20")]);
        assert_eq!((r.total_excluded, r.total_included), (d("100"), d("120")));
    }

    #[test]
    fn division_excluded() {
        // 80 * 20 / (100 - 20) = 20
        let r = run("80", "1", "0", &[tax(1, 1, AmountType::Division, "20")]);
        assert_eq!(amounts(&r), [d("20")]);
        assert_eq!(r.total_included, d("100"));
    }

    #[test]
    fn division_included() {
        let r = run(
            "100",
            "1",
            "0",
            &[included(tax(1, 1, AmountType::Division, "20"))],
        );
        assert_eq!(amounts(&r), [d("20")]);
        assert_eq!((r.total_excluded, r.total_included), (d("80"), d("100")));
    }

    #[test]
    fn fixed_per_unit() {
        let r = run("50", "3", "0", &[tax(1, 1, AmountType::Fixed, "5")]);
        assert_eq!(amounts(&r), [d("15")]);
        assert_eq!((r.total_excluded, r.total_included), (d("150"), d("165")));
    }

    #[test]
    fn included_fixed_lowers_the_base_of_an_included_percent() {
        // the 20 % is taken on 105 - 5 = 100: 100 * 20 / 120 = 16.666.. -> 16.67;
        // excluded total 105 - 5 - 16.67 = 83.33
        let taxes = [
            included(tax(1, 1, AmountType::Fixed, "5")),
            included(tax(2, 2, AmountType::Percent, "20")),
        ];
        let r = run("105", "1", "0", &taxes);
        assert_eq!(amounts(&r), [d("5"), d("16.67")]);
        assert_eq!((r.total_excluded, r.total_included), (d("83.33"), d("105")));
    }

    #[test]
    fn included_fixed_on_a_zero_price() {
        let r = run(
            "0",
            "1",
            "0",
            &[included(tax(1, 1, AmountType::Fixed, "0.05"))],
        );
        assert_eq!(amounts(&r), [d("0.05")]);
        assert_eq!((r.total_excluded, r.total_included), (d("-0.05"), d("0")));
    }

    #[test]
    fn adjacent_included_percents_share_one_divisor() {
        // 120 * 10 / (100 + 10 + 10) = 10 each, not compounded
        let taxes = [
            included(tax(1, 1, AmountType::Percent, "10")),
            included(tax(2, 2, AmountType::Percent, "10")),
        ];
        let r = run("120", "1", "0", &taxes);
        assert_eq!(amounts(&r), [d("10"), d("10")]);
        assert_eq!(r.total_excluded, d("100"));
        assert!(r.taxes.iter().all(|t| t.base == d("100")));
    }

    #[test]
    fn base_affecting_tax_enlarges_the_next_base() {
        let taxes = [
            affecting(tax(1, 1, AmountType::Percent, "10")),
            tax(2, 2, AmountType::Percent, "5"),
        ];
        let r = run("100", "1", "0", &taxes);
        assert_eq!(amounts(&r), [d("10"), d("5.50")]);
        assert_eq!(r.taxes[1].base, d("110"));
        assert_eq!(
            (r.total_excluded, r.total_included),
            (d("100"), d("115.50"))
        );
    }

    #[test]
    fn three_tax_chain_with_one_tax_not_affected() {
        let mut second = tax(2, 2, AmountType::Percent, "5");
        second.is_base_affected = false;
        let taxes = [
            affecting(tax(1, 1, AmountType::Percent, "10")),
            second,
            tax(3, 3, AmountType::Percent, "5"),
        ];
        let r = run("100", "1", "0", &taxes);
        let bases: Vec<Decimal> = r.taxes.iter().map(|t| t.base).collect();
        assert_eq!(bases, [d("100"), d("100"), d("110")]);
        assert_eq!(amounts(&r), [d("10"), d("5"), d("5.50")]);
        assert_eq!(r.total_included, d("120.50"));
    }

    #[test]
    fn taxes_run_in_sequence_then_id_order() {
        let taxes = [
            tax(1, 20, AmountType::Percent, "10"),
            tax(2, 10, AmountType::Percent, "5"),
        ];
        let r = run("100", "1", "0", &taxes);
        let order: Vec<i64> = r.taxes.iter().map(|t| t.tax_id).collect();
        assert_eq!(order, [2, 1]);
    }

    #[test]
    fn a_group_is_replaced_by_its_sorted_children() {
        let group = TaxDef {
            id: 10,
            sequence: 10,
            kind: TaxKind::Group(vec![
                tax(12, 2, AmountType::Percent, "10"),
                tax(11, 1, AmountType::Fixed, "2"),
            ]),
            amount: Decimal::ZERO,
            price_include: false,
            include_base_amount: false,
            is_base_affected: true,
        };
        let taxes = [group, tax(1, 1, AmountType::Percent, "5")];
        let r = run("50", "1", "0", &taxes);
        let order: Vec<(i64, Option<i64>)> =
            r.taxes.iter().map(|t| (t.tax_id, t.group_id)).collect();
        assert_eq!(order, [(1, None), (11, Some(10)), (12, Some(10))]);
        // 5 % of 50, 2 per unit, 10 % of 50
        assert_eq!(amounts(&r), [d("2.50"), d("2"), d("5")]);
        assert_eq!(r.total_included, d("59.50"));
    }

    #[test]
    fn a_nested_group_is_refused() {
        let inner = TaxDef {
            id: 3,
            sequence: 1,
            kind: TaxKind::Group(vec![]),
            amount: Decimal::ZERO,
            price_include: false,
            include_base_amount: false,
            is_base_affected: true,
        };
        let outer = TaxDef {
            kind: TaxKind::Group(vec![inner]),
            id: 4,
            ..tax(4, 1, AmountType::Percent, "0")
        };
        let increment = cent();
        let err = compute(&BaseLine {
            price_unit: d("1"),
            quantity: d("1"),
            discount: Decimal::ZERO,
            taxes: &[outer],
            increment: &increment,
        })
        .unwrap_err();
        assert!(err.to_string().contains("cannot be nested"), "{err}");
    }

    #[test]
    fn zero_rate() {
        let r = run("100", "1", "0", &[tax(1, 1, AmountType::Percent, "0")]);
        assert_eq!(amounts(&r), [d("0")]);
        assert_eq!((r.total_excluded, r.total_included), (d("100"), d("100")));
    }

    #[test]
    fn hundred_percent_included_division() {
        let r = run(
            "100",
            "1",
            "0",
            &[included(tax(1, 1, AmountType::Division, "100"))],
        );
        assert_eq!(amounts(&r), [d("100")]);
        assert_eq!(r.total_excluded, d("0"));
    }

    #[test]
    fn fixed_taxes_follow_the_quantity_sign() {
        let taxes = [
            tax(1, 1, AmountType::Fixed, "1"),
            tax(2, 2, AmountType::Fixed, "2.50"),
        ];
        let r = run("10", "-2", "0", &taxes);
        assert_eq!(amounts(&r), [d("-2"), d("-5")]);
        assert_eq!(r.total_excluded, d("-20"));
    }

    #[test]
    fn discounted_line_with_tax() {
        // 2 * 100 * 0.9 = 180; 20 % = 36
        let r = run("100", "2", "10", &[tax(1, 1, AmountType::Percent, "20")]);
        assert_eq!((r.total_excluded, r.total_included), (d("180"), d("216")));
    }

    #[test]
    fn discount_reduces_the_subtotal() {
        assert_eq!(run("100", "1", "25", &[]).total_excluded, d("75"));
        assert_eq!(run("100", "1", "100", &[]).total_excluded, d("0"));
    }

    #[test]
    fn amounts_are_rounded_per_line_half_away_from_zero() {
        // base 0.333 * 3 = 0.999 -> 1.00; 15 % of 10.05 = 1.5075 -> 1.51; -1.5075 -> -1.51
        assert_eq!(run("0.333", "3", "0", &[]).total_excluded, d("1.00"));
        let r = run("10.05", "1", "0", &[tax(1, 1, AmountType::Percent, "15")]);
        assert_eq!(amounts(&r), [d("1.51")]);
        let r = run("10.05", "-1", "0", &[tax(1, 1, AmountType::Percent, "15")]);
        assert_eq!(amounts(&r), [d("-1.51")]);
    }

    #[test]
    fn other_amount_types_name_the_kernel_task() {
        let err = AmountType::parse("code").unwrap_err();
        assert!(err.to_string().contains(KERNEL_TASK), "{err}");
        assert_eq!(AmountType::parse("division").unwrap(), AmountType::Division);
    }
}
