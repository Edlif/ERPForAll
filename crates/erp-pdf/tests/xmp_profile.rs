// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use erp_pdf::{
    MetadataField as F, MetadataUpdate, PdfErrorKind, PdfLimits, metadata, update_metadata,
};
use lopdf::dictionary;
use support::*;
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const PDF: &str = "http://ns.adobe.com/pdf/1.3/";
const DC: &str = "http://purl.org/dc/elements/1.1/";
fn root(body: &str) -> String {
    format!("<x xmlns:r='{RDF}' xmlns:p='{PDF}' xmlns:d='{DC}' xmlns:c='urn:custom'>{body}</x>")
}
fn change(packet: &str, field: F, value: &str) -> (Vec<u8>, String) {
    let l = PdfLimits::default();
    let input = with_xmp(&["unchanged page"], packet, dictionary! {});
    let update = MetadataUpdate::new().text(field, value, &l).unwrap();
    let out = collect(|e| update_metadata(&input, &update, &l, e)).unwrap();
    assert!(out.starts_with(&input));
    assert_eq!(
        out,
        collect(|e| update_metadata(&input, &update, &l, e)).unwrap()
    );
    let d = reopen(&out, 1);
    assert!(d.extract_text(&[1]).unwrap().contains("unchanged page"));
    let m = metadata(&out, &l).unwrap();
    assert_eq!(m.info[&field], value);
    assert_eq!(m.xmp[&field], value);
    (out, m.packet.unwrap())
}

#[test]
fn multiple_roots_edit_document_subject_and_preserve_other_resources() {
    let other = "<r:Description r:about='urn:other' p:Producer='other'><p:Producer>different</p:Producer></r:Description>";
    let custom = "<c:resource r:parseType='Resource'><p:Producer>nested</p:Producer></c:resource><c:label>First</c:label><c:label>Second</c:label>";
    let packet = root(&format!(
        "<r:RDF>{other}</r:RDF><r:RDF><r:Description r:about=''><p:Producer>old</p:Producer>{custom}</r:Description></r:RDF>"
    ));
    let (_, out) = change(&packet, F::Producer, "new");
    assert!(out.contains(other));
    assert!(out.contains(custom));
    assert!(out.contains("<p:Producer>new</p:Producer>"));
}
#[test]
fn qualified_value_edit_keeps_qualifiers_and_significant_whitespace() {
    for inner in [
        "<p:Producer r:parseType='Resource'><r:value>old</r:value><c:note a='1'>keep &amp; stay</c:note></p:Producer>",
        "<p:Producer><r:Description><r:value>old</r:value><c:note a='1'>keep &amp; stay</c:note></r:Description></p:Producer>",
    ] {
        let p = root(&format!(
            "<r:RDF><r:Description r:about=''>{inner}</r:Description></r:RDF>"
        ));
        let (_, out) = change(&p, F::Producer, "  new & 請求書  ");
        assert_eq!(
            out,
            p.replace(
                "<r:value>old</r:value>",
                "<r:value>  new &amp; 請求書  </r:value>"
            )
        );
    }
}
#[test]
fn language_alternatives_preserve_translations_and_comments() {
    let p = root(
        "<r:RDF><r:Description><d:title><r:Alt><r:li xml:lang='x-default'>old</r:li><!-- keep --><r:li xml:lang='fr'>Titre</r:li></r:Alt></d:title></r:Description></r:RDF>",
    );
    let (_, out) = change(&p, F::Title, "new");
    assert_eq!(out, p.replace(">old<", ">new<"));
}
#[test]
fn empty_root_and_default_rdf_namespace_are_supported() {
    for p in [root("<r:RDF />"), format!("<RDF xmlns='{RDF}'/>")] {
        let (_, out) = change(&p, F::Title, "new");
        roxmltree::Document::parse(&out).unwrap();
    }
}
#[test]
fn attribute_whitespace_round_trips_exactly() {
    let p = root("<r:RDF><r:Description p:Producer='old'/></r:RDF>");
    let (_, out) = change(&p, F::Producer, " new\n\tvalue ");
    assert!(out.contains("&#10;"));
    assert!(out.contains("&#9;"));
}
#[test]
fn conflicting_subjects_and_duplicate_languages_fail_before_emitting() {
    let cases = [
        (
            root("<r:RDF><r:Description r:about='a'/><r:Description r:about='b'/></r:RDF>"),
            "PDF XMP document subject is ambiguous",
        ),
        (
            root(
                "<r:RDF><r:Description><d:title><r:Alt><r:li xml:lang='x-default'>a</r:li><r:li xml:lang='x-default'>b</r:li></r:Alt></d:title></r:Description></r:RDF>",
            ),
            "PDF XMP duplicate language alternative",
        ),
    ];
    let l = PdfLimits::default();
    for (p, message) in cases {
        let input = with_xmp(&["A"], &p, dictionary! {});
        let u = MetadataUpdate::new().text(F::Title, "new", &l).unwrap();
        let mut bytes = 0;
        let err = update_metadata(&input, &u, &l, &mut |b| {
            bytes += b.len();
            Ok(())
        })
        .unwrap_err();
        assert_eq!(err.kind(), PdfErrorKind::Malformed);
        assert_eq!(err.to_string(), message);
        assert_eq!(bytes, 0);
    }
}

#[test]
fn text_cdata_and_comments_are_edited_without_losing_comment_bytes() {
    let p = root(
        "<r:RDF><r:Description><p:Producer>one<![CDATA[two]]><!--untouched-->three</p:Producer></r:Description></r:RDF>",
    );
    let (_, out) = change(&p, F::Producer, "new & value");
    assert!(out.contains("<!--untouched-->"));
    assert!(!out.contains("two"));
}
#[test]
fn author_replacement_preserves_first_qualifiers_and_refuses_dropping_others() {
    let p = root(
        "<r:RDF><r:Description><d:creator><r:Seq><r:li r:parseType='Resource'><r:value>A</r:value><c:note>keep</c:note></r:li><r:li>B</r:li></r:Seq></d:creator></r:Description></r:RDF>",
    );
    let (_, out) = change(&p, F::Author, "C");
    assert!(out.contains("<c:note>keep</c:note>"));
    assert!(!out.contains("<r:li>B</r:li>"));
    let p = p.replace("<r:li>B</r:li>", "<r:li c:note='keep'>B</r:li>");
    let l = PdfLimits::default();
    let input = with_xmp(&["A"], &p, dictionary! {});
    let u = MetadataUpdate::new().text(F::Author, "C", &l).unwrap();
    let err = collect(|e| update_metadata(&input, &u, &l, e)).unwrap_err();
    assert_eq!(err.kind(), PdfErrorKind::Unsupported);
    assert_eq!(
        err.to_string(),
        "PDF feature XMP qualified author removal is refused by design"
    );
}
#[test]
fn qualified_attribute_and_missing_default_language_are_preserved() {
    let p = root(
        "<r:RDF><r:Description><p:Producer r:value='old' c:note='keep'/><d:title><r:Alt><r:li xml:lang='fr'>Titre</r:li></r:Alt></d:title></r:Description></r:RDF>",
    );
    let (_, out) = change(&p, F::Producer, "new");
    assert!(out.contains("c:note='keep'"));
    let (_, out) = change(&out, F::Title, "Title");
    assert!(out.contains("<r:li xml:lang='fr'>Titre</r:li>"));
    let doc = roxmltree::Document::parse(&out).unwrap();
    let alt = doc
        .descendants()
        .find(|n| n.tag_name().name() == "Alt")
        .unwrap();
    assert_eq!(
        alt.children()
            .find(|n| n.is_element())
            .unwrap()
            .attribute(("http://www.w3.org/XML/1998/namespace", "lang")),
        Some("x-default")
    );
}
#[test]
fn compatible_duplicates_update_all_and_conflicts_are_refused() {
    let p = root(
        "<r:RDF><r:Description p:Producer='old'/></r:RDF><r:RDF><r:Description><p:Producer>old</p:Producer></r:Description></r:RDF>",
    );
    let (_, out) = change(&p, F::Producer, "new");
    assert!(!out.contains("old"));
    let p = p.replace(">old<", ">conflict<");
    let input = with_xmp(&["A"], &p, dictionary! {});
    let e = metadata(&input, &PdfLimits::default()).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Malformed);
    assert_eq!(e.to_string(), "PDF XMP conflicting document properties");
}

#[test]
fn unrelated_update_refuses_conflicting_document_metadata_before_emitting() {
    let l = PdfLimits::default();
    let update = MetadataUpdate::new()
        .text(F::Title, "New title", &l)
        .unwrap();
    for packet in [
        root(
            "<r:RDF><r:Description p:Producer='Writer'/></r:RDF><r:RDF><r:Description><p:Producer>PDFBox</p:Producer></r:Description></r:RDF>",
        ),
        root(
            "<r:RDF xmlns:a='http://www.aiim.org/pdfa/ns/id/'><r:Description a:part='3'/><r:Description><a:part>2</a:part></r:Description></r:RDF>",
        ),
        root(
            "<r:RDF xmlns:a='http://www.aiim.org/pdfa/ns/id/'><r:Description a:part='3' a:conformance='B'/><r:Description><a:conformance>U</a:conformance></r:Description></r:RDF>",
        ),
        root(
            "<r:RDF xmlns:a='http://www.aiim.org/pdfa/ns/id/'><r:Description a:conformance='B'/><r:Description><a:conformance>U</a:conformance></r:Description></r:RDF>",
        ),
    ] {
        let input = with_xmp(&["Document page"], &packet, dictionary! {});
        let error = metadata(&input, &l).unwrap_err();
        assert_eq!(error.kind(), PdfErrorKind::Malformed);
        assert_eq!(error.to_string(), "PDF XMP conflicting document properties");

        let mut emitted = 0;
        let error = update_metadata(&input, &update, &l, &mut |bytes| {
            emitted += bytes.len();
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind(), PdfErrorKind::Malformed);
        assert_eq!(error.to_string(), "PDF XMP conflicting document properties");
        assert_eq!(emitted, 0);
    }
}

#[test]
fn nested_qualified_value_conflicts_fail_on_read_and_edit() {
    let l = PdfLimits::default();
    let update = MetadataUpdate::new()
        .text(F::Producer, "New producer", &l)
        .unwrap();
    for (body, message) in [
        (
            "<p:Producer r:value='Writer'><r:Description><r:value>PDFBox</r:value></r:Description></p:Producer>",
            "PDF XMP qualified value is ambiguous",
        ),
        (
            "<p:Producer r:value='Writer'><r:Description r:value='PDFBox'/></p:Producer>",
            "PDF XMP qualified value is ambiguous",
        ),
        (
            "<p:Producer r:parseType='Resource'><r:value>Writer</r:value><r:Description r:value='PDFBox'/></p:Producer>",
            "PDF XMP qualified value is ambiguous",
        ),
        (
            "<p:Producer><r:Description r:resource='urn:producer:writer'><r:value>PDFBox</r:value></r:Description></p:Producer>",
            "PDF XMP qualified Description has conflicting RDF attributes",
        ),
        (
            "<p:Producer><r:Description r:about='urn:producer:writer' r:value='PDFBox'/></p:Producer>",
            "PDF XMP qualified Description has conflicting RDF attributes",
        ),
    ] {
        let packet = root(&format!(
            "<r:RDF><r:Description>{body}</r:Description></r:RDF>"
        ));
        let input = with_xmp(&["Document page"], &packet, dictionary! {});
        let error = metadata(&input, &l).unwrap_err();
        assert_eq!(error.kind(), PdfErrorKind::Malformed);
        assert_eq!(error.to_string(), message);

        let mut emitted = 0;
        let error = update_metadata(&input, &update, &l, &mut |bytes| {
            emitted += bytes.len();
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind(), PdfErrorKind::Malformed);
        assert_eq!(error.to_string(), message);
        assert_eq!(emitted, 0);
    }
}

#[test]
fn xmp_expansion_depth_and_work_remain_bounded() {
    use erp_pdf::Limit;
    let p = root("<r:RDF><r:Description><p:Producer>old</p:Producer></r:Description></r:RDF>");
    let input = with_xmp(&["A"], &p, dictionary! {});
    let defaults = PdfLimits::default();
    let u = MetadataUpdate::new()
        .text(F::Producer, &"&".repeat(p.len()), &defaults)
        .unwrap();
    let limits = defaults.with(Limit::MetadataBytes, p.len()).unwrap();
    let mut emitted = 0;
    let err = update_metadata(&input, &u, &limits, &mut |b| {
        emitted += b.len();
        Ok(())
    })
    .unwrap_err();
    assert_eq!(err.kind(), PdfErrorKind::Limit);
    assert!(err.to_string().contains("metadata_bytes"));
    assert_eq!(emitted, 0);
    for (limit, n, name) in [
        (Limit::Depth, 4, "depth"),
        (Limit::WorkUnits, 1, "work_units"),
    ] {
        let l = PdfLimits::default().with(limit, n).unwrap();
        let e = metadata(&input, &l).unwrap_err();
        assert_eq!(e.kind(), PdfErrorKind::Limit);
        assert!(e.to_string().contains(name));
    }
}
#[test]
fn malformed_qualified_values_never_turn_into_empty_text() {
    let l = PdfLimits::default();
    for (body, message) in [
        (
            "<p:Producer r:parseType='Resource'><c:q>keep</c:q></p:Producer>",
            "PDF XMP qualified value lacks rdf:value",
        ),
        (
            "<p:Producer r:parseType='Resource'><r:value>a</r:value><r:value>b</r:value></p:Producer>",
            "PDF XMP qualified value is ambiguous",
        ),
        (
            "<p:Producer r:value='a'><r:value>b</r:value></p:Producer>",
            "PDF XMP qualified value is ambiguous",
        ),
    ] {
        let p = root(&format!(
            "<r:RDF><r:Description>{body}</r:Description></r:RDF>"
        ));
        let input = with_xmp(&["A"], &p, dictionary! {});
        let u = MetadataUpdate::new().text(F::Producer, "new", &l).unwrap();
        let mut emitted = 0;
        let e = update_metadata(&input, &u, &l, &mut |b| {
            emitted += b.len();
            Ok(())
        })
        .unwrap_err();
        assert_eq!(e.kind(), PdfErrorKind::Malformed);
        assert_eq!(e.to_string(), message);
        assert_eq!(emitted, 0);
    }
}

#[test]
fn decoded_array_copies_obey_generated_byte_admission() {
    use erp_pdf::Limit;
    let mut body = "long value ".repeat(900);
    for _ in 0..12 {
        body = format!("<r:Seq><r:li>{body}</r:li></r:Seq>");
    }
    let p = root(&format!(
        "<r:RDF><r:Description><p:Producer>{body}</p:Producer></r:Description></r:RDF>"
    ));
    let input = with_xmp(&["A"], &p, dictionary! {});
    let l = PdfLimits::default()
        .with(Limit::OutputBytes, 50_000)
        .unwrap();
    let e = metadata(&input, &l).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Limit);
    assert_eq!(
        e.to_string(),
        "PDF copied/generated data exceeds output_bytes limit"
    );
}

#[test]
fn default_language_case_is_consistent_and_mixed_qualified_wrappers_fail() {
    let p = root(
        "<r:RDF><r:Description><d:title><r:Alt><r:li xml:lang='X-DEFAULT'>old</r:li></r:Alt></d:title></r:Description></r:RDF>",
    );
    let (_, out) = change(&p, F::Title, "new");
    assert_eq!(out, p.replace(">old<", ">new<"));
    let p = root(
        "<r:RDF><r:Description><p:Producer>stray<r:Description><r:value>old</r:value></r:Description></p:Producer></r:Description></r:RDF>",
    );
    let input = with_xmp(&["A"], &p, dictionary! {});
    let l = PdfLimits::default();
    let u = MetadataUpdate::new().text(F::Producer, "new", &l).unwrap();
    let mut emitted = 0;
    let e = update_metadata(&input, &u, &l, &mut |b| {
        emitted += b.len();
        Ok(())
    })
    .unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Malformed);
    assert_eq!(e.to_string(), "PDF XMP mixed structured value");
    assert_eq!(emitted, 0);
}
