// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! The style of a block crosses the worker frame field for field.
use erp_report::{
    CrossAlignment, DocumentBuilder, DocumentOptions, ImageFit, LimitKind, Limits, Node, PageSetup,
    PictureQuality, ReportError, Style, TableLook,
};
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

/// Each page margin crosses the worker frame on its own side.
#[test]
fn page_margins_survive_the_worker_frame_side_by_side() {
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(Node::Text("page".into())).unwrap();
    let page = PageSetup::with_margins(595.0, 842.0, [113.0, 34.0, 79.0, 21.0]).unwrap();
    let tree = b
        .finish(body, DocumentOptions::new("m", "M", page))
        .unwrap();
    let back = WireDocument::from_tree(&tree)
        .unwrap()
        .into_tree(Limits::default())
        .unwrap();
    assert_eq!(back.options().page().margins(), [113.0, 34.0, 79.0, 21.0]);
    let html = erp_report::render_html(&back, &[]).unwrap();
    assert!(html.contains("padding:113pt 34pt 79pt 21pt"), "{html}");
}

/// A picture (the letterhead logo) crosses the worker frame byte for byte, as one CBOR byte string.
///
/// picture over the per-document count is refused by name there too)
#[test]
fn a_picture_survives_the_worker_frame() {
    const PNG: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b
        .add(Node::Image {
            data: PNG.to_vec().into(),
            width: None,
            height: Some(34.0),
            fit: ImageFit::Cover,
            alt: "Logo".into(),
        })
        .unwrap();
    let page = PageSetup::new(595.0, 842.0, 40.0).unwrap();
    let pictures = PictureQuality::new(300, 70).unwrap();
    let tree = b
        .finish(
            body,
            DocumentOptions::new("p", "P", page).with_pictures(pictures),
        )
        .unwrap();
    let wire = WireDocument::from_tree(&tree).unwrap();
    let mut encoded = Vec::new();
    ciborium::into_writer(&wire, &mut encoded).unwrap();
    assert!(
        encoded.windows(PNG.len()).any(|w| w == PNG),
        "the bytes are one byte string"
    );
    let back = wire.into_tree(Limits::default()).unwrap();
    let Node::Image {
        data,
        width,
        height,
        fit,
        alt,
    } = &back.nodes()[back.body().index()]
    else {
        panic!("the body is no longer a picture");
    };
    assert_eq!(
        (&data[..], *width, *height, *fit, alt.as_str()),
        (&PNG[..], None, Some(34.0), ImageFit::Cover, "Logo")
    );
    // the resolution and the JPEG quality of the document cross with it (protocol 8)
    assert_eq!(back.options().pictures(), pictures);

    // the worker admits the frame under its own limits: a picture over the count is refused there
    let two = |limits: Limits| {
        let mut b = DocumentBuilder::new(Limits::default());
        let picture = |b: &mut DocumentBuilder| {
            b.add(Node::Image {
                data: PNG.to_vec().into(),
                width: None,
                height: None,
                fit: ImageFit::Contain,
                alt: String::new(),
            })
            .unwrap()
        };
        let children = vec![picture(&mut b), picture(&mut b)];
        let body = b.add(Node::Flow { children }).unwrap();
        let page = PageSetup::new(595.0, 842.0, 40.0).unwrap();
        let tree = b
            .finish(body, DocumentOptions::new("p", "P", page))
            .unwrap();
        WireDocument::from_tree(&tree).unwrap().into_tree(limits)
    };
    two(Limits::default()).unwrap();
    let e = two(Limits::default().lowered(LimitKind::Images, 1).unwrap()).unwrap_err();
    assert!(
        matches!(
            e,
            ReportError::DocumentLimit {
                kind: LimitKind::Images,
                actual: 2,
                max: 1
            }
        ),
        "{e:?}"
    );
    assert_eq!(e.to_string(), "report limit images exceeded: 2 > 1");
}

/// frame with its size, its alternative text and its reason)
#[test]
fn a_placeholder_survives_the_worker_frame() {
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b
        .add(Node::Placeholder {
            width: 36.0,
            height: 24.0,
            alt: "Seal".into(),
            reason: "the picture at t:1 cannot be read".into(),
        })
        .unwrap();
    let page = PageSetup::new(595.0, 842.0, 40.0).unwrap();
    let tree = b
        .finish(body, DocumentOptions::new("p", "P", page))
        .unwrap();
    let back = WireDocument::from_tree(&tree)
        .unwrap()
        .into_tree(Limits::default())
        .unwrap();
    let Node::Placeholder {
        width,
        height,
        alt,
        reason,
    } = &back.nodes()[back.body().index()]
    else {
        panic!("the body is no longer a placeholder");
    };
    assert_eq!(
        (*width, *height, alt.as_str(), reason.as_str()),
        (36.0, 24.0, "Seal", "the picture at t:1 cannot be read")
    );
    assert_eq!(back.options().pictures(), PictureQuality::default());
}

/// The row alignment of a grid crosses the worker frame (protocol 5): a letterhead whose name is
/// centred on the logo prints the same on a worker host as in the server process.
#[test]
fn the_row_alignment_of_a_grid_survives_the_worker_frame() {
    for align in [
        CrossAlignment::Start,
        CrossAlignment::Center,
        CrossAlignment::End,
    ] {
        let mut b = DocumentBuilder::new(Limits::default());
        let left = b.add(Node::Text("logo".into())).unwrap();
        let right = b.add(Node::Text("name".into())).unwrap();
        let grid = b
            .add(Node::Grid {
                columns: vec![4.0, 8.0],
                gutter: 15.0,
                cells: vec![left, right],
                align,
            })
            .unwrap();
        let options = DocumentOptions::new("g", "G", PageSetup::new(595.0, 842.0, 40.0).unwrap());
        let tree = b.finish(grid, options).unwrap();
        let back = WireDocument::from_tree(&tree)
            .unwrap()
            .into_tree(Limits::default())
            .unwrap();
        let Some(Node::Grid {
            columns,
            gutter,
            align: got,
            ..
        }) = back.nodes().get(back.body().index())
        else {
            panic!("the body is the grid");
        };
        assert_eq!((columns.as_slice(), *gutter), (&[4.0, 8.0][..], 15.0));
        assert_eq!(*got, align);
    }
}
