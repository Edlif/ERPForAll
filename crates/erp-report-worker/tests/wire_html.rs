// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
use ciborium::value::Value;
use erp_report::{
    DocTree, DocumentBuilder, DocumentOptions, Limits, Node, PageSetup, ReportError, RichText,
    Style,
};
use erp_report_worker::WireDocument;

fn document() -> DocTree {
    let rich = RichText::default()
        .with_strike(true)
        .with_preformatted(true)
        .with_rtl(Some(true))
        .with_baseline(-1)
        .unwrap()
        .with_tracking(1.5)
        .unwrap()
        .with_font("Noto Sans")
        .unwrap()
        .with_link("https://example.test/a?x=1&y=2")
        .unwrap();
    let mut b = DocumentBuilder::new(Limits::default());
    let text = b.add(Node::Text("safe text".into())).unwrap();
    let root = b
        .add(Node::Inline {
            children: vec![text],
            style: Style::default().with_rich(rich),
        })
        .unwrap();
    b.finish(
        root,
        DocumentOptions::new("html", "HTML", PageSetup::new(400.0, 500.0, 35.0).unwrap()),
    )
    .unwrap()
}

#[test]
fn rich_text_survives_encoded_worker_frame() {
    let doc = document();
    let mut bytes = Vec::new();
    ciborium::into_writer(&WireDocument::from_tree(&doc).unwrap(), &mut bytes).unwrap();
    let wire: WireDocument<'_> = ciborium::from_reader(bytes.as_slice()).unwrap();
    let back = wire.into_tree(Limits::default()).unwrap();
    let Node::Inline { style, .. } = &back.nodes()[back.body().index()] else {
        panic!("inline");
    };
    let Node::Inline {
        style: original, ..
    } = &doc.nodes()[doc.body().index()]
    else {
        panic!("inline");
    };
    assert_eq!(style.rich(), original.rich());
}

fn mutate_style(value: &mut Value, index: usize, replacement: &Value) -> bool {
    match value {
        Value::Map(entries) => {
            for (key, value) in entries {
                if key == &Value::Text("i".into()) {
                    let Value::Array(parts) = value else {
                        panic!("inline tuple");
                    };
                    let Value::Array(style) = &mut parts[1] else {
                        panic!("style tuple");
                    };
                    style[index] = replacement.clone();
                    return true;
                }
                if mutate_style(value, index, replacement) {
                    return true;
                }
            }
            false
        }
        Value::Array(values) => values
            .iter_mut()
            .any(|v| mutate_style(v, index, replacement)),
        _ => false,
    }
}

#[test]
fn worker_revalidates_untrusted_rich_style_fields() {
    let doc = document();
    for (index, replacement, reason) in [
        (
            13,
            Value::Integer(2.into()),
            "unsupported text baseline offset; use subscript (-1), normal (0), or superscript (1)",
        ),
        (
            16,
            Value::Float(f64::NAN),
            "letter spacing must be finite and in [-100, 100] points",
        ),
        (
            17,
            Value::Text("bad;font".into()),
            "inline font must be at most 256 bytes of letters, digits, spaces, '-' or '_'",
        ),
        (
            18,
            Value::Text("javascript:alert(1)".into()),
            "inline link must be an http, https or mailto URL of at most 4096 bytes without controls",
        ),
    ] {
        let mut bytes = Vec::new();
        ciborium::into_writer(&WireDocument::from_tree(&doc).unwrap(), &mut bytes).unwrap();
        let mut value: Value = ciborium::from_reader(bytes.as_slice()).unwrap();
        assert!(mutate_style(&mut value, index, &replacement));
        bytes.clear();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        let wire: WireDocument<'_> = ciborium::from_reader(bytes.as_slice()).unwrap();
        let err = wire.into_tree(Limits::default()).unwrap_err();
        assert!(matches!(err, ReportError::InvalidDocument { reason: actual } if actual == reason));
        assert_eq!(
            err.to_string(),
            format!("invalid report document: {reason}")
        );
    }
}
