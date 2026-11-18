// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! A table cell that spans columns or rows crosses the worker frame (protocol 6). The journal entry's
//! Total row (`colspan="3"`) and every section and note line of a quotation, order or invoice are such
//! cells; before protocol 6 the server refused to print them (C33-FIX-PRINT-CELL).
use erp_report::{
    DocTree, DocumentBuilder, DocumentOptions, Limits, Node, PageSetup, ReportError, render_html,
};
use erp_report_worker::WireDocument;

/// A three-column table: a header row, a row whose first cell spans all three columns, a row of
/// three cells, then a first column whose cell spans two rows beside two single cells each.
fn spanning_table() -> DocTree {
    fn text(b: &mut DocumentBuilder, s: &str) -> erp_report::NodeId {
        b.add(Node::Text(s.into())).unwrap()
    }
    let mut b = DocumentBuilder::new(Limits::default());
    let header = ["Account", "Debit", "Credit"]
        .map(|s| text(&mut b, s))
        .to_vec();
    let total = text(&mut b, "Total");
    let total = b
        .add(Node::Cell {
            child: total,
            colspan: 3,
            rowspan: 1,
        })
        .unwrap();
    let row = ["400000", "10.00", "0.00"].map(|s| text(&mut b, s));
    let tall = text(&mut b, "Section");
    let tall = b
        .add(Node::Cell {
            child: tall,
            colspan: 1,
            rowspan: 2,
        })
        .unwrap();
    let rest = ["a", "b", "c", "d"].map(|s| text(&mut b, s));
    let mut cells = vec![total];
    cells.extend(row);
    cells.push(tall);
    cells.extend(rest);
    let table = b
        .add(Node::Table {
            columns: 3,
            header,
            cells,
        })
        .unwrap();
    let options = DocumentOptions::new("c", "C", PageSetup::new(595.0, 842.0, 40.0).unwrap());
    b.finish(table, options).unwrap()
}

fn spans(tree: &DocTree) -> Vec<(String, usize, usize)> {
    tree.nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Cell {
                child,
                colspan,
                rowspan,
            } => match &tree.nodes()[child.index()] {
                Node::Text(text) => Some((text.clone(), *colspan, *rowspan)),
                other => panic!("the cell holds its text, not {other:?}"),
            },
            _ => None,
        })
        .collect()
}

#[test]
fn a_spanning_table_cell_survives_the_worker_frame() {
    let tree = spanning_table();
    let back = WireDocument::from_tree(&tree)
        .unwrap()
        .into_tree(Limits::default())
        .unwrap();
    assert_eq!(
        spans(&back),
        vec![("Total".into(), 3, 1), ("Section".into(), 1, 2)]
    );
    let html = render_html(&back, &[]).unwrap();
    assert_eq!(html, render_html(&tree, &[]).unwrap());
    assert!(html.contains("colspan=\"3\""), "{html}");
    assert!(html.contains("rowspan=\"2\""), "{html}");
    // spanning cells came with protocol 6; 7 added the optional font packages, 8 the fit of a picture and
    // placeholder boxes, 9 barcodes and QR codes
    assert_eq!(erp_report_worker::PROTOCOL_VERSION, 9);
}

/// The CBOR value of a frame with the colspan of its first spanning cell replaced by `colspan`.
fn with_colspan(tree: &DocTree, colspan: u64) -> Vec<u8> {
    use ciborium::Value;
    let mut value = Value::serialized(&WireDocument::from_tree(tree).unwrap()).unwrap();
    let Value::Map(fields) = &mut value else {
        panic!("a document frame is a map");
    };
    let Some((_, Value::Array(nodes))) = fields
        .iter_mut()
        .find(|(key, _)| key.as_text() == Some("nodes"))
    else {
        panic!("a document frame lists its nodes");
    };
    let cell = nodes
        .iter_mut()
        .find_map(|node| match node {
            Value::Map(variant) => match variant.as_mut_slice() {
                [(Value::Text(tag), Value::Array(fields))] if tag == "s" => Some(fields),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("the frame carries the spanning cell under the tag s"));
    cell[1] = Value::Integer(colspan.into());
    let mut encoded = Vec::new();
    ciborium::into_writer(&value, &mut encoded).unwrap();
    encoded
}

/// A frame the worker did not build itself is admitted by the same rules as a document built in
/// the server process: a span past the last column, a zero span and a span that overflows are each
/// refused, never laid out.
#[test]
fn a_frame_with_an_out_of_range_span_is_refused() {
    let tree = spanning_table();
    for (colspan, reason) in [
        (4, "table cell spans past the last column"),
        (0, "table cell spans must be at least 1"),
        (u64::MAX, "table cell spans past the last column"),
    ] {
        let frame: WireDocument<'static> =
            ciborium::from_reader(with_colspan(&tree, colspan).as_slice()).unwrap();
        match frame.into_tree(Limits::default()) {
            Err(ReportError::InvalidDocument { reason: got }) => {
                assert_eq!(got, reason, "colspan {colspan}")
            }
            other => panic!("colspan {colspan} must be refused, got {other:?}"),
        }
    }
}

/// One node of a hand-written frame, in the frame's own CBOR form.
enum N {
    Text,
    Cell(u64, u64, u64),
    Flow(Vec<u64>),
    Table(u64, Vec<u64>),
}

/// A frame that no builder wrote: `nodes` in order, the body at index `body`. Everything else is
/// taken from the frame of a one-text document.
fn frame(nodes: &[N], body: u64) -> WireDocument<'static> {
    use ciborium::Value;
    let int = |n: &u64| Value::Integer((*n).into());
    let ints = |ns: &[u64]| Value::Array(ns.iter().map(int).collect());
    let tagged = |tag: &str, value: Value| Value::Map(vec![(Value::Text(tag.into()), value)]);
    let mut b = DocumentBuilder::new(Limits::default());
    let text = b.add(Node::Text("x".into())).unwrap();
    let options = DocumentOptions::new("f", "F", PageSetup::new(595.0, 842.0, 40.0).unwrap());
    let tree = b.finish(text, options).unwrap();
    let mut value = Value::serialized(&WireDocument::from_tree(&tree).unwrap()).unwrap();
    let Value::Map(fields) = &mut value else {
        panic!("a document frame is a map");
    };
    for (key, field) in fields.iter_mut() {
        match key.as_text() {
            Some("nodes") => {
                *field = Value::Array(
                    nodes
                        .iter()
                        .map(|node| match node {
                            N::Text => tagged("t", Value::Text("x".into())),
                            N::Cell(child, colspan, rowspan) => tagged(
                                "s",
                                Value::Array(vec![int(child), int(colspan), int(rowspan)]),
                            ),
                            N::Flow(children) => tagged("f", ints(children)),
                            N::Table(columns, cells) => tagged(
                                "a",
                                Value::Array(vec![int(columns), ints(&[]), ints(cells)]),
                            ),
                        })
                        .collect(),
                )
            }
            Some("body") => *field = int(&body),
            _ => {}
        }
    }
    let mut encoded = Vec::new();
    ciborium::into_writer(&value, &mut encoded).unwrap();
    ciborium::from_reader(encoded.as_slice()).unwrap()
}

#[track_caller]
fn refused(frame: WireDocument<'static>, reason: &str) {
    match frame.into_tree(Limits::default()) {
        Err(ReportError::InvalidDocument { reason: got }) => assert_eq!(got, reason),
        other => panic!("the frame must be refused ({reason}), got {other:?}"),
    }
}

/// A row span chosen by a frame cannot reach rows that no cell of the table starts in: the
/// renderers walk every covered row, so `u64::MAX` would loop and `1 << 33` would allocate 64 GiB.
/// Refused when the worker admits the frame, before any layout.
#[test]
fn a_frame_row_span_past_the_rows_of_its_cells_is_refused() {
    for rowspan in [u64::MAX, 1 << 33, 4] {
        refused(
            frame(
                &[
                    N::Text,
                    N::Cell(0, 1, rowspan),
                    N::Table(2, vec![1, 1, 0, 0]),
                ],
                2,
            ),
            "table section has more rows than cells: a row span covers rows that start no cell",
        );
    }
    // Two rows spanned and two more cells: as many rows as cells, admitted.
    frame(
        &[N::Text, N::Cell(0, 1, 2), N::Table(2, vec![1, 1, 0, 0])],
        2,
    )
    .into_tree(Limits::default())
    .unwrap();
}

/// A frame cannot claim a table wider than any page shows; placing its cells would cost columns
/// times cells.
#[test]
fn a_frame_table_wider_than_the_column_cap_is_refused() {
    for columns in [1001, 200_000] {
        refused(
            frame(&[N::Text, N::Table(columns, vec![0; columns as usize])], 1),
            "table has more than 1000 columns",
        );
    }
    frame(&[N::Text, N::Table(1000, vec![0; 1000])], 1)
        .into_tree(Limits::default())
        .unwrap();
}

/// The structural rules of a cell hold for a frame as for a document built in process.
#[test]
fn a_frame_breaking_the_cell_rules_is_refused() {
    let precede = "node must precede its parent";
    let inside = "a Cell node is only allowed directly inside a Table";
    let cases: [(&[N], u64, &str); 8] = [
        // the child index points forward, then at the cell itself
        (
            &[N::Cell(1, 1, 1), N::Text, N::Table(1, vec![0])],
            2,
            precede,
        ),
        (
            &[N::Text, N::Cell(1, 1, 1), N::Table(1, vec![1])],
            2,
            precede,
        ),
        // a cell outside a table, as the body, and around a cell
        (&[N::Text, N::Cell(0, 1, 1), N::Flow(vec![1])], 2, inside),
        (&[N::Text, N::Cell(0, 1, 1)], 1, inside),
        (&[N::Text, N::Cell(0, 1, 1), N::Cell(1, 1, 1)], 2, inside),
        // a zero row span
        (
            &[N::Text, N::Cell(0, 1, 0), N::Table(1, vec![1])],
            2,
            "table cell spans must be at least 1",
        ),
        // a colspan that overflows from a later column
        (
            &[N::Text, N::Cell(0, u64::MAX, 1), N::Table(2, vec![0, 1])],
            2,
            "table cell spans past the last column",
        ),
        // plain and spanning cells whose last row is short
        (
            &[N::Text, N::Cell(0, 2, 1), N::Table(3, vec![1, 0, 1])],
            2,
            "table rows must fill a positive column count no larger than its cells",
        ),
    ];
    for (nodes, body, reason) in cases {
        refused(frame(nodes, body), reason);
    }
}
