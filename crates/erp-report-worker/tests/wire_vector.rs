// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! A barcode or QR code crosses the worker frame (protocol 9) as the numbers it is made from, and
//! the worker makes it again under the checks of a document built in the server process: the frame
//! carries no markup, and no frame gets a graphic past the limits.
use ciborium::Value;
use erp_report::{
    Code, DocTree, DocumentBuilder, DocumentOptions, LimitKind, Limits, ModuleGrid, Node,
    PageSetup, QrLevel, ReportError, Symbology, render_html,
};
use erp_report_worker::WireDocument;

/// A transfer name as Code 128 with its text, and a QR code, in one flow.
fn coded() -> DocTree {
    let limits = Limits::default();
    let mut b = DocumentBuilder::new(limits);
    let mut nodes = Vec::new();
    for (symbology, value, caption) in [
        (Symbology::Code128, "WH/OUT/00012", Some("WH/OUT/00012")),
        (Symbology::Qr, "https://erpforall.example/t/12", None),
    ] {
        let code = Code::encode(symbology, value, QrLevel::Medium, &limits).unwrap();
        let fitted = code.fit(225.0, 60.0, ModuleGrid::default()).unwrap();
        nodes.push(
            b.add(Node::Vector {
                width: Some(fitted.width()),
                height: Some(fitted.height()),
                vector: fitted.into_vector(),
                alt: format!("Barcode {value}"),
                caption: caption.map(str::to_owned),
            })
            .unwrap(),
        );
    }
    let body = b.add(Node::Flow { children: nodes }).unwrap();
    let options = DocumentOptions::new("c", "C", PageSetup::new(595.0, 842.0, 40.0).unwrap());
    b.finish(body, options).unwrap()
}
fn encoded(tree: &DocTree) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::into_writer(&WireDocument::from_tree(tree).unwrap(), &mut bytes).unwrap();
    bytes
}
fn vectors(tree: &DocTree) -> Vec<&Node> {
    tree.nodes()
        .iter()
        .filter(|node| matches!(node, Node::Vector { .. }))
        .collect()
}

/// writes the markup itself)
#[test]
fn a_code_survives_the_worker_frame_as_numbers() {
    let tree = coded();
    let bytes = encoded(&tree);
    // No markup crosses: the frame holds numbers and the two texts only.
    assert!(!bytes.windows(4).any(|w| w == b"<svg" || w == b"path"));
    let frame: WireDocument<'static> = ciborium::from_reader(bytes.as_slice()).unwrap();
    let back = frame.into_tree(Limits::default()).unwrap();
    let (sent, received) = (vectors(&tree), vectors(&back));
    assert_eq!(sent.len(), 2);
    for (a, b) in sent.iter().zip(&received) {
        let (
            Node::Vector {
                vector: va,
                width: wa,
                height: ha,
                alt: aa,
                caption: ca,
            },
            Node::Vector {
                vector: vb,
                width: wb,
                height: hb,
                alt: ab,
                caption: cb,
            },
        ) = (a, b)
        else {
            panic!("both are vectors");
        };
        // The worker's own markup from the same numbers is the server's, byte for byte.
        assert_eq!(va, vb);
        assert_eq!(va.svg(), vb.svg());
        assert!(!va.rectangle_list().is_empty());
        assert_eq!((wa, ha, aa, ca), (wb, hb, ab, cb));
    }
    assert_eq!(
        render_html(&back, &[]).unwrap(),
        render_html(&tree, &[]).unwrap()
    );
    assert_eq!(erp_report_worker::PROTOCOL_VERSION, 9);
}

/// The frame of `coded()` with the fields of its first graphic changed by `edit`. The fields are
/// box width, box height, rectangles, colour, printed width, printed height, alt, caption.
fn tampered(edit: impl FnOnce(&mut Vec<Value>)) -> WireDocument<'static> {
    let mut value = Value::serialized(&WireDocument::from_tree(&coded()).unwrap()).unwrap();
    let Value::Map(fields) = &mut value else {
        panic!("a document frame is a map");
    };
    let Some((_, Value::Array(nodes))) = fields
        .iter_mut()
        .find(|(key, _)| key.as_text() == Some("nodes"))
    else {
        panic!("a document frame lists its nodes");
    };
    let graphic = nodes
        .iter_mut()
        .find_map(|node| match node {
            Value::Map(variant) => match variant.as_mut_slice() {
                [(Value::Text(tag), Value::Array(fields))] if tag == "v" => Some(fields),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("the frame carries the graphic under the tag v"));
    assert_eq!(graphic.len(), 8);
    edit(graphic);
    let mut bytes = Vec::new();
    ciborium::into_writer(&value, &mut bytes).unwrap();
    ciborium::from_reader(bytes.as_slice()).unwrap()
}
fn rectangle(x: f64, y: f64, w: f64, h: f64) -> Value {
    Value::Array([x, y, w, h].into_iter().map(Value::Float).collect())
}

/// A frame the worker did not build itself gets no graphic the server could not have built: every
/// rectangle is finite, positive and inside the box, their number is bounded, and so is the printed
/// size.
#[test]
fn a_frame_with_a_graphic_out_of_bounds_is_refused() {
    type Edit = Box<dyn FnOnce(&mut Vec<Value>)>;
    let cases: Vec<(&str, Edit)> = vec![
        (
            "vector rectangles must have a positive size and lie inside the vector box",
            Box::new(|g| g[2] = Value::Array(vec![rectangle(0.0, 0.0, 1.0e9, 1.0)])),
        ),
        (
            "vector rectangles must have a positive size and lie inside the vector box",
            Box::new(|g| g[2] = Value::Array(vec![rectangle(-1.0, 0.0, 1.0, 1.0)])),
        ),
        (
            "vector rectangles must have a positive size and lie inside the vector box",
            Box::new(|g| g[2] = Value::Array(vec![rectangle(0.0, 0.0, 0.0, 1.0)])),
        ),
        (
            "vector rectangles must be finite",
            Box::new(|g| g[2] = Value::Array(vec![rectangle(0.0, f64::NAN, 1.0, 1.0)])),
        ),
        (
            "vector box must be finite and in (0, 14400] units each way",
            Box::new(|g| g[0] = Value::Float(1.0e7)),
        ),
        (
            "vector box must be finite and in (0, 14400] units each way",
            Box::new(|g| g[1] = Value::Float(f64::INFINITY)),
        ),
        (
            "image and vector sizes must be finite and in (0, 14400] points",
            Box::new(|g| g[4] = Value::Float(0.0)),
        ),
        (
            "image and vector sizes must be finite and in (0, 14400] points",
            Box::new(|g| g[5] = Value::Float(1.0e6)),
        ),
    ];
    for (reason, edit) in cases {
        match tampered(edit).into_tree(Limits::default()) {
            Err(ReportError::InvalidDocument { reason: got }) => assert_eq!(got, reason),
            other => panic!("`{reason}` must be refused, got {other:?}"),
        }
    }
}

#[test]
fn the_rectangles_of_a_graphic_are_bounded_in_a_frame_too() {
    // Past the hard bound: refused when the worker makes the graphic.
    let flood = tampered(|g| {
        g[2] = Value::Array(vec![rectangle(0.0, 0.0, 1.0, 1.0); 16_385]);
    });
    match flood.into_tree(Limits::default()) {
        Err(ReportError::DocumentLimit {
            kind: LimitKind::VectorRectangles,
            actual: 16_385,
            max: 16_384,
        }) => {}
        other => panic!("{other:?}"),
    }
    // Under a limit the host lowered: refused when the document is admitted, as in process.
    let lowered = Limits::default()
        .lowered(LimitKind::VectorRectangles, 10)
        .unwrap();
    let frame: WireDocument<'static> = ciborium::from_reader(encoded(&coded()).as_slice()).unwrap();
    match frame.into_tree(lowered) {
        Err(ReportError::DocumentLimit {
            kind: LimitKind::VectorRectangles,
            max: 10,
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
    let mut b = DocumentBuilder::new(lowered);
    let code = Code::encode(Symbology::Code128, "WH/OUT/00012", QrLevel::Low, &lowered).unwrap();
    let fitted = code.fit(225.0, 60.0, ModuleGrid::default()).unwrap();
    let refused = b.add(Node::Vector {
        width: Some(fitted.width()),
        height: Some(fitted.height()),
        vector: fitted.into_vector(),
        alt: String::new(),
        caption: None,
    });
    assert_eq!(
        refused.unwrap_err().to_string(),
        "report limit vector_rectangles exceeded: 43 > 10"
    );
}
