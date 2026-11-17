// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit tests of the payment term arithmetic.
#![allow(clippy::unwrap_used, reason = "tests")]

use super::*;

fn d(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn day(text: &str) -> Date {
    Date::parse_odoo(text).unwrap()
}

fn inc(text: &str) -> Increment {
    Increment::new(d(text)).unwrap()
}

fn line(fixed: bool, value: &str, delay: Delay, nb_days: i32) -> TermLine {
    TermLine {
        fixed,
        value: d(value),
        delay,
        nb_days,
        days_next_month: Some("10".into()),
    }
}

fn percent(value: &str, nb_days: i32) -> TermLine {
    line(false, value, Delay::DaysAfter, nb_days)
}

fn split(lines: &[TermLine], total: &str) -> Vec<(String, Decimal, Decimal)> {
    let cent = inc("0.01");
    instalments(
        lines,
        day("2026-01-10"),
        d(total),
        d(total),
        Decimal::ONE,
        &cent,
        &cent,
    )
    .unwrap()
    .iter()
    .map(|i| (i.date.to_string(), i.balance, i.amount_currency))
    .collect()
}

#[test]
fn percent_lines_split_the_total_and_the_last_line_takes_the_rest() {
    let rows = split(&[percent("30", 0), percent("70", 30)], "100.01");
    assert_eq!(rows[0], ("2026-01-10".into(), d("30"), d("30")));
    assert_eq!(rows[1], ("2026-02-09".into(), d("70.01"), d("70.01")));
    let single = split(&[percent("100", 0)], "250.55");
    assert_eq!(single[0].1, d("250.55"));
}

#[test]
fn fixed_lines_take_their_amount_and_negative_amounts_are_allowed() {
    let rows = split(
        &[
            line(true, "-50", Delay::DaysAfter, 0),
            line(true, "200", Delay::DaysAfter, 0),
            percent("100", 0),
        ],
        "500",
    );
    let amounts: Vec<Decimal> = rows.iter().map(|r| r.1).collect();
    assert_eq!(amounts, [d("-50"), d("200"), d("350")]);
}

#[test]
fn a_fixed_line_in_a_foreign_currency_converts_through_the_ratio_of_the_totals() {
    // 200 EUR of 400 EUR against 100 USD: 100 EUR is 25 USD
    let out = instalments(
        &[line(true, "100", Delay::DaysAfter, 0), percent("100", 10)],
        day("2026-01-10"),
        d("100"),
        d("400"),
        Decimal::ONE,
        &inc("0.01"),
        &inc("0.01"),
    )
    .unwrap();
    assert_eq!(
        (out[0].balance, out[0].amount_currency),
        (d("25.00"), d("100.00"))
    );
    assert_eq!(
        (out[1].balance, out[1].amount_currency),
        (d("75.00"), d("300.00"))
    );
}

#[test]
fn a_fixed_line_of_a_credit_note_takes_the_sign() {
    let cent = inc("0.01");
    let out = instalments(
        &[line(true, "200", Delay::DaysAfter, 0), percent("100", 0)],
        day("2026-01-10"),
        d("-1000"),
        d("-1000"),
        -Decimal::ONE,
        &cent,
        &cent,
    )
    .unwrap();
    assert_eq!(out[0].balance, d("-200.00"));
    assert_eq!(out[1].balance, d("-800.00"));
}

#[test]
fn a_zero_total_gives_zero_instalments_and_refuses_a_fixed_line() {
    let rows = split(&[percent("30", 0), percent("70", 30)], "0");
    assert_eq!(rows[0].1, Decimal::ZERO);
    assert_eq!(rows[1].1, Decimal::ZERO);
    let cent = inc("0.01");
    let error = instalments(
        &[line(true, "10", Delay::DaysAfter, 0), percent("100", 0)],
        day("2026-01-10"),
        Decimal::ZERO,
        Decimal::ZERO,
        Decimal::ONE,
        &cent,
        &cent,
    )
    .unwrap_err();
    assert!(
        error.message().contains("fixed payment term line"),
        "{error:?}"
    );
}

#[test]
fn the_instalments_always_add_up_to_the_total() {
    let rows = split(
        &[
            percent("33.333333", 0),
            percent("33.333333", 0),
            percent("33.333334", 0),
        ],
        "100.00",
    );
    let sum: Decimal = rows.iter().map(|r| r.1).sum();
    assert_eq!(sum, d("100.00"));
}

fn dated(delay: Delay, nb_days: i32, next: Option<&str>, from: &str) -> String {
    let mut l = line(false, "100", delay, nb_days);
    l.days_next_month = next.map(Into::into);
    due_date(&l, day(from)).unwrap().to_string()
}

#[test]
fn the_delay_kinds_count_from_the_reference_date() {
    assert_eq!(
        dated(Delay::DaysAfter, 30, None, "2026-01-31"),
        "2026-03-02"
    );
    assert_eq!(
        dated(Delay::AfterEndOfMonth, 5, None, "2026-02-10"),
        "2026-03-05"
    );
    assert_eq!(
        dated(Delay::AfterEndOfNextMonth, 0, None, "2026-12-15"),
        "2027-01-31"
    );
}

#[test]
fn the_day_of_the_next_month_is_clamped_and_falls_back() {
    assert_eq!(
        dated(Delay::EndOfMonthOn, 10, Some("10"), "2026-01-25"),
        "2026-03-10"
    );
    assert_eq!(
        dated(Delay::EndOfMonthOn, 0, Some("0"), "2026-01-25"),
        "2026-01-31"
    );
    assert_eq!(
        dated(Delay::EndOfMonthOn, 0, Some("ab"), "2026-01-25"),
        "2026-02-01"
    );
    assert_eq!(
        dated(Delay::EndOfMonthOn, 0, Some("31"), "2026-01-25"),
        "2026-02-28"
    );
    assert_eq!(
        dated(Delay::EndOfMonthOn, 0, Some("31"), "2028-01-25"),
        "2028-02-29"
    );
    assert_eq!(
        dated(Delay::AfterEndOfMonth, 0, None, "2028-02-03"),
        "2028-02-29"
    );
    assert_eq!(
        dated(Delay::EndOfMonthOn, 0, Some("5"), "2026-12-20"),
        "2027-01-05"
    );
}

#[test]
fn percent_lines_must_add_up_to_one_hundred() {
    let ok = [percent("33.333", 0), percent("66.667", 30)];
    check_percent_lines(&ok, 3).unwrap();
    assert!(check_percent_lines(&[percent("95", 0)], 6).is_err());
    assert!(check_percent_lines(&[], 6).is_err());
    assert!(check_percent_lines(&[line(true, "100", Delay::DaysAfter, 0)], 6).is_err());
    let error = check_percent_lines(&[percent("105", 0), percent("-5", 0)], 6).unwrap_err();
    assert!(error.message().contains("from 0 to 100"), "{error:?}");
}

#[test]
fn the_day_of_the_next_month_is_a_number_from_zero_to_thirty_one() {
    assert!(check_days_next_month(Some("0")).is_ok());
    assert!(check_days_next_month(Some("31")).is_ok());
    for bad in [Some("ab"), Some(""), None] {
        let e = check_days_next_month(bad).unwrap_err();
        assert!(e.message().contains("not a whole number"), "{e:?}");
    }
    for bad in ["32", "-5"] {
        let e = check_days_next_month(Some(bad)).unwrap_err();
        assert!(
            e.message().contains("from 0 to 31") && !e.message().contains("not a whole number"),
            "{e:?}"
        );
    }
}
