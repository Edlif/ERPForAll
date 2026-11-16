// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit tests of the invoice line sync helpers.
#![allow(clippy::unwrap_used, reason = "tests")]

use super::*;

fn rep(id: i64, factor: &str) -> Repartition {
    Repartition {
        id,
        tax: 1,
        refund: false,
        factor: factor.parse().unwrap(),
        account: None,
        sequence: 1,
    }
}

#[test]
fn a_split_tax_adds_up_to_the_rounded_whole() {
    let increment = Increment::new("0.01".parse().unwrap()).unwrap();
    let reps = [rep(1, "50"), rep(2, "50")];
    // 0.05 split in halves: 0.025 -> 0.03 each would give 0.06; the first part takes -0.01
    let parts = distribute("0.05".parse().unwrap(), reps.iter(), &increment).unwrap();
    let amounts: Vec<String> = parts.iter().map(|(_, a)| a.to_string()).collect();
    assert_eq!(amounts, ["0.02", "0.03"]);
}

#[test]
fn the_due_date_follows_the_invoice_date_until_set_later() {
    let day = |d: &str| Date::parse_odoo(d).unwrap();
    let mut invoice = Invoice {
        id: 1,
        move_type: "out_invoice".into(),
        company: 1,
        currency: 1,
        journal: 1,
        partner: None,
        invoice_date: Some(day("2026-11-16")),
        due: None,
        date: Some(day("2026-11-10")),
    };
    assert_eq!(due_date(&invoice), Some(day("2026-11-16")));
    invoice.due = Some(day("2026-12-01"));
    assert_eq!(due_date(&invoice), Some(day("2026-12-01")));
    invoice.due = Some(day("2026-11-01"));
    assert_eq!(due_date(&invoice), Some(day("2026-11-16")));
}
