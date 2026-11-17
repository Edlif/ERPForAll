// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit tests of the payment status (`payment::service`).
use super::*;

#[test]
fn the_status_follows_the_liquidity_item_and_the_documents() {
    let paid = Documents {
        invoices: (2, 2),
        bills: (0, 0),
    };
    let open = Documents {
        invoices: (2, 1),
        bills: (0, 0),
    };
    assert_eq!(payment_state(true, false, None), "reconciled");
    assert_eq!(payment_state(false, false, None), "paid");
    assert_eq!(payment_state(false, false, Some(&paid)), "reconciled");
    assert_eq!(payment_state(false, false, Some(&open)), "paid");
    assert_eq!(payment_state(false, true, Some(&paid)), "paid");
}
