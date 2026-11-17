// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit tests of the pairing and the payment status (`reconcile::plan`).
#![allow(clippy::unwrap_used, reason = "tests")]
use super::*;

fn d(v: &str) -> Decimal {
    v.parse().unwrap()
}

fn open(id: i64, residual: &str, day: u32, partner: Option<i64>) -> Open {
    Open {
        id,
        residual: d(residual),
        due: Date::from_ymd(2024, 1, day).unwrap(),
        amount_currency: d(residual),
        balance: d(residual),
        partner,
    }
}

fn p(debit: i64, credit: i64, amount: &str) -> Pair {
    Pair {
        debit,
        credit,
        amount: d(amount),
    }
}

#[test]
fn a_smaller_credit_settles_part_of_the_debit() {
    let pairs = pair(vec![open(1, "1000", 1, None), open(2, "-300", 2, None)]);
    assert_eq!(pairs, vec![p(1, 2, "300")]);
}

#[test]
fn a_batch_is_matched_in_date_order_until_it_offsets() {
    let pairs = pair(vec![
        open(1, "1000", 1, None),
        open(2, "-300", 1, None),
        open(3, "-400", 1, None),
        open(4, "-500", 1, None),
        open(5, "200", 1, None),
    ]);
    // same date: amount order puts the credits -500, -400, -300 and the debits 200, 1000
    assert_eq!(
        pairs,
        vec![
            p(5, 4, "200"),
            p(1, 4, "300"),
            p(1, 3, "400"),
            p(1, 2, "300")
        ]
    );
    let total: Decimal = pairs.iter().map(|p| p.amount).sum();
    assert_eq!(total, d("1200"));
}

#[test]
fn several_partners_are_matched_partner_by_partner() {
    let pairs = pair(vec![
        open(1, "100", 1, Some(7)),
        open(2, "100", 2, Some(3)),
        open(3, "-100", 3, Some(7)),
        open(4, "-100", 4, Some(3)),
    ]);
    assert_eq!(pairs, vec![p(2, 4, "100"), p(1, 3, "100")]);
}

#[test]
fn settled_items_are_left_out() {
    assert!(pair(vec![open(1, "0", 1, None), open(2, "-5", 1, None)]).is_empty());
}

fn matched(types: &[&str], has_payment: bool) -> Settlement {
    Settlement {
        counterpart_types: types.iter().map(|t| (*t).to_owned()).collect(),
        has_payment,
    }
}

#[test]
fn the_status_follows_the_residual_and_the_counterparts() {
    assert_eq!(
        payment_state("out_invoice", false, &matched(&[], false)),
        "not_paid"
    );
    assert_eq!(
        payment_state("out_invoice", false, &matched(&["entry"], true)),
        "partial"
    );
    assert_eq!(
        payment_state("out_invoice", true, &matched(&["entry"], true)),
        "paid"
    );
    assert_eq!(
        payment_state("out_invoice", true, &matched(&["out_refund"], false)),
        "reversed"
    );
    assert_eq!(
        payment_state(
            "out_invoice",
            true,
            &matched(&["out_refund", "entry"], false)
        ),
        "reversed"
    );
    assert_eq!(
        payment_state("out_invoice", true, &matched(&["entry"], false)),
        "paid"
    );
    assert_eq!(
        payment_state("in_invoice", true, &matched(&["in_refund"], false)),
        "reversed"
    );
    assert_eq!(
        payment_state("in_refund", true, &matched(&["entry"], false)),
        "reversed"
    );
    assert_eq!(
        payment_state("out_invoice", true, &matched(&[], false)),
        "paid"
    );
}
