// SPDX-License-Identifier: MIT OR Apache-2.0
//! The pure part of reconciliation: which debit is matched with which credit for how much, and the
//! payment status an invoice reads from its residual and its matches. No database access.
//!
//! Pairing as observed in the reference (`_optimize_reconciliation_plan`,
//! `_prepare_reconciliation_amls`, 3005-3040`): the items are taken
//! in the order due date (else date), amount in currency, balance, then grouped by partner when
//! several partners take part; debits and credits are walked side by side and each step matches the
//! smaller of the two open amounts. Our own implementation, in exact decimals.

use std::collections::BTreeSet;

use erp_sdk::prelude::*;

/// An item as the pairing sees it: its open amount (positive on a debit, negative on a credit) and
/// its sort keys.
#[derive(Debug, Clone)]
pub(crate) struct Open {
    pub id: i64,
    pub residual: Decimal,
    pub due: Date,
    pub amount_currency: Decimal,
    pub balance: Decimal,
    pub partner: Option<i64>,
}

/// One match to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pair {
    pub debit: i64,
    pub credit: i64,
    pub amount: Decimal,
}

/// The matches that settle `items` as far as they offset each other, in the reference's order.
pub(crate) fn pair(mut items: Vec<Open>) -> Vec<Pair> {
    items.sort_by(|a, b| {
        (a.due, a.amount_currency, a.balance, a.id).cmp(&(
            b.due,
            b.amount_currency,
            b.balance,
            b.id,
        ))
    });
    let partners: BTreeSet<Option<i64>> = items.iter().map(|i| i.partner).collect();
    if partners.len() > 1 {
        // stable: the order above holds inside each partner; no partner comes first
        items.sort_by_key(|i| i.partner);
    }
    let mut debits: Vec<(i64, Decimal)> = Vec::new();
    let mut credits: Vec<(i64, Decimal)> = Vec::new();
    for item in &items {
        if item.residual > Decimal::ZERO {
            debits.push((item.id, item.residual));
        } else if item.residual < Decimal::ZERO {
            credits.push((item.id, -item.residual));
        }
    }
    let mut pairs = Vec::new();
    let (mut d, mut c) = (0, 0);
    while let (Some(debit), Some(credit)) = (debits.get(d).copied(), credits.get(c).copied()) {
        let amount = debit.1.min(credit.1);
        pairs.push(Pair {
            debit: debit.0,
            credit: credit.0,
            amount,
        });
        // amounts are exact: the smaller side is exhausted, possibly both
        if let Some(left) = debits.get_mut(d) {
            left.1 -= amount;
            if left.1.is_zero() {
                d += 1;
            }
        }
        if let Some(left) = credits.get_mut(c) {
            left.1 -= amount;
            if left.1.is_zero() {
                c += 1;
            }
        }
    }
    pairs
}

/// What an invoice's matches say about it, for [`payment_state`].
#[derive(Debug, Default)]
pub(crate) struct Settlement {
    /// The kinds (`move_type`) of the other entries matched with its receivable or payable items.
    pub counterpart_types: BTreeSet<String>,
    /// Whether one of those entries belongs to a payment or a bank statement line.
    pub has_payment: bool,
}

/// The payment status of a posted invoice (reference `_compute_payment_state`,
/// observed): settled is paid, or reversed when only credit notes of
/// its kind (and plain entries) settled it; a residual with matches is partial; else not paid. The
/// "in payment" status of the reference resolves to paid here, as in the reference without its
/// accountant extension (plan decision L14).
pub(crate) fn payment_state(move_type: &str, settled: bool, matches: &Settlement) -> &'static str {
    let types = &matches.counterpart_types;
    if settled {
        if matches.has_payment || types.is_empty() {
            return "paid";
        }
        let only = |kinds: &[&str]| types.iter().all(|t| kinds.contains(&t.as_str()));
        let reversed = match move_type {
            "out_invoice" => types.contains("out_refund") && only(&["out_refund", "entry"]),
            "in_invoice" => types.contains("in_refund") && only(&["in_refund", "entry"]),
            "out_refund" | "in_refund" => only(&["entry"]),
            _ => false,
        };
        if reversed { "reversed" } else { "paid" }
    } else if types.is_empty() {
        "not_paid"
    } else {
        "partial"
    }
}

#[cfg(test)]
#[path = "tests/plan.rs"]
mod tests;
