// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use erp_pdf::{
    ConformanceLevel, EInvoiceName, EInvoiceOptions, MetadataField, PdfErrorKind, PdfLimits,
    Relationship, einvoice, embedded_files, merge, metadata,
};
use lopdf::{Object, dictionary};
use support::*;

/// Original minimal CII document; only the guideline identifier matters to the container.
fn cii(guideline: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rsm:CrossIndustryInvoice \
         xmlns:rsm=\"urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100\" \
         xmlns:ram=\"urn:un:unece:uncefact:data:standard:ReusableAggregateBusinessInformationEntity:100\">\
         <rsm:ExchangedDocumentContext><ram:GuidelineSpecifiedDocumentContextParameter>\
         <ram:ID>{guideline}</ram:ID></ram:GuidelineSpecifiedDocumentContextParameter>\
         </rsm:ExchangedDocumentContext><rsm:ExchangedDocument><ram:ID>INV/2026/0001</ram:ID>\
         </rsm:ExchangedDocument></rsm:CrossIndustryInvoice>"
    )
    .into_bytes()
}
fn invoice_pdf() -> Vec<u8> {
    with_xmp(
        &["INVOICE INV/2026/0001"],
        &packet(3, "<custom:Note>keep</custom:Note>"),
        dictionary! {"Producer"=>Object::string_literal("Fixture Producer")},
    )
}
fn opts(name: EInvoiceName, l: &PdfLimits) -> EInvoiceOptions {
    EInvoiceOptions::new(name, "Factur-X invoice INV/2026/0001", date(), l).unwrap()
}
fn fx(packet: &str, local: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(packet).unwrap();
    doc.descendants()
        .find(|n| {
            n.tag_name().namespace() == Some("urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#")
                && n.tag_name().name() == local
        })
        .and_then(|n| n.text().map(str::to_string))
}

#[test]
fn invoice_xml_is_embedded_as_associated_alternative() {
    let l = PdfLimits::default();
    let input = invoice_pdf();
    let xml = cii("urn:cen.eu:en16931:2017");
    let out =
        collect(|e| einvoice(&input, &xml, &opts(EInvoiceName::FacturX, &l), &l, e).map(|_| ()))
            .unwrap();
    assert!(
        out.starts_with(&input),
        "post-pass leaves the original bytes as a prefix"
    );
    reopen(&out, 1);
    let files = embedded_files(&out, &l).unwrap();
    assert_eq!(files.len(), 1);
    let f = &files[0];
    assert_eq!(f.name, "factur-x.xml");
    assert_eq!(f.data, xml);
    assert_eq!(f.mime.as_deref(), Some("text/xml"));
    assert_eq!(f.relationship, Some(Relationship::Alternative));
    assert_eq!(
        f.description.as_deref(),
        Some("Factur-X invoice INV/2026/0001")
    );
    assert!(f.associated);
    let again =
        collect(|e| einvoice(&input, &xml, &opts(EInvoiceName::FacturX, &l), &l, e).map(|_| ()))
            .unwrap();
    assert_eq!(out, again, "byte-identical re-run");
}

#[test]
fn conformance_levels_follow_the_xml_profile_and_pdfa_level_is_kept() {
    let l = PdfLimits::default();
    let input = invoice_pdf();
    for (guideline, level, rel) in [
        (
            "urn:factur-x.eu:1p0:minimum",
            ConformanceLevel::Minimum,
            Relationship::Data,
        ),
        (
            "urn:factur-x.eu:1p0:basicwl",
            ConformanceLevel::BasicWl,
            Relationship::Data,
        ),
        (
            "urn:cen.eu:en16931:2017#compliant#urn:factur-x.eu:1p0:basic",
            ConformanceLevel::Basic,
            Relationship::Alternative,
        ),
        (
            "urn:cen.eu:en16931:2017#conformant#urn:factur-x.eu:1p0:extended",
            ConformanceLevel::Extended,
            Relationship::Alternative,
        ),
    ] {
        let mut got = None;
        let out = collect(|e| {
            got = Some(einvoice(
                &input,
                &cii(guideline),
                &opts(EInvoiceName::FacturX, &l),
                &l,
                e,
            )?);
            Ok(())
        })
        .unwrap();
        assert_eq!(got, Some(level));
        let m = metadata(&out, &l).unwrap();
        let packet = m.packet.clone().unwrap();
        assert_eq!(
            fx(&packet, "ConformanceLevel").as_deref(),
            Some(level.as_str())
        );
        assert_eq!(
            fx(&packet, "DocumentFileName").as_deref(),
            Some("factur-x.xml")
        );
        assert_eq!(fx(&packet, "DocumentType").as_deref(), Some("INVOICE"));
        assert!(packet.contains("<pdfaSchema:namespaceURI>urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#</pdfaSchema:namespaceURI>"));
        assert!(packet.contains("<custom:Note>keep</custom:Note>"));
        let id = m.pdfa.clone().unwrap();
        assert_eq!(
            (id.part, id.conformance.as_deref()),
            (3, Some("B")),
            "pdfaid level is the producer's, never forced to A"
        );
        let p = MetadataField::Producer;
        assert_eq!(m.info.get(&p), m.xmp.get(&p), "Info and XMP agree");
        assert_eq!(embedded_files(&out, &l).unwrap()[0].relationship, Some(rel));
    }
    let xr = cii("urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0");
    let out =
        collect(|e| einvoice(&input, &xr, &opts(EInvoiceName::XRechnung, &l), &l, e).map(|_| ()))
            .unwrap();
    assert_eq!(
        fx(
            &metadata(&out, &l).unwrap().packet.unwrap(),
            "ConformanceLevel"
        )
        .as_deref(),
        Some("XRECHNUNG")
    );
}

#[test]
fn container_refusals_are_explicit() {
    let l = PdfLimits::default();
    let xml = cii("urn:cen.eu:en16931:2017");
    let o = opts(EInvoiceName::FacturX, &l);
    let check = |input: &[u8], xml: &[u8], o: &EInvoiceOptions, kind, message: &str| {
        let mut emitted = 0;
        let e = einvoice(input, xml, o, &l, &mut |b| {
            emitted += b.len();
            Ok(())
        })
        .unwrap_err();
        assert_eq!((e.kind(), e.to_string().as_str()), (kind, message));
        assert_eq!(emitted, 0);
    };
    check(
        &fixture(&["A"]),
        &xml,
        &o,
        PdfErrorKind::Unsupported,
        "PDF e-invoice container requires PDF/A-3: input has no XMP metadata (a later release)",
    );
    check(
        &with_xmp(&["A"], &packet(2, ""), dictionary! {}),
        &xml,
        &o,
        PdfErrorKind::Unsupported,
        "PDF e-invoice container requires PDF/A-3: input is PDF/A-2, not PDF/A-3 (a later release)",
    );
    check(
        &invoice_pdf(),
        &cii("urn:example:unknown"),
        &o,
        PdfErrorKind::InvalidOptions,
        "PDF e-invoice guideline \"urn:example:unknown\" is not a known profile",
    );
    check(
        &invoice_pdf(),
        &cii("urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0"),
        &o,
        PdfErrorKind::InvalidOptions,
        "PDF e-invoice profile XRECHNUNG requires xrechnung.xml",
    );
    check(
        &invoice_pdf(),
        b"<Invoice xmlns=\"urn:oasis:names:specification:ubl:schema:xsd:Invoice-2\"/>",
        &o,
        PdfErrorKind::InvalidOptions,
        "PDF e-invoice XML must be a UN/CEFACT CrossIndustryInvoice",
    );
    let once = collect(|e| einvoice(&invoice_pdf(), &xml, &o, &l, e).map(|_| ())).unwrap();
    check(
        &once,
        &xml,
        &o,
        PdfErrorKind::InvalidOptions,
        "PDF input already carries Factur-X metadata",
    );

    use lopdf::encryption::{EncryptionState, EncryptionVersion, Permissions};
    let mut d = lopdf::Document::load_mem(&invoice_pdf()).unwrap();
    d.trailer.set(
        "ID",
        vec![
            Object::string_literal(vec![1; 16]),
            Object::string_literal(vec![2; 16]),
        ],
    );
    let state = EncryptionState::try_from(EncryptionVersion::V1 {
        document: &d,
        owner_password: "fixture-owner",
        user_password: "",
        permissions: Permissions::all(),
    })
    .unwrap();
    d.encrypt(&state).unwrap();
    check(
        &bytes(d),
        &xml,
        &o,
        PdfErrorKind::Unsupported,
        "PDF feature encryption is unsupported (a later release)",
    );
}

#[test]
fn merged_download_of_archival_invoices_is_plain() {
    let l = PdfLimits::default();
    let xml = cii("urn:cen.eu:en16931:2017");
    let one = collect(|e| {
        einvoice(
            &invoice_pdf(),
            &xml,
            &opts(EInvoiceName::FacturX, &l),
            &l,
            e,
        )
        .map(|_| ())
    })
    .unwrap();
    let merged = collect(|e| merge([one.clone(), one], &l, e)).unwrap();
    reopen(&merged, 2);
    let m = metadata(&merged, &l).unwrap();
    assert!(m.pdfa.is_none() && m.packet.is_none());
    assert!(embedded_files(&merged, &l).unwrap().is_empty());
}

// Original synthetic packets exercise shapes not present in the producer samples.
#[test]
fn existing_schema_bags_accept_aliases_and_refuse_ambiguous_or_repeated_schemas() {
    const EXT: &str = "http://www.aiim.org/pdfa/ns/extension/";
    const SCHEMA: &str = "http://www.aiim.org/pdfa/ns/schema#";
    const FX: &str = "urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#";
    let l = PdfLimits::default();
    let xml = cii("urn:cen.eu:en16931:2017");
    let schema = |body: &str| format!("<e:schemas xmlns:e='{EXT}'>{body}</e:schemas>");
    for inner in [
        "<rdf:Bag/>",
        "<rdf:Bag></rdf:Bag>",
        "<q:Bag xmlns:q='http://www.w3.org/1999/02/22-rdf-syntax-ns#'/>",
    ] {
        let packet = packet(3, &schema(inner));
        let input = with_xmp(&["A"], &packet, dictionary! {});
        let out = collect(|e| {
            einvoice(&input, &xml, &opts(EInvoiceName::FacturX, &l), &l, e).map(|_| ())
        })
        .unwrap();
        assert!(out.starts_with(&input));
        let m = metadata(&out, &l).unwrap();
        let p = m.packet.unwrap();
        let d = roxmltree::Document::parse(&p).unwrap();
        assert_eq!(
            d.descendants()
                .filter(|n| n.has_tag_name((EXT, "schemas")))
                .count(),
            1
        );
        assert_eq!(fx(&p, "DocumentFileName").as_deref(), Some("factur-x.xml"));
        assert_eq!(m.pdfa.unwrap().conformance.as_deref(), Some("B"));
    }
    let existing = schema(&format!(
        "<rdf:Bag><rdf:li rdf:parseType='Resource' xmlns:s='{SCHEMA}' s:namespaceURI='{FX}'/></rdf:Bag>"
    ));
    for (body, kind, message) in [
        (
            schema("<rdf:Seq/>"),
            PdfErrorKind::Malformed,
            "PDF XMP extension schemas must hold one rdf:Bag",
        ),
        (
            format!("{}{}", schema("<rdf:Bag/>"), schema("<rdf:Bag/>")),
            PdfErrorKind::Malformed,
            "PDF XMP ambiguous extension schema containers",
        ),
        (
            existing,
            PdfErrorKind::InvalidOptions,
            "PDF input already carries Factur-X metadata",
        ),
    ] {
        let input = with_xmp(&["A"], &packet(3, &body), dictionary! {});
        let mut emitted = 0;
        let err = einvoice(
            &input,
            &xml,
            &opts(EInvoiceName::FacturX, &l),
            &l,
            &mut |b| {
                emitted += b.len();
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(err.kind(), kind);
        assert_eq!(err.to_string(), message);
        assert_eq!(emitted, 0);
    }
}

#[test]
fn contradictory_or_ambiguous_schema_resources_are_malformed() {
    let l = PdfLimits::default();
    let xml = cii("urn:cen.eu:en16931:2017");
    let o = opts(EInvoiceName::FacturX, &l);
    for (item, message) in [
        (
            "<rdf:li rdf:parseType='Resource'>stray<s:namespaceURI>urn:other</s:namespaceURI></rdf:li>",
            "PDF XMP mixed structured value",
        ),
        (
            "<rdf:li rdf:resource='urn:else' rdf:parseType='Resource'><s:namespaceURI>urn:other</s:namespaceURI></rdf:li>",
            "PDF XMP schema resource has conflicting RDF attributes",
        ),
        (
            "<rdf:li rdf:parseType='Resource'><s:schema>Missing namespace</s:schema></rdf:li>",
            "PDF XMP schema lacks namespaceURI",
        ),
        (
            "<rdf:li rdf:parseType='Resource' s:namespaceURI='urn:first'><s:namespaceURI>urn:second</s:namespaceURI></rdf:li>",
            "PDF XMP schema namespaceURI is ambiguous",
        ),
    ] {
        let body = format!(
            "<e:schemas xmlns:e='http://www.aiim.org/pdfa/ns/extension/' xmlns:s='http://www.aiim.org/pdfa/ns/schema#'><rdf:Bag>{item}</rdf:Bag></e:schemas>"
        );
        let input = with_xmp(&["A"], &packet(3, &body), dictionary! {});
        let mut emitted = 0;
        let e = einvoice(&input, &xml, &o, &l, &mut |b| {
            emitted += b.len();
            Ok(())
        })
        .unwrap_err();
        assert_eq!(e.kind(), PdfErrorKind::Malformed);
        assert_eq!(e.to_string(), message);
        assert_eq!(emitted, 0);
    }
}

fn extension_schema_item(item: &str) -> String {
    format!(
        "<e:schemas xmlns:e='http://www.aiim.org/pdfa/ns/extension/' xmlns:s='http://www.aiim.org/pdfa/ns/schema#'><rdf:Bag>{item}</rdf:Bag></e:schemas>"
    )
}

fn refuse_nested_schema(item: &str, message: &str) {
    let l = PdfLimits::default();
    let input = with_xmp(
        &["A"],
        &packet(3, &extension_schema_item(item)),
        dictionary! {},
    );
    let xml = cii("urn:cen.eu:en16931:2017");
    let mut emitted = 0;
    let error = einvoice(
        &input,
        &xml,
        &opts(EInvoiceName::FacturX, &l),
        &l,
        &mut |bytes| {
            emitted += bytes.len();
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), PdfErrorKind::Malformed);
    assert_eq!(error.to_string(), message);
    assert_eq!(emitted, 0);
}

#[test]
fn nested_schema_refuses_an_outer_namespace_without_output() {
    refuse_nested_schema(
        "<rdf:li s:namespaceURI='urn:outer'><rdf:Description><s:namespaceURI>urn:inner</s:namespaceURI></rdf:Description></rdf:li>",
        "PDF XMP schema namespaceURI is ambiguous",
    );
}

#[test]
fn nested_schema_refuses_description_parse_type_without_output() {
    refuse_nested_schema(
        "<rdf:li><rdf:Description rdf:parseType='Resource'><s:namespaceURI>urn:inner</s:namespaceURI></rdf:Description></rdf:li>",
        "PDF XMP schema item must be a resource",
    );
}

#[test]
fn nested_schema_preserves_supported_namespace_forms() {
    let limits = PdfLimits::default();
    let xml = cii("urn:cen.eu:en16931:2017");
    for description in [
        "<rdf:Description s:namespaceURI='urn:other'/>",
        "<rdf:Description><s:namespaceURI>urn:other</s:namespaceURI></rdf:Description>",
    ] {
        let item = format!("<rdf:li>{description}</rdf:li>");
        let body = extension_schema_item(&item);
        let input = with_xmp(&["A"], &packet(3, &body), dictionary! {});
        let output = collect(|emit| {
            einvoice(
                &input,
                &xml,
                &opts(EInvoiceName::FacturX, &limits),
                &limits,
                emit,
            )
            .map(|_| ())
        })
        .unwrap();
        let packet = metadata(&output, &limits).unwrap().packet.unwrap();
        assert!(packet.contains(&item), "{packet}");
        assert_eq!(
            fx(&packet, "DocumentFileName").as_deref(),
            Some("factur-x.xml")
        );
    }
}
