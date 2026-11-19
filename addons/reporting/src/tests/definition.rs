// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

fn line(
    code: &str,
    parent: Option<usize>,
    children: Vec<usize>,
    formulas: &[(&str, &str)],
) -> LineDef {
    LineDef {
        code: code.to_owned(),
        name: code.to_owned(),
        parent,
        children,
        groupby: Vec::new(),
        group: Vec::new(),
        foldable: Foldable::OnDemand,
        hide_if_zero: false,
        optional: false,
        exprs: formulas
            .iter()
            .map(|(label, text)| ExprDef {
                label: (*label).to_owned(),
                engine: Engine::Formula {
                    term: formula::parse(text).unwrap(),
                    zero_empty: false,
                },
            })
            .collect(),
    }
}

#[test]
fn formulas_are_ordered_and_cycles_name_their_lines() {
    let lines = vec![
        line("NET", None, vec![], &[("qty", "IN.qty - OUT.qty")]),
        line("IN", None, vec![], &[("qty", "2")]),
        line("OUT", None, vec![], &[("qty", "1")]),
    ];
    let order = evaluation_order(&lines).unwrap();
    let net = order.iter().position(|(l, _)| *l == 0).unwrap();
    assert!(order.iter().position(|(l, _)| *l == 1).unwrap() < net);
    assert!(order.iter().position(|(l, _)| *l == 2).unwrap() < net);

    let cycle = vec![
        line("A", None, vec![], &[("qty", "B.qty + 1")]),
        line("B", None, vec![], &[("qty", "A.qty")]),
        line("C", None, vec![], &[("qty", "1")]),
    ];
    assert_eq!(
        evaluation_order(&cycle).unwrap_err(),
        "the formulas of A.qty, B.qty form a cycle."
    );
    let own = vec![
        line("P", None, vec![1], &[("qty", "sum_children")]),
        line("K", Some(0), vec![], &[("qty", "P.qty")]),
    ];
    assert!(evaluation_order(&own).unwrap_err().contains("P.qty, K.qty"));
}

#[test]
fn codes_and_labels() {
    assert!(is_code("STOCK_1") && !is_code("stock") && !is_code("1A") && !is_code(""));
    assert!(is_label("on_hand") && !is_label("OnHand") && !is_label("1a"));
}
