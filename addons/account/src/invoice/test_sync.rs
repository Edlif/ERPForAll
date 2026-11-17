// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit tests of the invoice line sync helpers.
#![allow(clippy::unwrap_used, reason = "tests")]

use super::*;
use erp_sdk::ErrorKind;

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
        stored: Totals::default(),
    };
    assert_eq!(due_date(&invoice), Some(day("2026-11-16")));
    invoice.due = Some(day("2026-12-01"));
    assert_eq!(due_date(&invoice), Some(day("2026-12-01")));
    invoice.due = Some(day("2026-11-01"));
    assert_eq!(due_date(&invoice), Some(day("2026-11-16")));
}

fn candidates(accounts: &[(i64, &str)]) -> Vec<(i64, Option<Arc<str>>)> {
    accounts
        .iter()
        .map(|(id, code)| (*id, Some(Arc::<str>::from(*code))))
        .collect()
}

/// The only receivable or payable account is used without a chosen code.
#[test]
fn the_only_account_of_the_type_is_taken_without_a_setting() {
    let one = candidates(&[(7, "121000")]);
    assert_eq!(
        choose_account("asset_receivable", 3, &one, None).unwrap(),
        7
    );
    assert_eq!(
        choose_account("liability_payable", 3, &one, Some("999999")).unwrap(),
        7
    );
}

/// Among several accounts the one with the chosen code is used, whatever its position.
#[test]
fn the_chosen_code_picks_among_several_accounts() {
    let several = candidates(&[(7, "121000"), (8, "121100"), (9, "121200")]);
    assert_eq!(
        choose_account("asset_receivable", 3, &several, Some("121100")).unwrap(),
        8
    );
    assert_eq!(
        choose_account("liability_payable", 3, &several, Some("121200")).unwrap(),
        9
    );
}

/// Several accounts without a chosen code, or with a code none of them has, are refused with the
/// codes to choose from and the setting to enter the choice in.
#[test]
fn several_accounts_without_a_valid_choice_are_refused() {
    let several = candidates(&[(7, "121000"), (8, "121100")]);
    for (configured, problem) in [
        (None, "none is chosen"),
        (Some(""), "none is chosen"),
        (Some("400000"), "the chosen code 400000 is not one of them"),
    ] {
        let error = choose_account("asset_receivable", 3, &several, configured).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UserError, "{configured:?}");
        let message = error.message();
        assert!(
            message.contains("Company 3 has several receivable accounts (121000, 121100)"),
            "{message}"
        );
        assert!(message.contains(problem), "{message}");
        assert!(message.contains(settings::BLOCK), "{message}");
        assert!(message.contains(settings::RECEIVABLE_LABEL), "{message}");
    }
    let error = choose_account("liability_payable", 3, &several, None).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert!(
        error.message().contains(settings::PAYABLE_LABEL),
        "{error:?}"
    );
    // an account without a code cannot be chosen by code and is named by its id
    let uncoded = vec![(7, Some(Arc::<str>::from("121000"))), (8, None)];
    let error = choose_account("asset_receivable", 3, &uncoded, None).unwrap_err();
    assert!(
        error.message().contains("121000, account 8 without a code"),
        "{error:?}"
    );
}

/// No account of the type is refused naming what to create; an account type that is neither
/// receivable nor payable is a broken call.
#[test]
fn no_account_of_the_type_is_refused() {
    let error = choose_account("liability_payable", 3, &[], None).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert!(
        error.message().contains("Company 3 has no payable account"),
        "{error:?}"
    );
    let error = choose_account("income", 3, &[], None).unwrap_err();
    assert!(error.is_invariant_violation(), "{error:?}");
}

/// A company's parent path lists its parents then itself; a path that does not is refused.
#[test]
fn the_parent_path_gives_the_company_and_its_parents() {
    assert_eq!(company_with_parents(9, Some("1/4/9/")).unwrap(), [1, 4, 9]);
    assert_eq!(company_with_parents(1, Some("1/")).unwrap(), [1]);
    assert_eq!(company_with_parents(5, None).unwrap(), [5]);
    for path in ["1/x/9/", "1/4/", "1//9/"] {
        let error = company_with_parents(9, Some(path)).unwrap_err();
        assert!(error.is_invariant_violation(), "{path}: {error:?}");
        assert!(error.message().contains(path), "{error:?}");
    }
}

/// An unexpected kind of cell is refused naming the field, never read as empty.
#[test]
fn an_unexpected_cell_kind_is_refused_by_field() {
    let wrong = Cell::Int(5);
    let error = text(&wrong, "display_type").unwrap_err();
    assert!(error.is_invariant_violation(), "{error:?}");
    assert!(error.message().contains("display_type"), "{error:?}");
    assert!(
        date(&wrong, "date_maturity")
            .unwrap_err()
            .is_invariant_violation()
    );
    assert!(
        flag(&wrong, "price_include")
            .unwrap_err()
            .is_invariant_violation()
    );
    assert!(ids(&wrong, "tax_ids").unwrap_err().is_invariant_violation());
    assert!(
        int(&Cell::Str("1".into()), "sequence")
            .unwrap_err()
            .is_invariant_violation()
    );
    assert!(
        nth(&[], 0, "tax_id")
            .unwrap_err()
            .message()
            .contains("tax_id column is short")
    );
    // empty cells stay empty
    assert_eq!(text(&Cell::Null, "name").unwrap(), None);
    assert_eq!(date(&Cell::Bool(false), "date").unwrap(), None);
    assert!(!flag(&Cell::Null, "price_include").unwrap());
    assert!(ids(&Cell::Null, "tax_ids").unwrap().is_empty());
    assert_eq!(int(&Cell::Null, "sequence").unwrap(), 0);
}

/// A header whose stored totals, amount due and payment state already match is not rewritten.
#[test]
fn a_matching_header_is_recognised() {
    let d = |v: &str| v.parse::<Decimal>().unwrap();
    let stored = Totals {
        untaxed: Some(d("100.00")),
        tax: Some(d("15")),
        total: Some(d("115")),
        residual: Some(d("115")),
        payment_state: Some("not_paid".into()),
    };
    assert!(header_matches(&stored, d("100"), d("15"), d("115")));
    assert!(!header_matches(&stored, d("100"), d("16"), d("116")));
    // nothing stored yet is not a stored zero: the first sync writes the header
    assert!(!header_matches(
        &Totals::default(),
        Decimal::ZERO,
        Decimal::ZERO,
        Decimal::ZERO
    ));
}
