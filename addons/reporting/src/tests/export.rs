// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::line::{LineId, Segment};

fn line(path: &[Segment], name: &str, kind: LineKind, value: i64) -> Line {
    let id = path
        .iter()
        .fold(LineId::root(1), |id, segment| id.child(segment.clone()));
    let cell = Cell::number(CellKind::Integer, Some(Decimal::from(value)), 0);
    Line::new(id, name, kind, vec![cell])
}

/// Stock > WH/Stock > [Desk 4, Chair 0]; Empty > [Bin 0]; a load-more line closes WH/Stock.
fn tree() -> Vec<Line> {
    let stock = [Segment::line("STOCK")];
    let wh = [stock[0].clone(), Segment::group("loc", "res.loc", "1")];
    let desk = [
        wh[0].clone(),
        wh[1].clone(),
        Segment::group("name", "", "desk"),
    ];
    let chair = [
        wh[0].clone(),
        wh[1].clone(),
        Segment::group("name", "", "chair"),
    ];
    let more = [wh[0].clone(), wh[1].clone(), Segment::more()];
    let empty = [Segment::line("EMPTY")];
    let bin = [empty[0].clone(), Segment::group("loc", "res.loc", "2")];
    vec![
        line(&stock, "Stock", LineKind::Section, 4),
        line(&wh, "WH/Stock", LineKind::Group, 4),
        line(&desk, "[P1] Desk", LineKind::Detail, 4),
        line(&chair, "[P2] Chair", LineKind::Detail, 0),
        line(&more, "3 more", LineKind::More, 0),
        line(&empty, "Empty", LineKind::Section, 0),
        line(&bin, "Bin", LineKind::Group, 0),
    ]
}

fn names(lines: &[Line], shown: &[bool]) -> Vec<String> {
    lines
        .iter()
        .zip(shown)
        .filter(|(_, keep)| **keep)
        .map(|(line, _)| line.name.clone())
        .collect()
}

#[test]
fn hide_zero_keeps_lines_with_a_non_zero_value_below() {
    let lines = tree();
    assert_eq!(
        names(&lines, &visible(&lines, true, "")),
        ["Stock", "WH/Stock", "[P1] Desk", "3 more"]
    );
    assert_eq!(names(&lines, &visible(&lines, false, "")).len(), 7);
}

#[test]
fn unknown_descendants_keep_blank_groups_reachable() {
    let mut lines = tree();
    for line in &mut lines {
        line.cells.clear();
    }
    lines[5].unfoldable = true;
    assert_eq!(
        names(&lines, &visible(&lines, true, "")),
        ["Stock", "WH/Stock", "3 more", "Empty"]
    );
}

#[test]
fn search_keeps_matches_with_ancestors_and_descendants() {
    let lines = tree();
    assert_eq!(
        names(&lines, &visible(&lines, false, "DESK")),
        ["Stock", "WH/Stock", "[P1] Desk", "3 more"]
    );
    // a match on a group keeps what is loaded below it
    assert_eq!(
        names(&lines, &visible(&lines, false, "wh/")),
        ["Stock", "WH/Stock", "[P1] Desk", "[P2] Chair", "3 more"]
    );
    assert!(names(&lines, &visible(&lines, false, "nothing")).is_empty());
    // both rules together: the zero match is hidden, its non-zero ancestors stay
    assert_eq!(
        names(&lines, &visible(&lines, true, "chair")),
        ["Stock", "WH/Stock", "3 more"]
    );
}

#[test]
fn a_request_has_exactly_report_options_and_format() {
    let ok = parse(br#"{"report": 3, "options": {"hide_zero": true}, "format": "xlsx"}"#).unwrap();
    assert_eq!((ok.report, ok.format), (3, Format::Xlsx));
    assert_eq!(ok.options["report"], 3);
    for (body, text) in [
        (
            r#"{"report":3,"options":{},"format":"pdf","lines":[]}"#,
            "'lines' is not part",
        ),
        (
            r#"{"report":3,"options":{},"format":"csv"}"#,
            "format must be",
        ),
        (
            r#"{"report":3,"options":[],"format":"pdf"}"#,
            "options must be an object",
        ),
        (r#"{"options":{},"format":"pdf"}"#, "report must be the id"),
        (
            r#"{"report":3,"options":{"report":4},"format":"pdf"}"#,
            "not the report to export",
        ),
        (r#"[1]"#, "must be an object"),
        (r#"{"#, "not JSON"),
    ] {
        let error = parse(body.as_bytes()).unwrap_err();
        assert!(
            error.message().contains(text),
            "{body}: {}",
            error.message()
        );
    }
}

#[test]
fn rows_over_the_setting_are_refused_by_name() {
    let bounds = Bounds {
        max_filter_source_rows: 1000,
        max_load_more_limit: 500,
        max_unfold_depth: 8,
        max_screen_lines: 10_000,
        max_comparison_periods: 12,
        export_max_rows: 100,
        export_max_request_bytes: 1_048_576,
    };
    assert!(check_rows(100, &bounds).is_ok());
    let error = check_rows(101, &bounds).unwrap_err();
    assert!(
        error.message().contains("reporting.export_max_rows"),
        "{}",
        error.message()
    );
}
