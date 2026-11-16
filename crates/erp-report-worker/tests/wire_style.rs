// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! The style of a block crosses the worker frame field for field.
use erp_report::{DocumentBuilder, DocumentOptions, Limits, Node, PageSetup, Style, TableLook};
use erp_report_worker::WireDocument;

#[test]
fn block_decoration_survives_the_worker_frame() {
    let mut b = DocumentBuilder::new(Limits::default());
    let text = b.add(Node::Text("panel".into())).unwrap();
    let style = Style::default()
        .with_spacing(4.0, 9.0)
        .unwrap()
        .with_fill(Some([1, 2, 3]))
        .with_rules(true, false)
        .with_table_look(TableLook::Borderless);
    let block = b
        .add(Node::Block {
            children: vec![text],
            style: style.clone(),
        })
        .unwrap();
    let options = DocumentOptions::new("w", "W", PageSetup::new(595.0, 842.0, 40.0).unwrap());
    let tree = b.finish(block, options).unwrap();
    let back = WireDocument::from_tree(&tree)
        .unwrap()
        .into_tree(Limits::default())
        .unwrap();
    let Some(Node::Block { style: got, .. }) = back.nodes().get(back.body().index()) else {
        panic!("the body is the block");
    };
    assert_eq!(got.spacing(), (4.0, 9.0));
    assert_eq!(got.fill(), Some([1, 2, 3]));
    assert_eq!(got.rules(), (true, false));
    assert_eq!(got.table_look(), TableLook::Borderless);
}
