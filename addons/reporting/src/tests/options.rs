// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

fn day(y: i32, m: u32, d: u32) -> Date {
    Date::from_ymd(y, m, d).unwrap()
}
fn range(from: (i32, u32, u32), to: (i32, u32, u32)) -> Period {
    Period {
        from: Some(day(from.0, from.1, from.2)),
        to: day(to.0, to.1, to.2),
    }
}

#[test]
fn presets_in_range_mode() {
    let today = day(2026, 11, 19);
    for (preset, expected) in [
        (Preset::ThisMonth, range((2026, 11, 1), (2026, 11, 30))),
        (Preset::LastMonth, range((2026, 10, 1), (2026, 10, 31))),
        (Preset::ThisQuarter, range((2026, 10, 1), (2026, 12, 31))),
        (Preset::LastQuarter, range((2026, 7, 1), (2026, 9, 30))),
        (Preset::ThisYear, range((2026, 1, 1), (2026, 12, 31))),
        (Preset::LastYear, range((2025, 1, 1), (2025, 12, 31))),
    ] {
        assert_eq!(
            preset_period(preset, DateMode::Range, today),
            Some(expected),
            "{preset:?}"
        );
    }
    assert_eq!(
        preset_period(Preset::LastQuarter, DateMode::Range, day(2026, 2, 3)),
        Some(range((2025, 10, 1), (2025, 12, 31)))
    );
    assert!(!Preset::Today.offered(DateMode::Range));
}

#[test]
fn single_date_mode_has_only_an_end() {
    let today = day(2026, 11, 19);
    assert_eq!(
        preset_period(Preset::Today, DateMode::Single, today),
        Some(Period {
            from: None,
            to: today
        })
    );
    assert_eq!(
        preset_period(Preset::ThisMonth, DateMode::Single, today),
        Some(Period {
            from: None,
            to: day(2026, 11, 30)
        })
    );
}

#[test]
fn previous_periods_follow_whole_months_or_the_length_in_days() {
    let november = range((2026, 11, 1), (2026, 11, 30));
    assert_eq!(
        compare(november, ComparisonMode::PreviousPeriod, 1),
        Some(range((2026, 10, 1), (2026, 10, 31)))
    );
    assert_eq!(
        compare(november, ComparisonMode::PreviousPeriod, 2),
        Some(range((2026, 9, 1), (2026, 9, 30)))
    );
    let quarter = range((2026, 10, 1), (2026, 12, 31));
    assert_eq!(
        compare(quarter, ComparisonMode::PreviousPeriod, 1),
        Some(range((2026, 7, 1), (2026, 9, 30)))
    );
    let ten_days = range((2026, 3, 11), (2026, 3, 20));
    assert_eq!(
        compare(ten_days, ComparisonMode::PreviousPeriod, 1),
        Some(range((2026, 3, 1), (2026, 3, 10)))
    );
    let end = Period {
        from: None,
        to: day(2026, 11, 30),
    };
    assert_eq!(
        compare(end, ComparisonMode::PreviousPeriod, 1),
        Some(Period {
            from: None,
            to: day(2026, 10, 31)
        })
    );
}

#[test]
fn same_period_last_year_keeps_month_ends() {
    let november = range((2026, 11, 1), (2026, 11, 30));
    assert_eq!(
        compare(november, ComparisonMode::SameLastYear, 1),
        Some(range((2025, 11, 1), (2025, 11, 30)))
    );
    let february = range((2028, 2, 1), (2028, 2, 29));
    assert_eq!(
        compare(february, ComparisonMode::SameLastYear, 1),
        Some(range((2027, 2, 1), (2027, 2, 28)))
    );
    assert_eq!(compare(november, ComparisonMode::Custom, 1), None);
}
