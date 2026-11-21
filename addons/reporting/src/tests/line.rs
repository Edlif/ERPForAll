// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn line_ids_round_trip_and_bad_ones_are_refused_by_segment() {
    let id = LineId::root(7)
        .child(Segment::line("STOCK"))
        .child(Segment::group("name", "", "a|b~c%d"));
    let text = id.to_string();
    assert_eq!(
        text,
        "root~reporting.report~7|line:STOCK~~|group:name~~a%7Cb%7Ec%25d"
    );
    let parsed = LineId::parse(&text).unwrap();
    assert_eq!(parsed, id);
    assert_eq!(parsed.segments()[2].value(), "a|b~c%d");
    assert_eq!((parsed.report(), parsed.level()), (7, 1));
    assert!(parsed.has_group("name"));

    let refused = |text: &str| LineId::parse(text).unwrap_err();
    let long = format!(
        "root~reporting.report~1|line:A~~{}",
        "x".repeat(MAX_LINE_ID_BYTES)
    );
    assert!(refused(&long).message().contains("longer than 2048 bytes"));
    for (text, segment, what) in [
        ("root~reporting.report~1|line:A~~%G1", 2, "malformed escape"),
        ("root~reporting.report~1|bogus~~", 2, "unknown kind"),
        ("line:A~~|root~reporting.report~1", 1, "root of a report"),
        ("root~reporting.report~0", 1, "root of a report"),
        ("root~res.partner~1", 1, "root of a report"),
        ("root~reporting.report~1|line:A~", 2, "kind~model~value"),
        (
            "root~reporting.report~1|group:x~Bad Model~1",
            2,
            "malformed model",
        ),
    ] {
        let error = refused(text);
        assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
        assert!(
            error.message().contains(&format!("segment {segment} ")),
            "{text}: {}",
            error.message()
        );
        assert!(
            error.message().contains(what),
            "{text}: {}",
            error.message()
        );
    }
}

#[test]
fn zero_is_decided_at_the_cells_precision() {
    let small = Some(Decimal::new(4, 3));
    let money = Cell::monetary(small, 2, Some(1));
    let decimal = Cell::number(CellKind::Decimal, small, 3);
    assert!(money.is_zero());
    assert!(!decimal.is_zero());
    assert_eq!(money.to_json()["value"], "0.00");
    assert!(money.to_json().get("drillable").is_none());
    assert_eq!(decimal.to_json()["value"], "0.004");
    assert_eq!(money.clone().blank_if_zero().value(), None);
    assert_eq!(
        money.blank_if_zero().with_drillable(true).to_json()["drillable"],
        false
    );
    // text and empty cells do not make a line non-zero
    assert!(Cell::text("Desk").is_zero() && Cell::empty().is_zero());
}

#[test]
fn cell_drill_capability_is_optional_and_generated_values_do_not_inherit_it() {
    let legacy = Cell::number(CellKind::Decimal, Some(Decimal::ONE), 2);
    assert!(legacy.to_json().get("drillable").is_none());

    let mut declared = legacy.clone().with_drillable(true);
    assert_eq!(declared.to_json()["drillable"], true);
    declared.suppress_declared_drill();
    assert_eq!(declared.to_json()["drillable"], false);
    assert_eq!(declared.to_json()["value"], legacy.to_json()["value"]);

    let mut unannotated = legacy;
    unannotated.suppress_declared_drill();
    assert!(unannotated.to_json().get("drillable").is_none());
    assert_eq!(
        Cell::empty().with_drillable(true).to_json()["drillable"],
        false
    );
}

#[test]
fn drill_targets_are_an_item_or_a_cell() {
    assert_eq!(
        DrillTarget::parse(&json!({"item": "open"})).unwrap(),
        DrillTarget::Item("open".into())
    );
    assert_eq!(
        DrillTarget::parse(&json!({"cell": 3})).unwrap(),
        DrillTarget::Cell(3)
    );
    for bad in [
        json!({"cell": -1}),
        json!({"item": "a b"}),
        json!({"item": "open", "cell": 1}),
        json!([]),
    ] {
        assert!(DrillTarget::parse(&bad).is_err(), "{bad}");
    }
}
