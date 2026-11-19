// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

fn value(
    text: &str,
    values: &[(&str, &str, i64)],
    children: Option<i64>,
) -> std::result::Result<Option<Decimal>, EvalError> {
    let term = parse(text).unwrap();
    let lookup = |r: &Ref| {
        values
            .iter()
            .find(|(code, label, _)| r.code.as_deref().unwrap_or("") == *code && r.label == *label)
            .map(|(_, _, n)| Decimal::from(*n))
    };
    term.eval(&lookup, children.map(Decimal::from))
}

#[test]
fn formulas_read_other_lines_their_own_labels_and_children() {
    let values = [
        ("IN", "qty", 10),
        ("OUT", "qty", 4),
        ("", "on_hand", 9),
        ("", "reserved", 2),
    ];
    let n = |v: i64| Ok(Some(Decimal::from(v)));
    assert_eq!(value("IN.qty - OUT.qty", &values, None), n(6));
    assert_eq!(value("on_hand - reserved", &values, None), n(7));
    assert_eq!(value("sum_children * 2", &values, Some(5)), n(10));
    assert_eq!(value("1 + 2 * 3 - -(4 / 2)", &values, None), n(9));
    // an empty value makes the result empty, never zero
    assert_eq!(value("IN.missing + 1", &values, None), Ok(None));
    assert_eq!(
        value("IN.qty / (OUT.qty - 4)", &values, None),
        Err(EvalError::DivisionByZero)
    );

    let (mut refs, mut children) = (Vec::new(), false);
    parse("IN.qty + on_hand + sum_children")
        .unwrap()
        .refs(&mut refs, &mut children);
    assert!(children);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].code.as_deref(), Some("IN"));

    for bad in ["1 +", "In.qty", "IN.Qty", "(1", "1 2", "a ; b"] {
        let error = parse(bad).unwrap_err();
        assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation, "{bad}");
        assert!(
            error.message().starts_with(&format!("Formula '{bad}'")),
            "{}",
            error.message()
        );
    }
    assert!(
        parse(&"(".repeat(65))
            .unwrap_err()
            .message()
            .contains("nesting deeper than 64")
    );
    assert!(
        parse(&"1+".repeat(300))
            .unwrap_err()
            .message()
            .contains("operators in a row")
    );
}
