// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! Image admission, trusted vectors and table spans of the native report document.
use erp_report::{
    DocumentBuilder, DocumentOptions, ImageFormat, LimitKind, Limits, Node, NodeId, PageSetup,
    ReportError, Style, Vector, probe_image,
};
use std::sync::Arc;

/// A PNG signature and IHDR chunk declaring `width` × `height`; no image data.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut d = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    d.extend(width.to_be_bytes());
    d.extend(height.to_be_bytes());
    d.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
    d
}
fn vp8x(width: u32, height: u32) -> Vec<u8> {
    let mut d = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
    d.extend(&(width - 1).to_le_bytes()[..3]);
    d.extend(&(height - 1).to_le_bytes()[..3]);
    d
}
fn image_node(data: Vec<u8>) -> Node {
    Node::Image {
        data: Arc::from(data),
        width: None,
        height: None,
        alt: "logo".into(),
    }
}
fn limited(kind: LimitKind, value: usize) -> Limits {
    Limits::default().lowered(kind, value).unwrap()
}

/// before any decoder; the decode of a corrupt body is in the renderer tests)
#[test]
fn image_bounds_precede_parsing() {
    let png = png_header(10, 10);
    let limits = limited(LimitKind::ImageBytes, png.len());
    let info = probe_image(&png, &limits).unwrap();
    assert_eq!(
        (info.format(), info.width(), info.height()),
        (ImageFormat::Png, 10, 10)
    );
    let mut longer = png.clone();
    longer.push(0);
    let e = probe_image(&longer, &limits).unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::ImageBytes,
            ..
        }
    ));
    assert_eq!(
        e.to_string(),
        format!(
            "report limit image_bytes exceeded: {} > {}",
            png.len() + 1,
            png.len()
        )
    );

    let pixels = limited(LimitKind::ImagePixels, 100);
    probe_image(&png_header(10, 10), &pixels).unwrap();
    let e = probe_image(&png_header(10, 11), &pixels).unwrap_err();
    assert_eq!(
        e.to_string(),
        "report limit image_pixels exceeded: 110 > 100"
    );

    // 2^24 × 2^24 overflows the 32-bit pixel count and is refused as a limit, not wrapped.
    let e = probe_image(&vp8x(1 << 24, 1 << 24), &Limits::default()).unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::ImagePixels,
            actual: usize::MAX,
            ..
        }
    ));
    let info = probe_image(&vp8x(300, 200), &Limits::default()).unwrap();
    assert_eq!(
        (info.format(), info.width(), info.height()),
        (ImageFormat::WebP, 300, 200)
    );

    for (bytes, message) in [
        (
            png_header(10, 10)[..20].to_vec(),
            "report image: image header is malformed or truncated",
        ),
        (
            png_header(0, 10),
            "report image: image header declares an empty image",
        ),
        (
            b"%PDF-1.7".to_vec(),
            "report image: image bytes are not PNG, JPEG, GIF, WebP or SVG",
        ),
        (
            vec![0xFF, 0xD8, 0xFF, 0xDA],
            "report image: image header is malformed or truncated",
        ),
    ] {
        let e = probe_image(&bytes, &Limits::default()).unwrap_err();
        assert!(matches!(e, ReportError::Image { .. }));
        assert_eq!(e.to_string(), message);
    }
}

#[test]
fn formats_are_detected_from_content() {
    // SOI, an APP0 segment of 16 bytes, then SOF0 with height 300 and width 400.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    jpeg.extend([0; 14]);
    jpeg.extend([0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0x2C, 0x01, 0x90, 0x03]);
    let info = probe_image(&jpeg, &Limits::default()).unwrap();
    assert_eq!(
        (info.format(), info.width(), info.height()),
        (ImageFormat::Jpeg, 400, 300)
    );
    let gif = b"GIF89a\x20\x00\x10\x00".to_vec();
    let info = probe_image(&gif, &Limits::default()).unwrap();
    assert_eq!(
        (info.format(), info.width(), info.height()),
        (ImageFormat::Gif, 32, 16)
    );
    let svg = b"\n <?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>";
    assert_eq!(
        probe_image(svg, &Limits::default()).unwrap().format(),
        ImageFormat::UntrustedSvg
    );
}

/// document byte budget once per occurrence)
#[test]
fn image_nodes_are_admitted_under_the_builder_limits() {
    let mut b = DocumentBuilder::new(limited(LimitKind::ImagePixels, 50));
    let e = b.add(image_node(png_header(10, 10))).unwrap_err();
    assert_eq!(
        e.to_string(),
        "report limit image_pixels exceeded: 100 > 50"
    );
    let e = b
        .add(Node::Image {
            data: Arc::from(png_header(5, 5)),
            width: Some(f64::NAN),
            height: None,
            alt: String::new(),
        })
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "invalid report document: image and vector sizes must be finite and in (0, 14400] points"
    );
    b.add(image_node(png_header(5, 5))).unwrap();

    let data = vec![0x20u8; 5000];
    let mut svg = b"<svg>".to_vec();
    svg.extend(&data);
    let mut b = DocumentBuilder::new(limited(LimitKind::Bytes, 4096));
    let e = b.add(image_node(svg)).unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::Bytes,
            ..
        }
    ));
}

/// writes from numbers)
#[test]
fn vectors_are_generated_from_numbers() {
    let v = Vector::rectangles(
        10.0,
        4.0,
        &[[0.0, 0.0, 1.0, 4.0], [2.5, 0.0, 0.5, 4.0]],
        [1, 2, 3],
    )
    .unwrap();
    assert_eq!(
        std::str::from_utf8(v.svg()).unwrap(),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 4\" width=\"10\" height=\"4\"><path fill=\"#010203\" d=\"M0 0h1v4h-1zM2.5 0h0.5v4h-0.5z\"/></svg>"
    );
    for (w, rects) in [(0.0, vec![]), (10.0, vec![[f64::INFINITY, 0.0, 1.0, 1.0]])] {
        let e = Vector::rectangles(w, 4.0, &rects, [0; 3]).unwrap_err();
        assert!(matches!(e, ReportError::InvalidDocument { .. }));
    }
}

/// (colspan, rowspan) of each cell.
type Spans = &'static [(usize, usize)];
fn cell(b: &mut DocumentBuilder, label: &str, colspan: usize, rowspan: usize) -> NodeId {
    let child = b.add(Node::Text(label.into())).unwrap();
    b.add(Node::Cell {
        child,
        colspan,
        rowspan,
    })
    .unwrap()
}
fn table(
    b: &mut DocumentBuilder,
    columns: usize,
    cells: Vec<NodeId>,
) -> Result<NodeId, ReportError> {
    b.add(Node::Table {
        columns,
        header: vec![],
        cells,
    })
}

/// the grid are refused before layout; valid spans are admitted)
#[test]
fn table_spans_are_validated() {
    let mut b = DocumentBuilder::new(Limits::default());
    // Header: one cell over three columns. Body: [A B C] [A D E] with A over two rows.
    let wide = cell(&mut b, "wide", 3, 1);
    let a = cell(&mut b, "A", 1, 2);
    let mut body = vec![a];
    for label in ["B", "C", "D", "E"] {
        body.push(b.add(Node::Text(label.into())).unwrap());
    }
    b.add(Node::Table {
        columns: 3,
        header: vec![wide],
        cells: body,
    })
    .unwrap();

    let child = b.add(Node::Text("x".into())).unwrap();
    let e = b
        .add(Node::Cell {
            child,
            colspan: 0,
            rowspan: 1,
        })
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "invalid report document: table cell spans must be at least 1"
    );

    let cases: [(&str, usize, Spans); 4] = [
        (
            "invalid report document: table cell spans past the last column",
            2,
            &[(1, 1), (1, 1), (3, 1)],
        ),
        (
            "invalid report document: table cell overlaps a row span or leaves a gap in its row",
            3,
            &[(1, 1), (1, 2), (1, 1), (2, 1)],
        ),
        (
            "invalid report document: table rows must fill a positive column count no larger than its cells",
            2,
            &[(1, 1), (1, 1), (1, 1)],
        ),
        (
            "invalid report document: table row span reaches past the last row",
            2,
            &[(1, 2), (1, 1)],
        ),
    ];
    for (message, columns, spans) in cases {
        let cells = spans
            .iter()
            .map(|(c, r)| cell(&mut b, "x", *c, *r))
            .collect();
        let e = table(&mut b, columns, cells).unwrap_err();
        assert!(matches!(e, ReportError::InvalidDocument { .. }));
        assert_eq!(e.to_string(), message);
    }
    // A row covered entirely by row spans from above needs no cell of its own.
    let cells = [(1, 3), (1, 3), (1, 1), (1, 1)]
        .iter()
        .map(|(c, r)| cell(&mut b, "x", *c, *r))
        .collect::<Vec<_>>();
    table(&mut b, 2, cells).unwrap();
    let cells = [(1, 2), (1, 2), (1, 1), (1, 1)]
        .iter()
        .map(|(c, r)| cell(&mut b, "x", *c, *r))
        .collect::<Vec<_>>();
    table(&mut b, 2, cells).unwrap();
}

#[test]
fn cells_outside_tables_are_refused() {
    let mut b = DocumentBuilder::new(Limits::default());
    let c = cell(&mut b, "x", 1, 1);
    let message = "invalid report document: a Cell node is only allowed directly inside a Table";
    let e = b
        .add(Node::Block {
            children: vec![c.clone()],
            style: Style::default(),
        })
        .unwrap_err();
    assert_eq!(e.to_string(), message);
    let e = b
        .add(Node::Cell {
            child: c.clone(),
            colspan: 1,
            rowspan: 1,
        })
        .unwrap_err();
    assert_eq!(e.to_string(), message);
    assert_eq!(b.set_header(c.clone()).unwrap_err().to_string(), message);
    let options = DocumentOptions::new(
        "cells",
        "Cells",
        PageSetup::new(400.0, 500.0, 35.0).unwrap(),
    );
    assert_eq!(b.finish(c, options).unwrap_err().to_string(), message);
}
