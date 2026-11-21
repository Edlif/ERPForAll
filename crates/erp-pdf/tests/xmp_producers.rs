// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;

use erp_pdf::{
    EInvoiceName, EInvoiceOptions, MetadataField as F, MetadataUpdate, PdfErrorKind, PdfLimits,
    Relationship, einvoice, embedded_files, metadata, update_metadata,
};
use lopdf::dictionary;
use support::*;

const LIBREOFFICE: &str = include_str!("fixtures/xmp/libreoffice-pdfa3.xmp");
const PDFBOX: &str = include_str!("fixtures/xmp/pdfbox-notepad-a1b.xmp");
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const EXT: &str = "http://www.aiim.org/pdfa/ns/extension/";
const SCHEMA: &str = "http://www.aiim.org/pdfa/ns/schema#";
const FX: &str = "urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#";

fn raw_element<'a>(packet: &'a str, namespace: &str, local: &str) -> &'a str {
    let doc = roxmltree::Document::parse(packet).unwrap();
    let node = doc
        .descendants()
        .find(|n| n.has_tag_name((namespace, local)))
        .unwrap();
    &packet[node.range()]
}

fn assert_unchanged_page(input: &[u8], output: &[u8]) {
    assert!(output.starts_with(input));
    let before = lopdf::Document::load_mem(input).unwrap();
    let after = reopen(output, 1);
    assert_eq!(
        before.get_page_content(before.get_pages()[&1]),
        after.get_page_content(after.get_pages()[&1])
    );
}

#[test]
fn libreoffice_title_edit_preserves_every_other_packet_byte() {
    let limits = PdfLimits::default();
    let input = with_xmp(&["Original public fixture"], LIBREOFFICE, dictionary! {});
    let before = metadata(&input, &limits).unwrap();
    assert_eq!(before.xmp[&F::Title], "Original XMP fixture");
    assert_eq!(before.xmp[&F::Author], "Fixture Author");
    let update = MetadataUpdate::new()
        .text(F::Title, "Revised & <public> title", &limits)
        .unwrap();
    let output = collect(|emit| update_metadata(&input, &update, &limits, emit)).unwrap();
    assert_unchanged_page(&input, &output);
    let after = metadata(&output, &limits).unwrap();
    assert_eq!(after.info[&F::Title], "Revised & <public> title");
    assert_eq!(after.xmp[&F::Title], "Revised & <public> title");
    assert_eq!(
        after.packet.unwrap(),
        LIBREOFFICE.replace("Original XMP fixture", "Revised &amp; &lt;public&gt; title")
    );
    assert_eq!(
        output,
        collect(|emit| update_metadata(&input, &update, &limits, emit)).unwrap()
    );
}

#[test]
fn upstream_preflight_schema_bag_survives_metadata_edit_byte_for_byte() {
    let limits = PdfLimits::default();
    let input = with_xmp(&["Public upstream packet"], PDFBOX, dictionary! {});
    let before = metadata(&input, &limits).unwrap();
    assert_eq!(before.xmp[&F::Title], "Notepad");
    assert_eq!(before.xmp[&F::Author], "Bill");
    let update = MetadataUpdate::new()
        .text(F::Title, "Updated public fixture", &limits)
        .unwrap()
        .text(F::Author, "Replacement Author", &limits)
        .unwrap();
    let output = collect(|emit| update_metadata(&input, &update, &limits, emit)).unwrap();
    assert_unchanged_page(&input, &output);
    let after = metadata(&output, &limits).unwrap();
    assert_eq!(after.info[&F::Author], "Replacement Author");
    assert_eq!(after.xmp[&F::Author], "Replacement Author");
    assert_eq!(after.info[&F::Title], "Updated public fixture");
    assert_eq!(after.xmp[&F::Title], "Updated public fixture");
    let packet = after.packet.unwrap();
    assert_eq!(
        raw_element(&packet, EXT, "schemas"),
        raw_element(PDFBOX, EXT, "schemas")
    );
    assert_eq!(
        raw_element(&packet, "http://ns.adobe.com/xap/1.0/mm/", "History"),
        raw_element(PDFBOX, "http://ns.adobe.com/xap/1.0/mm/", "History")
    );
    let doc = roxmltree::Document::parse(&packet).unwrap();
    let creator = doc
        .descendants()
        .find(|n| n.has_tag_name((DC, "creator")))
        .unwrap();
    let items: Vec<_> = creator
        .descendants()
        .filter(|n| n.has_tag_name((RDF, "li")))
        .collect();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].text(), Some("Replacement Author"));
}

// Synthetic mutation: the upstream packet claims PDF/A-1, not PDF/A-3. Changing
// this declaration exercises structural admission; it does not certify PDF/A.
#[test]
fn synthetic_pdfa3_mutation_appends_schema_without_changing_existing_entries() {
    let limits = PdfLimits::default();
    let packet = PDFBOX.replace(
        "<pdfaid:part>1</pdfaid:part>",
        "<pdfaid:part>3</pdfaid:part>",
    );
    let input = with_xmp(&["Synthetic PDF/A-3 declaration"], &packet, dictionary! {});
    let xml = br#"<rsm:CrossIndustryInvoice xmlns:rsm="urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100" xmlns:ram="urn:un:unece:uncefact:data:standard:ReusableAggregateBusinessInformationEntity:100"><rsm:ExchangedDocumentContext><ram:GuidelineSpecifiedDocumentContextParameter><ram:ID>urn:cen.eu:en16931:2017</ram:ID></ram:GuidelineSpecifiedDocumentContextParameter></rsm:ExchangedDocumentContext></rsm:CrossIndustryInvoice>"#;
    let options =
        EInvoiceOptions::new(EInvoiceName::FacturX, "Public fixture", date(), &limits).unwrap();
    let output =
        collect(|emit| einvoice(&input, xml, &options, &limits, emit).map(|_| ())).unwrap();
    assert_unchanged_page(&input, &output);
    let after = metadata(&output, &limits).unwrap();
    assert_eq!(after.pdfa.unwrap().part, 3);
    let after = after.packet.unwrap();
    let before_doc = roxmltree::Document::parse(&packet).unwrap();
    let after_doc = roxmltree::Document::parse(&after).unwrap();
    let bag = |doc: &'_ roxmltree::Document<'_>| {
        doc.descendants()
            .find(|n| n.has_tag_name((EXT, "schemas")))
            .unwrap()
            .children()
            .find(|n| n.has_tag_name((RDF, "Bag")))
            .unwrap()
            .range()
    };
    let before_range = bag(&before_doc);
    let after_range = bag(&after_doc);
    let old_bag = &packet[before_range];
    let new_bag = &after[after_range];
    // The contiguous original inner bytes include whitespace between entries.
    let inner_start = old_bag.find('>').unwrap() + 1;
    let inner_end = old_bag.rfind("</rdf:Bag>").unwrap();
    assert!(new_bag.contains(&old_bag[inner_start..inner_end]));
    let old_entries = before_doc
        .descendants()
        .find(|n| n.has_tag_name((EXT, "schemas")))
        .unwrap()
        .children()
        .find(|n| n.has_tag_name((RDF, "Bag")))
        .unwrap()
        .children()
        .filter(|n| n.has_tag_name((RDF, "li")))
        .count();
    let new_entries = after_doc
        .descendants()
        .find(|n| n.has_tag_name((EXT, "schemas")))
        .unwrap()
        .children()
        .find(|n| n.has_tag_name((RDF, "Bag")))
        .unwrap()
        .children()
        .filter(|n| n.has_tag_name((RDF, "li")))
        .count();
    assert_eq!(new_entries, old_entries + 1);
    assert_eq!(
        after_doc
            .descendants()
            .filter(|n| n.has_tag_name((SCHEMA, "namespaceURI")) && n.text() == Some(FX))
            .count(),
        1
    );
    let files = embedded_files(&output, &limits).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].data, xml);
    assert_eq!(files[0].name, "factur-x.xml");
    assert_eq!(files[0].mime.as_deref(), Some("text/xml"));
    assert_eq!(files[0].relationship, Some(Relationship::Alternative));
}

// These language alternatives are original synthetic additions, not observed
// LibreOffice output. Both x-default-present and x-default-absent are covered.
#[test]
fn synthetic_translations_survive_default_title_update_and_insertion() {
    let translation = "<rdf:li xml:lang=\"id-ID\">Judul &amp; asli</rdf:li>";
    let default = "<rdf:li xml:lang=\"x-default\">Original XMP fixture</rdf:li>";
    for keep_default in [true, false] {
        let replacement = if keep_default {
            format!("{default}{translation}")
        } else {
            translation.to_owned()
        };
        let packet = LIBREOFFICE.replace(default, &replacement);
        let input = with_xmp(&["Synthetic translation"], &packet, dictionary! {});
        let limits = PdfLimits::default();
        let update = MetadataUpdate::new()
            .text(F::Title, "New default", &limits)
            .unwrap();
        let output = collect(|emit| update_metadata(&input, &update, &limits, emit)).unwrap();
        let after = metadata(&output, &limits).unwrap();
        assert_eq!(after.xmp[&F::Title], "New default");
        let packet = after.packet.unwrap();
        assert!(packet.contains(translation));
        let doc = roxmltree::Document::parse(&packet).unwrap();
        let title = doc
            .descendants()
            .find(|n| n.has_tag_name((DC, "title")))
            .unwrap();
        let items: Vec<_> = title
            .descendants()
            .filter(|n| n.has_tag_name((RDF, "li")))
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].attribute(("http://www.w3.org/XML/1998/namespace", "lang")),
            Some("x-default")
        );
        assert_eq!(items[0].text(), Some("New default"));
    }
}

#[test]
fn synthetic_doctype_mutations_are_refused_before_any_output() {
    let limits = PdfLimits::default();
    let update = MetadataUpdate::new()
        .text(F::Title, "New title", &limits)
        .unwrap();
    for declaration in [
        "<!DOCTYPE x:xmpmeta>",
        "<!DOCTYPE x:xmpmeta [<!ENTITY fixture 'replacement'>]>",
        "<!DOCTYPE x:xmpmeta SYSTEM 'https://example.invalid/fixture.dtd'>",
    ] {
        let packet = format!("{declaration}{LIBREOFFICE}");
        let input = with_xmp(&["Synthetic DTD"], &packet, dictionary! {});
        let mut emitted = 0;
        let error = update_metadata(&input, &update, &limits, &mut |chunk| {
            emitted += chunk.len();
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind(), PdfErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "PDF feature XMP DTD/entity is refused by design"
        );
        assert_eq!(emitted, 0);
    }
}

#[test]
fn pdfbox_adobe_packet_updates_only_the_document_producer() {
    let input = include_bytes!("fixtures/xmp/pdfbox-generated/qualified-and-nested.pdf");
    let limits = PdfLimits::default();
    let before = metadata(input, &limits).unwrap();
    assert_eq!(before.xmp[&F::Producer], "Document tool");
    let packet = before.packet.unwrap();
    assert!(packet.contains("<rdf:value>Document tool</rdf:value>"));
    assert!(packet.contains("pdf:Producer=\"Source tool\""));
    let change = MetadataUpdate::new()
        .text(F::Producer, "Revised & reviewed", &limits)
        .unwrap();
    let out = collect(|emit| update_metadata(input, &change, &limits, emit)).unwrap();
    assert_unchanged_page(input, &out);
    let after = metadata(&out, &limits).unwrap();
    assert_eq!(after.info[&F::Producer], "Revised & reviewed");
    assert_eq!(after.xmp[&F::Producer], "Revised & reviewed");
    assert_eq!(
        after.packet.unwrap(),
        packet.replace("Document tool", "Revised &amp; reviewed")
    );
}

#[test]
fn pdfbox_empty_xmpbox_packet_accepts_its_first_field() {
    let input = include_bytes!("fixtures/xmp/pdfbox-generated/empty.pdf");
    let limits = PdfLimits::default();
    let before = metadata(input, &limits).unwrap();
    assert!(before.xmp.is_empty());
    let packet = before.packet.unwrap();
    assert!(raw_element(&packet, RDF, "RDF").ends_with("/>"));
    let change = MetadataUpdate::new()
        .text(F::Title, "Incoming receipt", &limits)
        .unwrap();
    let out = collect(|emit| update_metadata(input, &change, &limits, emit)).unwrap();
    assert_unchanged_page(input, &out);
    let after = metadata(&out, &limits).unwrap();
    assert_eq!(after.info[&F::Title], "Incoming receipt");
    assert_eq!(after.xmp[&F::Title], "Incoming receipt");
    let output = after.packet.unwrap();
    let root = raw_element(&packet, RDF, "RDF");
    let edited = raw_element(&output, RDF, "RDF");
    assert_eq!(output.replacen(edited, root, 1), packet);
    assert!(edited.ends_with("</rdf:RDF>"));
}
