// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use erp_pdf::{
    Duplicate, EmbeddedFileOptions, Limit, PdfErrorKind, PdfLimits, Relationship, attach,
    embedded_files,
};
use lopdf::{Document, Object, Stream, dictionary};
use support::*;

/// A document whose existing attachment sits in a nested name tree and the catalog AF array.
fn nested(data: &[u8], compress: bool) -> Vec<u8> {
    let mut d = Document::load_mem(&fixture(&["BODY"])).unwrap();
    let mut s = Stream::new(
        dictionary! {"Type"=>"EmbeddedFile","Subtype"=>"text/plain"},
        data.to_vec(),
    );
    if compress {
        s.compress().unwrap();
    }
    let ef = d.add_object(s);
    let spec = d.add_object(dictionary! {"Type"=>"Filespec","F"=>Object::string_literal("keep.txt"),
        "UF"=>Object::string_literal("keep.txt"),"EF"=>dictionary!{"F"=>ef},"AFRelationship"=>"Source"});
    let leaf = d.add_object(
        dictionary! {"Names"=>vec![Object::string_literal("keep.txt"), spec.into()],
        "Limits"=>vec![Object::string_literal("keep.txt"), Object::string_literal("keep.txt")]},
    );
    let root = d.add_object(dictionary! {"Kids"=>vec![leaf.into()]});
    let catalog = d.trailer.get(b"Root").unwrap().as_reference().unwrap();
    let c = d.get_dictionary_mut(catalog).unwrap();
    c.set("Names", dictionary! {"EmbeddedFiles"=>root});
    c.set("AF", vec![spec.into()]);
    bytes(d)
}
fn options(name: &str, l: &PdfLimits) -> EmbeddedFileOptions {
    EmbeddedFileOptions::new(
        name,
        "application/octet-stream",
        Relationship::Supplement,
        date(),
        l,
    )
    .unwrap()
    .description("Prüfbericht – 検査", l)
    .unwrap()
    .created(erp_pdf::PdfDate::new(2025, 1, 2, 3, 4, 5, 0).unwrap())
}

#[test]
fn embedded_file_round_trips_and_keeps_unrelated_names() {
    let l = PdfLimits::default();
    let input = nested(b"keep me", false);
    let payload: Vec<u8> = (0..=255u8).chain([0, 0, 13, 10]).collect();
    let out = collect(|e| {
        attach(
            &input,
            &payload,
            &options("Rechnung-ü.bin", &l),
            Duplicate::Refuse,
            &l,
            e,
        )
    })
    .unwrap();
    assert!(
        out.starts_with(&input),
        "incremental update keeps the original bytes"
    );
    reopen(&out, 1);
    let files = embedded_files(&out, &l).unwrap();
    assert_eq!(files.len(), 2);
    let keep = files.iter().find(|f| f.name == "keep.txt").unwrap();
    assert_eq!(keep.data, b"keep me");
    assert_eq!(keep.relationship, Some(Relationship::Source));
    assert!(keep.associated);
    let new = files.iter().find(|f| f.name == "Rechnung-ü.bin").unwrap();
    assert_eq!(new.data, payload);
    assert_eq!(new.mime.as_deref(), Some("application/octet-stream"));
    assert_eq!(new.description.as_deref(), Some("Prüfbericht – 検査"));
    assert_eq!(new.relationship, Some(Relationship::Supplement));
    assert_eq!(new.modified.as_deref(), Some("2026-11-15T09:30:00+07:00"));
    assert_eq!(new.created.as_deref(), Some("2025-01-02T03:04:05Z"));
    assert!(new.associated);
    // Filespec, EF and AF agree in the emitted objects.
    let d = Document::load_mem(&out).unwrap();
    let catalog = d.catalog().unwrap();
    let af = catalog.get(b"AF").unwrap().as_array().unwrap();
    assert_eq!(af.len(), 2);
    for spec in af {
        let spec = d.dereference(spec).unwrap().1.as_dict().unwrap();
        assert!(spec.get(b"EF").unwrap().as_dict().unwrap().has(b"F"));
        assert!(spec.has(b"AFRelationship"));
    }
}

#[test]
fn duplicate_names_are_refused_unless_replacement_is_explicit() {
    let l = PdfLimits::default();
    let input = nested(b"keep me", false);
    let first = collect(|e| {
        attach(
            &input,
            b"one",
            &options("report.bin", &l),
            Duplicate::Refuse,
            &l,
            e,
        )
    })
    .unwrap();
    let mut emitted = 0;
    let e = attach(
        &first,
        b"two",
        &options("report.bin", &l),
        Duplicate::Refuse,
        &l,
        &mut |b| {
            emitted += b.len();
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::InvalidOptions);
    assert_eq!(
        e.to_string(),
        "PDF embedded file \"report.bin\" already exists"
    );
    assert_eq!(emitted, 0);
    let second = collect(|e| {
        attach(
            &first,
            b"two",
            &options("report.bin", &l),
            Duplicate::Replace,
            &l,
            e,
        )
    })
    .unwrap();
    assert!(second.starts_with(&first));
    let files = embedded_files(&second, &l).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(
        files.iter().find(|f| f.name == "report.bin").unwrap().data,
        b"two"
    );
    assert_eq!(
        files.iter().find(|f| f.name == "keep.txt").unwrap().data,
        b"keep me"
    );
    let d = Document::load_mem(&second).unwrap();
    assert_eq!(
        d.catalog()
            .unwrap()
            .get(b"AF")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn embedded_decoding_and_traversal_are_bounded() {
    let payload = vec![b'x'; 4096];
    let input = nested(&payload, true);
    let at = PdfLimits::default()
        .with(Limit::DecodedBytes, 4096)
        .unwrap();
    assert_eq!(embedded_files(&input, &at).unwrap()[0].data, payload);
    let below = PdfLimits::default()
        .with(Limit::DecodedBytes, 4095)
        .unwrap();
    let e = embedded_files(&input, &below).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Limit);
    assert_eq!(e.to_string(), "PDF decoded_bytes limit exceeded");

    let mut d = Document::load_mem(&nested(b"k", false)).unwrap();
    let catalog = d.trailer.get(b"Root").unwrap().as_reference().unwrap();
    let names = d
        .get_dictionary(catalog)
        .unwrap()
        .get(b"Names")
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    let root = names.get(b"EmbeddedFiles").unwrap().as_reference().unwrap();
    d.get_dictionary_mut(root)
        .unwrap()
        .set("Kids", vec![root.into()]);
    let e = embedded_files(&bytes(d), &PdfLimits::default()).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Malformed);
    assert_eq!(e.to_string(), "PDF cyclic EmbeddedFiles name tree");
}

#[test]
fn path_names_and_pdfa2_targets_are_refused() {
    let l = PdfLimits::default();
    for name in ["../x", "a/b", "", ".."] {
        let e = EmbeddedFileOptions::new(name, "text/plain", Relationship::Data, date(), &l)
            .unwrap_err();
        assert_eq!(e.kind(), PdfErrorKind::InvalidOptions);
    }
    assert!(EmbeddedFileOptions::new("a", "text", Relationship::Data, date(), &l).is_err());
    let input = with_xmp(&["A"], &packet(2, ""), dictionary! {});
    let e = attach(
        &input,
        b"x",
        &options("x.bin", &l),
        Duplicate::Refuse,
        &l,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Unsupported);
    assert_eq!(
        e.to_string(),
        "PDF feature embedding files into PDF/A-2 (PDF/A-3 only) is refused by design"
    );
}
