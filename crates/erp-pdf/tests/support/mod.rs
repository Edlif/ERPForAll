// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(dead_code)]
use lopdf::{Document, Object, Stream, dictionary};

pub fn fixture(labels: &[&str]) -> Vec<u8> {
    let mut d = Document::with_version("1.7");
    let root = d.new_object_id();
    let font =
        d.add_object(dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica"});
    let resources = d.add_object(dictionary! {"Font"=>dictionary!{"F1"=>font}});
    let mut pages = Vec::new();
    for label in labels {
        let content = lopdf::content::Content {
            operations: vec![
                lopdf::content::Operation::new("BT", vec![]),
                lopdf::content::Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 18.into()]),
                lopdf::content::Operation::new("Td", vec![20.into(), 80.into()]),
                lopdf::content::Operation::new("Tj", vec![Object::string_literal(*label)]),
                lopdf::content::Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap();
        let stream = d.add_object(Stream::new(dictionary! {}, content));
        let id = d.add_object(dictionary! {"Type"=>"Page", "Parent"=>root,"Contents"=>stream});
        pages.push(Object::Reference(id));
    }
    d.objects.insert(root,Object::Dictionary(dictionary!{"Type"=>"Pages","Kids"=>pages,"Count"=>labels.len() as i64,"MediaBox"=>vec![0.into(),0.into(),200.into(),120.into()],"Resources"=>resources}));
    let catalog = d.add_object(dictionary! {"Type"=>"Catalog","Pages"=>root});
    d.trailer.set("Root", catalog);
    bytes(d)
}
pub fn bytes(mut d: Document) -> Vec<u8> {
    let mut b = Vec::new();
    d.save_to(&mut b).unwrap();
    b
}
pub fn reopen(b: &[u8], count: usize) -> Document {
    let d = Document::load_mem(b).unwrap();
    assert_eq!(d.get_pages().len(), count);
    let h = hayro::hayro_syntax::Pdf::new(b.to_vec()).unwrap();
    assert_eq!(h.pages().len(), count);
    d
}
pub fn raster(b: &[u8]) -> Vec<u8> {
    raster_page(b, 0)
}
pub fn raster_page(b: &[u8], page: usize) -> Vec<u8> {
    let h = hayro::hayro_syntax::Pdf::new(b.to_vec()).unwrap();
    hayro::render(
        &h.pages()[page],
        &hayro::RenderCache::new(),
        &Default::default(),
        &Default::default(),
        &Default::default(),
    )
    .into_png()
    .unwrap()
}
/// Inspect text in emitted Form XObjects using their own resources (fixture helper).
pub fn form_text(bytes: &[u8]) -> String {
    let d = Document::load_mem(bytes).unwrap();
    let mut text = String::new();
    for o in d.objects.values() {
        let Ok(s) = o.as_stream() else { continue };
        if !matches!(
            s.dict.get(b"Subtype").and_then(Object::as_name),
            Ok(b"Form")
        ) {
            continue;
        }
        let mut isolated = d.clone();
        let page = isolated.get_pages()[&1];
        let content = isolated.add_object(s.clone());
        let p = isolated.get_dictionary_mut(page).unwrap();
        p.set("Contents", content);
        p.set("Resources", s.dict.get(b"Resources").unwrap().clone());
        text.push_str(&isolated.extract_text(&[1]).unwrap());
    }
    text
}
/// Fixture with an XMP packet and an Info dictionary (archival claims are fixture text only).
pub fn with_xmp(labels: &[&str], xmp: &str, info: lopdf::Dictionary) -> Vec<u8> {
    let mut d = Document::load_mem(&fixture(labels)).unwrap();
    let m = d.add_object(Stream::new(
        dictionary! {"Type"=>"Metadata","Subtype"=>"XML"},
        xmp.as_bytes().to_vec(),
    ));
    let info = d.add_object(info);
    d.trailer.set("Info", info);
    let root = d.trailer.get(b"Root").unwrap().as_reference().unwrap();
    d.get_dictionary_mut(root).unwrap().set("Metadata", m);
    bytes(d)
}
/// XMP packet declaring a PDF/A part/level plus caller properties.
pub fn packet(part: u8, body: &str) -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\" \
         xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" xmlns:custom=\"urn:example:custom\">\
         <pdfaid:part>{part}</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance>\
         <pdf:Producer>Fixture Producer</pdf:Producer>{body}</rdf:Description></rdf:RDF></x:xmpmeta>\
         <?xpacket end=\"w\"?>"
    )
}
pub fn collect(
    f: impl FnOnce(&mut erp_pdf::Emitter<'_>) -> Result<(), erp_pdf::PdfError>,
) -> Result<Vec<u8>, erp_pdf::PdfError> {
    let mut out = Vec::new();
    f(&mut |b: &[u8]| {
        out.extend_from_slice(b);
        Ok(())
    })?;
    Ok(out)
}
pub fn date() -> erp_pdf::PdfDate {
    erp_pdf::PdfDate::new(2026, 11, 15, 9, 30, 0, 420).unwrap()
}
