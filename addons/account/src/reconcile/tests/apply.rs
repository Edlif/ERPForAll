// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit tests of the match graph (`reconcile::apply`).
#![allow(clippy::unwrap_used, reason = "tests")]
use super::*;

fn graph(edges: &[(i64, i64, i64)]) -> Graph {
    let mut g = Graph::default();
    for &(id, d, c) in edges {
        g.edges.insert(id, (d, c));
        g.lines.insert(d);
        g.lines.insert(c);
    }
    g
}

#[test]
fn matches_group_items_into_components() {
    let chain = graph(&[(10, 1, 2), (11, 1, 3), (12, 4, 3)]);
    assert_eq!(
        chain.components(),
        vec![(vec![1, 2, 3, 4], vec![10, 11, 12])]
    );
    let split = graph(&[(11, 1, 3), (12, 4, 3), (20, 5, 6)]);
    let parts = split.components();
    assert_eq!(parts.len(), 2);
    assert!(parts.contains(&(vec![1, 3, 4], vec![11, 12])));
    assert!(parts.contains(&(vec![5, 6], vec![20])));
}
