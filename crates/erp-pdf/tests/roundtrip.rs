// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use erp_pdf::{PageSelection, PdfLimits, Rotation, merge, rotate};
use lopdf::{Document, Object, Stream, dictionary};
use support::*;
fn merged(inputs: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    merge(inputs, &PdfLimits::default(), &mut |b| {
        out.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    out
}
#[test]
fn thousand_inputs_keep_every_page_in_order() {
    let out = merged((0..1000).map(|n| fixture(&[&format!("PAGE{n:04}")])));
    let d = reopen(&out, 1000);
    for n in 0..1000 {
        assert_eq!(
            d.extract_text(&[n + 1]).unwrap().trim(),
            format!("PAGE{n:04}")
        );
    }
}
#[test]
fn serializer_preserves_object_values_and_shared_resources() {
    let mut d = Document::load_mem(&fixture(&["A", "B"])).unwrap();
    let value=d.add_object(dictionary!{"A /#"=>Object::Array(vec![Object::Null,true.into(),(-9).into(),Object::Real(0.25),Object::string_literal(vec![0,255,40,41]),Object::Name(b" /#\xFF".to_vec())])});
    for id in d.get_pages().values() {
        d.get_dictionary_mut(*id).unwrap().set("Probe", value);
    }
    d.compress();
    let out = merged([bytes(d)]);
    let loaded = reopen(&out, 2);
    let p = loaded.get_pages();
    let a = loaded.get_dictionary(p[&1]).unwrap();
    let b = loaded.get_dictionary(p[&2]).unwrap();
    assert_eq!(a.get(b"Resources").unwrap(), b.get(b"Resources").unwrap());
    assert_eq!(a.get(b"Probe").unwrap(), b.get(b"Probe").unwrap());
    let probe = loaded
        .get_dictionary(a.get(b"Probe").unwrap().as_reference().unwrap())
        .unwrap();
    assert_eq!(
        probe.get(b"A /#").unwrap().as_array().unwrap()[4]
            .as_str()
            .unwrap(),
        &[0, 255, 40, 41]
    );
    assert_eq!(
        loaded
            .extract_text(&[1, 2])
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
}
#[test]
fn plain_merge_drops_archival_claims_and_associated_payload() {
    let mut d = Document::load_mem(&fixture(&["A"])).unwrap();
    let embedded = d.add_object(Stream::new(
        dictionary! {"Type"=>"EmbeddedFile"},
        b"SECRET_ATTACHMENT_SENTINEL".to_vec(),
    ));
    let fs=d.add_object(dictionary!{"Type"=>"Filespec","F"=>Object::string_literal("a.xml"),"EF"=>dictionary!{"F"=>embedded}});
    let metadata = d.add_object(Stream::new(
        dictionary! {"Type"=>"Metadata"},
        b"ARCHIVAL_XMP_SENTINEL".to_vec(),
    ));
    let root = d.trailer.get(b"Root").unwrap().as_reference().unwrap();
    let cat = d.get_dictionary_mut(root).unwrap();
    cat.set("AF", vec![Object::Reference(fs)]);
    cat.set("Metadata", metadata);
    cat.set("OutputIntents", vec![Object::Dictionary(dictionary! {})]);
    let page = d.get_pages()[&1];
    d.get_dictionary_mut(page)
        .unwrap()
        .set("AF", vec![Object::Reference(fs)]);
    let out = merged([bytes(d)]);
    let loaded = reopen(&out, 1);
    assert!(!loaded.catalog().unwrap().has(b"Metadata"));
    assert!(!loaded.catalog().unwrap().has(b"AF"));
    assert!(
        !out.windows(b"SECRET_ATTACHMENT_SENTINEL".len())
            .any(|s| s == b"SECRET_ATTACHMENT_SENTINEL")
    );
    assert!(
        !out.windows(b"ARCHIVAL_XMP_SENTINEL".len())
            .any(|s| s == b"ARCHIVAL_XMP_SENTINEL")
    );
    assert_eq!(loaded.extract_text(&[1]).unwrap().trim(), "A");
}
#[test]
fn rotation_composes_without_mutating_sibling_page() {
    let mut d = Document::load_mem(&fixture(&["A", "B"])).unwrap();
    let root = d
        .catalog()
        .unwrap()
        .get(b"Pages")
        .unwrap()
        .as_reference()
        .unwrap();
    d.get_dictionary_mut(root).unwrap().set("Rotate", 270);
    let l = PdfLimits::default();
    let mut out = Vec::new();
    rotate(
        &bytes(d),
        &PageSelection::new(vec![1], &l).unwrap(),
        Rotation::new(180).unwrap(),
        &l,
        &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        },
    )
    .unwrap();
    let d = reopen(&out, 2);
    let pages = d.get_pages();
    assert_eq!(
        d.get_dictionary(pages[&1])
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap(),
        90
    );
    assert!(!d.get_dictionary(pages[&2]).unwrap().has(b"Rotate"));
    assert_eq!(
        d.get_dictionary(root)
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap(),
        270
    );
}
#[test]
fn repeated_operations_are_byte_deterministic() {
    let a = fixture(&["A"]);
    assert_eq!(merged([a.clone()]), merged([a]));
}
#[test]
fn large_direct_inherited_resources_are_promoted_once() {
    let labels = vec!["A"; 200];
    let mut d = Document::load_mem(&fixture(&labels)).unwrap();
    let root = d
        .catalog()
        .unwrap()
        .get(b"Pages")
        .unwrap()
        .as_reference()
        .unwrap();
    let resources = d
        .get_dictionary(root)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_reference()
        .unwrap();
    let mut direct = d.get_dictionary(resources).unwrap().clone();
    direct.set("Probe", Object::string_literal(vec![b'x'; 65536]));
    d.get_dictionary_mut(root).unwrap().set("Resources", direct);
    let out = merged([bytes(d)]);
    let loaded = reopen(&out, 200);
    let mut resource_ids = std::collections::BTreeSet::new();
    for id in loaded.get_pages().values() {
        resource_ids.insert(
            loaded
                .get_dictionary(*id)
                .unwrap()
                .get(b"Resources")
                .unwrap()
                .as_reference()
                .unwrap(),
        );
    }
    assert_eq!(resource_ids.len(), 1);
}
#[test]
fn modern_object_stream_input_reopens_after_merge() {
    let mut d = Document::load_mem(&fixture(&["COMPRESSED"])).unwrap();
    let mut input = Vec::new();
    d.save_modern(&mut input).unwrap();
    let out = merged([input]);
    let d = reopen(&out, 1);
    assert_eq!(d.extract_text(&[1]).unwrap().trim(), "COMPRESSED");
    assert!(!raster(&out).is_empty());
}
#[test]
fn incremental_and_nested_pages_keep_latest_geometry() {
    let input = fixture(&["A", "B"]);
    let mut d = Document::load_mem(&input).unwrap();
    let root = d
        .catalog()
        .unwrap()
        .get(b"Pages")
        .unwrap()
        .as_reference()
        .unwrap();
    let children = d
        .get_dictionary(root)
        .unwrap()
        .get(b"Kids")
        .unwrap()
        .clone();
    let nested =
        d.add_object(dictionary! {"Type"=>"Pages","Parent"=>root,"Count"=>2,"Kids"=>children});
    for id in d.get_pages().values() {
        d.get_dictionary_mut(*id).unwrap().set("Parent", nested);
    }
    d.get_dictionary_mut(root)
        .unwrap()
        .set("Kids", vec![Object::Reference(nested)]);
    let original = bytes(d);
    let d = Document::load_mem(&original).unwrap();
    let page = d.get_pages()[&1];
    let mut incremental = lopdf::IncrementalDocument::create_from(original, d);
    incremental.opt_clone_object_to_new_document(page).unwrap();
    incremental
        .new_document
        .get_dictionary_mut(page)
        .unwrap()
        .set("Rotate", 90);
    let mut input = Vec::new();
    incremental.save_to(&mut input).unwrap();
    let out = merged([input]);
    let d = reopen(&out, 2);
    let p = d.get_pages();
    assert_eq!(
        d.get_dictionary(p[&1])
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap(),
        90
    );
    assert_eq!(
        d.extract_text(&[1, 2])
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
}
