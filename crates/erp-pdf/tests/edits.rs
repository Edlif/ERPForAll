// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use erp_pdf::{PageSelection, PdfLimits, StampPlacement, stamp};
use support::*;
#[test]
fn overlay_is_visible_and_preserves_target() {
    let l = PdfLimits::default();
    let input = fixture(&["ORIGINAL"]);
    let overlay = fixture(&["PAID"]);
    let mut out = Vec::new();
    stamp(
        &input,
        &overlay,
        &PageSelection::new(vec![1], &l).unwrap(),
        &StampPlacement::new([1., 0., 0., 1., 0., -35.]).unwrap(),
        &l,
        &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        },
    )
    .unwrap();
    let d = reopen(&out, 1);
    assert!(d.extract_text(&[1]).unwrap().contains("ORIGINAL"));
    assert!(form_text(&out).contains("PAID"));
    assert_ne!(raster(&input), raster(&out));
}
#[test]
fn overlay_geometry_references_are_remapped_and_group_preserved() {
    use lopdf::{Object, dictionary};
    let mut overlay = lopdf::Document::load_mem(&fixture(&["STAMP"])).unwrap();
    let x = overlay.add_object(Object::Integer(180));
    let y = overlay.add_object(Object::Integer(110));
    let page = overlay.get_pages()[&1];
    let p = overlay.get_dictionary_mut(page).unwrap();
    p.set(
        "CropBox",
        vec![
            0.into(),
            0.into(),
            Object::Reference(x),
            Object::Reference(y),
        ],
    );
    p.set(
        "Group",
        dictionary! {"S"=>"Transparency","I"=>true,"K"=>true},
    );
    let l = PdfLimits::default();
    let mut out = Vec::new();
    stamp(
        &fixture(&["TARGET"]),
        &bytes(overlay),
        &PageSelection::new(vec![1], &l).unwrap(),
        &StampPlacement::new([1., 0., 0., 1., 0., 0.]).unwrap(),
        &l,
        &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        },
    )
    .unwrap();
    let d = reopen(&out, 1);
    let form = d
        .objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .find(|s| {
            matches!(
                s.dict.get(b"Subtype").and_then(Object::as_name),
                Ok(b"Form")
            )
        })
        .unwrap();
    let (_, bbox) = d.dereference(form.dict.get(b"BBox").unwrap()).unwrap();
    let coords: Vec<_> = bbox
        .as_array()
        .unwrap()
        .iter()
        .map(|o| d.dereference(o).unwrap().1.as_i64().unwrap())
        .collect();
    assert_eq!(coords, [0, 0, 180, 110]);
    let (_, group) = d.dereference(form.dict.get(b"Group").unwrap()).unwrap();
    assert!(
        group
            .as_dict()
            .unwrap()
            .get(b"I")
            .unwrap()
            .as_bool()
            .unwrap()
    );
    assert!(!raster(&out).is_empty());
}
#[test]
fn stamp_keeps_unselected_shared_resources_and_page_rendering() {
    use lopdf::Object;
    let mut target = lopdf::Document::load_mem(&fixture(&["FIRST", "SECOND"])).unwrap();
    let first = target.get_pages()[&1];
    target.get_dictionary_mut(first).unwrap().set(
        "CropBox",
        vec![10.into(), 10.into(), 190.into(), 110.into()],
    );
    target.get_dictionary_mut(first).unwrap().set("Rotate", 90);
    let input = bytes(target);
    let mut overlay = lopdf::Document::load_mem(&fixture(&["COURIER"])).unwrap();
    for o in overlay.objects.values_mut() {
        if let Object::Dictionary(d) = o
            && d.has(b"BaseFont")
        {
            d.set("BaseFont", "Courier");
        }
    }
    let l = PdfLimits::default();
    let mut out = Vec::new();
    stamp(
        &input,
        &bytes(overlay),
        &PageSelection::new(vec![1], &l).unwrap(),
        &StampPlacement::new([1., 0., 0., 1., 0., -35.]).unwrap(),
        &l,
        &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        },
    )
    .unwrap();
    let d = reopen(&out, 2);
    assert!(form_text(&out).contains("COURIER"));
    assert!(d.extract_text(&[1]).unwrap().contains("FIRST"));
    assert_eq!(raster_page(&input, 1), raster_page(&out, 1));
    assert_ne!(raster(&input), raster(&out));
    let p = d.get_dictionary(d.get_pages()[&1]).unwrap();
    assert_eq!(p.get(b"Rotate").unwrap().as_i64().unwrap(), 90);
}
