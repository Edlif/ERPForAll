// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit tests of the payments a registration plans (`payment::register`).
#![allow(clippy::unwrap_used, reason = "tests")]
use super::*;

fn d(v: &str) -> Decimal {
    v.parse().unwrap()
}

fn doc(id: i64, kind: &str, residual: &str, partner: i64) -> Document {
    Document {
        id,
        name: format!("DOC/{id}"),
        kind: kind.to_owned(),
        state: "posted".to_owned(),
        payment_state: "not_paid".to_owned(),
        company: 1,
        currency: 7,
        company_currency: 7,
        partner: Some(partner),
        residual: d(residual),
        account: Some(50),
    }
}

#[test]
fn a_grouped_registration_nets_credit_notes_and_lists_the_numbers() {
    let docs = [
        doc(3, "in_refund", "1600", 9),
        doc(1, "in_invoice", "1000", 9),
        doc(2, "in_invoice", "2000", 9),
    ];
    let planned = plan(&docs, true, d("1400"), d("1400"), None).unwrap();
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].amount, d("1400"));
    assert!(!planned[0].inbound, "a vendor is paid");
    assert!(!planned[0].customer);
    assert_eq!(planned[0].memo, "DOC/1, DOC/2, DOC/3");
}

#[test]
fn without_grouping_each_document_is_paid_in_full() {
    let docs = [
        doc(1, "out_invoice", "1000", 4),
        doc(2, "out_refund", "300", 4),
    ];
    assert!(
        plan(&docs, false, d("5"), d("700"), None)
            .unwrap_err()
            .message()
            .contains("2 payments would be made (one per document)")
    );
    let planned = plan(&docs, false, d("700"), d("700"), Some("ignored")).unwrap();
    assert_eq!(planned.len(), 2);
    assert_eq!((planned[0].amount, planned[0].inbound), (d("1000"), true));
    assert_eq!((planned[1].amount, planned[1].inbound), (d("300"), false));
    assert_eq!(planned[1].memo, "DOC/2");
}

#[test]
fn partners_and_refusals() {
    let two = [
        doc(1, "out_invoice", "100", 4),
        doc(2, "out_invoice", "50", 5),
    ];
    assert!(
        plan(&two, true, d("1"), d("150"), None)
            .unwrap_err()
            .message()
            .contains("The amount was changed, but 2 payments would be made")
    );
    let planned = plan(&two, true, d("150"), d("150"), None).unwrap();
    assert_eq!(
        planned.len(),
        2,
        "one payment per partner, each of its own amount due"
    );
    assert_eq!(planned[0].amount + planned[1].amount, d("150"));
    let even = [
        doc(1, "out_invoice", "100", 4),
        doc(2, "out_refund", "100", 4),
    ];
    assert!(
        plan(&even, true, d("0"), d("0"), None)
            .unwrap_err()
            .message()
            .contains("offset each other")
    );
    let mixed = [
        doc(1, "out_invoice", "100", 4),
        doc(2, "in_invoice", "100", 4),
    ];
    assert!(plan(&mixed, true, d("1"), d("0"), None).is_err());
    let mut paid = doc(1, "out_invoice", "0", 4);
    paid.payment_state = "paid".to_owned();
    assert!(
        plan(&[paid], true, d("1"), d("0"), None)
            .unwrap_err()
            .message()
            .contains("nothing left to pay")
    );
    assert!(
        plan(
            &[doc(1, "out_invoice", "100", 4)],
            true,
            d("0"),
            d("100"),
            None
        )
        .unwrap_err()
        .message()
        .contains("more than zero")
    );
    // a document paid in part since the dialog opened is not paid the old amount
    assert!(
        plan(
            &[doc(1, "out_invoice", "60", 4)],
            true,
            d("100"),
            d("100"),
            None
        )
        .unwrap_err()
        .message()
        .contains("What is due on DOC/1 changed since the dialog was opened (100 then, 60 now)")
    );
    // one document not grouped is one payment: its amount can be changed
    let one = plan(
        &[doc(1, "out_invoice", "100", 4)],
        false,
        d("40"),
        d("100"),
        None,
    )
    .unwrap();
    assert_eq!(one[0].amount, d("40"));
    let mut entry = doc(5, "entry", "10", 4);
    entry.name = "/".to_owned();
    assert!(
        plan(&[entry], true, d("10"), d("10"), None)
            .unwrap_err()
            .message()
            .starts_with("Draft Entry 5 is not an invoice")
    );
}
