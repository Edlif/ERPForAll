// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use erp_pdf::{
    Limit, MetadataField as F, MetadataUpdate, PdfDate, PdfErrorKind, PdfLimits, metadata,
    update_metadata,
};
use lopdf::{Object, dictionary};
use support::*;

fn update(l: &PdfLimits) -> MetadataUpdate {
    MetadataUpdate::new()
        .text(F::Title, "Faktur Penjualan – 請求書 & <Co>", l)
        .unwrap()
        .text(F::Author, "Ayu Lestari", l)
        .unwrap()
        .text(F::Producer, "ERPForAll", l)
        .unwrap()
        .date(F::ModDate, date())
        .unwrap()
        .date(
            F::CreationDate,
            PdfDate::new(2026, 1, 31, 23, 0, 0, -60).unwrap(),
        )
        .unwrap()
}

#[test]
fn info_and_xmp_are_synchronized_deterministic_and_keep_unrelated_properties() {
    let l = PdfLimits::default();
    let input = with_xmp(
        &["A"],
        &packet(
            3,
            "<custom:Note>keep &amp; stay</custom:Note><pdf:Keywords>old words</pdf:Keywords>",
        ),
        dictionary! {"Producer"=>Object::string_literal("Fixture Producer"),"Keywords"=>Object::string_literal("old words")},
    );
    let u = update(&l);
    let out = collect(|e| update_metadata(&input, &u, &l, e)).unwrap();
    let again = collect(|e| update_metadata(&input, &u, &l, e)).unwrap();
    assert_eq!(out, again, "identical inputs give identical bytes");
    assert!(out.starts_with(&input));
    reopen(&out, 1);
    let m = metadata(&out, &l).unwrap();
    for (f, want) in [
        (F::Title, "Faktur Penjualan – 請求書 & <Co>"),
        (F::Author, "Ayu Lestari"),
        (F::Producer, "ERPForAll"),
        (F::ModDate, "2026-11-15T09:30:00+07:00"),
        (F::CreationDate, "2026-01-31T23:00:00-01:00"),
        (F::Keywords, "old words"),
    ] {
        assert_eq!(m.info.get(&f).map(String::as_str), Some(want), "Info {f:?}");
        assert_eq!(m.xmp.get(&f).map(String::as_str), Some(want), "XMP {f:?}");
    }
    let packet = m.packet.unwrap();
    assert!(packet.contains("<custom:Note>keep &amp; stay</custom:Note>"));
    assert_eq!(
        packet.matches("pdf:Producer>").count(),
        2,
        "one open and one close tag"
    );
    assert_eq!(m.pdfa.unwrap().part, 3);
}

#[test]
fn metadata_is_created_when_absent() {
    let l = PdfLimits::default();
    let input = fixture(&["A"]);
    let u = MetadataUpdate::new()
        .text(F::Subject, "Ringkasan", &l)
        .unwrap();
    let out = collect(|e| update_metadata(&input, &u, &l, e)).unwrap();
    let m = metadata(&out, &l).unwrap();
    assert_eq!(m.info[&F::Subject], "Ringkasan");
    assert_eq!(m.xmp[&F::Subject], "Ringkasan");
    assert!(m.pdfa.is_none());
    let e = update_metadata(&input, &MetadataUpdate::new(), &l, &mut |_| Ok(())).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::InvalidOptions);
    assert!(MetadataUpdate::new().date(F::Title, date()).is_err());
    assert!(MetadataUpdate::new().text(F::ModDate, "x", &l).is_err());
}

#[test]
fn unsafe_or_unpreservable_xmp_is_refused_without_output() {
    let l = PdfLimits::default();
    let u = update(&l);
    let doctype =
        "<?xml version=\"1.0\"?><!DOCTYPE x [<!ENTITY e SYSTEM \"file:///etc/passwd\">]><x>&e;</x>";
    let nested = packet(
        3,
        "<custom:Wrap rdf:parseType=\"Resource\"><pdf:Producer>inner</pdf:Producer></custom:Wrap>",
    );
    for (xmp, kind, message) in [
        (
            doctype.to_string(),
            PdfErrorKind::Unsupported,
            "PDF feature XMP DTD/entity is refused by design",
        ),
        (
            "<x:xmpmeta".to_string(),
            PdfErrorKind::Malformed,
            "PDF malformed XMP",
        ),
        (
            "<x/>".to_string(),
            PdfErrorKind::Malformed,
            "PDF XMP lacks rdf:RDF",
        ),
        (
            nested,
            PdfErrorKind::Unsupported,
            "PDF feature XMP nested standard property is unsupported (a later release)",
        ),
    ] {
        let input = with_xmp(&["A"], &xmp, dictionary! {});
        let mut emitted = 0;
        let e = update_metadata(&input, &u, &l, &mut |b| {
            emitted += b.len();
            Ok(())
        })
        .unwrap_err();
        assert_eq!((e.kind(), e.to_string().as_str()), (kind, message));
        assert_eq!(emitted, 0);
    }
    let big = with_xmp(
        &["A"],
        &packet(3, &"<custom:Pad>x</custom:Pad>".repeat(100)),
        dictionary! {},
    );
    let small = PdfLimits::default()
        .with(Limit::MetadataBytes, 1000)
        .unwrap();
    let e = metadata(&big, &small).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Limit);
    assert_eq!(e.to_string(), "PDF metadata_bytes limit exceeded");
}
