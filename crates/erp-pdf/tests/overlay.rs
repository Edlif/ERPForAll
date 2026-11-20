// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use erp_pdf::{PageSelection, PdfError, PdfErrorKind, PdfLimits, StampPlacement, stamp};
use lopdf::Object;
use support::*;

const IDENTITY: [f32; 6] = [1., 0., 0., 1., 0., 0.];
const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];
/// A red mark near the lower-left corner and a blue one near the upper-right corner of a
/// 200 by 120 page: no turn maps the pair onto itself.
const MARKS: &str = "1 0 0 rg 10 10 40 20 re f 0 0 1 rg 150 90 30 10 re f";

fn stamped(
    target: &[u8],
    overlay: &[u8],
    pages: Vec<u32>,
    placement: [f32; 6],
) -> Result<Vec<u8>, PdfError> {
    let l = PdfLimits::default();
    collect(|emit| {
        stamp(
            target,
            overlay,
            &PageSelection::new(pages, &l).unwrap(),
            &StampPlacement::new(placement).unwrap(),
            &l,
            emit,
        )
    })
}
/// Where a viewer shows the point `(x, y)` of a page with the box `[x0 y0 x1 y1]` after turning
/// the page clockwise by `turn` degrees, the turned box keeping the corner `(x0, y0)`.
fn shown(turn: i64, (x, y): (i64, i64), [x0, y0, x1, y1]: [i64; 4]) -> (i64, i64) {
    match turn {
        0 => (x, y),
        90 => (x0 + (y - y0), y0 + (x1 - x)),
        180 => (x0 + (x1 - x), y0 + (y1 - y)),
        270 => (x0 + (y1 - y), y0 + (x - x0)),
        _ => unreachable!("not a quarter turn: {turn}"),
    }
}
fn form_matrix(out: &[u8]) -> Option<Vec<f32>> {
    let d = lopdf::Document::load_mem(out).unwrap();
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
    let m = form.dict.get(b"Matrix").ok()?;
    Some(
        m.as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_float().unwrap())
            .collect(),
    )
}

#[test]
fn rotated_overlay_is_placed_as_a_viewer_shows_it() {
    for rotate in [0i64, 90, 180, 270, -90, 450] {
        let turn = rotate.rem_euclid(360);
        let mut overlay = drawing([0, 0, 200, 120], &[MARKS]);
        page_mut(&mut overlay, 1).set("Rotate", rotate);
        let overlay = bytes(overlay);
        let (w, h) = if turn % 180 == 0 {
            (200, 120)
        } else {
            (120, 200)
        };
        let target = bytes(drawing([0, 0, w, h], &[""]));
        let out = stamped(&target, &overlay, vec![1], IDENTITY).unwrap();
        reopen(&out, 1);
        let got = pixels(&out, 0);
        // The renderer turns the overlay page by itself: stamped on a blank page of the shown
        // size, the overlay must give the same picture.
        assert_eq!(got, pixels(&overlay, 0), "Rotate {rotate}");
        let page = [0, 0, 200, 120];
        assert_eq!(colour(&got, shown(turn, (30, 20), page)), RED, "{rotate}");
        assert_eq!(colour(&got, shown(turn, (165, 95), page)), BLUE, "{rotate}");
        // The target page itself is not turned.
        let d = lopdf::Document::load_mem(&out).unwrap();
        assert!(!d.get_dictionary(d.get_pages()[&1]).unwrap().has(b"Rotate"));
    }
}

#[test]
fn form_matrix_is_written_only_for_a_turn_or_a_unit() {
    let target = bytes(drawing([0, 0, 300, 300], &[""]));
    let turned = |rotate: i64| {
        let mut overlay = drawing([0, 0, 200, 120], &[MARKS]);
        page_mut(&mut overlay, 1).set(
            "CropBox",
            vec![20.into(), 10.into(), 180.into(), 110.into()],
        );
        page_mut(&mut overlay, 1).set("Rotate", rotate);
        form_matrix(&stamped(&target, &bytes(overlay), vec![1], IDENTITY).unwrap())
    };
    assert_eq!(turned(0), None);
    assert_eq!(turned(360), None);
    assert_eq!(turned(90), Some(vec![0., -1., 1., 0., 10., 190.]));
    assert_eq!(turned(180), Some(vec![-1., 0., 0., -1., 200., 120.]));
    assert_eq!(turned(270), Some(vec![0., 1., -1., 0., 130., -10.]));
}

#[test]
fn turned_overlay_keeps_the_lower_left_corner_of_its_crop_box() {
    // Red inside the crop box, blue to the right of it.
    let marks = "1 0 0 rg 30 20 40 20 re f 0 0 1 rg 184 50 12 10 re f";
    let crop = [20, 10, 180, 110];
    for rotate in [0i64, 90, 180, 270, -90, 450, 360] {
        let turn = rotate.rem_euclid(360);
        let mut overlay = drawing([0, 0, 200, 120], &[marks]);
        let p = page_mut(&mut overlay, 1);
        p.set(
            "CropBox",
            crop.iter().map(|v| (*v).into()).collect::<Vec<Object>>(),
        );
        p.set("Rotate", rotate);
        let overlay = bytes(overlay);
        let target = bytes(drawing([0, 0, 300, 300], &[""]));
        let out = stamped(&target, &overlay, vec![1], IDENTITY).unwrap();
        // Without a turn the form has no matrix, as before; the target page is never turned.
        assert_eq!(form_matrix(&out).is_none(), turn == 0, "Rotate {rotate}");
        let d = reopen(&out, 1);
        assert!(!d.get_dictionary(d.get_pages()[&1]).unwrap().has(b"Rotate"));
        let got = pixels(&out, 0);
        assert_eq!(colour(&got, shown(turn, (50, 30), crop)), RED, "{turn}");
        // The form clips to the crop box: the blue mark is not painted where it would land.
        assert_eq!(colour(&got, shown(turn, (190, 55), crop)), CLEAR);
        assert!(!got.2.chunks(4).any(|p| p == BLUE), "turn {turn}");
        // Moved to the origin on a page of the shown size, it is the page as the renderer
        // shows it, crop box included.
        let (w, h) = if turn % 180 == 0 {
            (160, 100)
        } else {
            (100, 160)
        };
        let blank = bytes(drawing([0, 0, w, h], &[""]));
        let moved = stamped(&blank, &overlay, vec![1], [1., 0., 0., 1., -20., -10.]).unwrap();
        assert_eq!(pixels(&moved, 0), pixels(&overlay, 0), "turn {turn}");
    }
}

#[test]
fn user_unit_keeps_the_physical_size_of_the_overlay() {
    let mark = "1 0 0 rg 10 10 40 20 re f";
    let overlay = |unit: Option<Object>, rotate: i64| {
        let mut d = drawing([0, 0, 100, 60], &[mark]);
        if let Some(unit) = unit {
            page_mut(&mut d, 1).set("UserUnit", unit);
        }
        page_mut(&mut d, 1).set("Rotate", rotate);
        bytes(d)
    };
    let target = |unit: Option<Object>| {
        let mut d = drawing([0, 0, 300, 300], &["", ""]);
        if let Some(unit) = unit {
            page_mut(&mut d, 2).set("UserUnit", unit);
        }
        bytes(d)
    };
    let plain = overlay(None, 0);
    // Overlay unit 2 on a page in default units: twice the size, the same as a placement of 2.
    let out = stamped(
        &target(None),
        &overlay(Some(2.into()), 0),
        vec![1],
        IDENTITY,
    )
    .unwrap();
    let scaled = stamped(&target(None), &plain, vec![1], [2., 0., 0., 2., 0., 0.]).unwrap();
    assert_eq!(pixels(&out, 0), pixels(&scaled, 0));
    assert_eq!(colour(&pixels(&out, 0), (90, 50)), RED);
    assert_eq!(colour(&pixels(&out, 0), (15, 15)), CLEAR);
    // A real unit, and a turn together with a unit.
    let out = stamped(
        &target(None),
        &overlay(Some(Object::Real(1.5)), 90),
        vec![1],
        IDENTITY,
    )
    .unwrap();
    let scaled = stamped(
        &target(None),
        &overlay(None, 90),
        vec![1],
        [1.5, 0., 0., 1.5, 0., 0.],
    )
    .unwrap();
    assert_eq!(pixels(&out, 0), pixels(&scaled, 0));
    assert_eq!(form_matrix(&out), Some(vec![0., -1.5, 1.5, 0., 0., 150.]));
    // Target unit 2 on page 2: half the size there, page 1 in default units keeps the size. The
    // renderer draws one pixel per unit of either page.
    let out = stamped(
        &target(Some(2.into())),
        &plain,
        vec![1, 2],
        [1., 0., 0., 1., 100., 50.],
    )
    .unwrap();
    reopen(&out, 2);
    assert_eq!(colour(&pixels(&out, 0), (130, 70)), RED);
    let second = pixels(&out, 1);
    assert_eq!(colour(&second, (115, 60)), RED); // 100 + [5, 25], 50 + [5, 15]
    assert_eq!(colour(&second, (130, 70)), CLEAR);
    let halved = stamped(
        &target(None),
        &plain,
        vec![2],
        [0.5, 0., 0., 0.5, 100., 50.],
    )
    .unwrap();
    assert_eq!(second, pixels(&halved, 1));
    // The same unit on both sides: the size of a stamp between pages in default units.
    let out = stamped(
        &target(Some(3.into())),
        &overlay(Some(3.into()), 0),
        vec![2],
        IDENTITY,
    )
    .unwrap();
    let got = pixels(&out, 1);
    assert_eq!(colour(&got, (30, 20)), RED);
    assert_eq!(colour(&got, (60, 20)), CLEAR);
    assert_eq!(colour(&got, (30, 40)), CLEAR);
    // A page that was not selected keeps its picture.
    assert_eq!(pixels(&out, 0), pixels(&target(None), 0));
}

#[test]
fn user_unit_must_be_a_positive_number() {
    for bad in [
        Object::Integer(0),
        Object::Real(-1.5),
        Object::string_literal("2"),
    ] {
        let with_unit = || {
            let mut d = drawing([0, 0, 100, 60], &["0 0 10 10 re f"]);
            page_mut(&mut d, 1).set("UserUnit", bad.clone());
            bytes(d)
        };
        let plain = bytes(drawing([0, 0, 100, 60], &["0 0 10 10 re f"]));
        for (target, overlay) in [(plain.clone(), with_unit()), (with_unit(), plain)] {
            let e = stamped(&target, &overlay, vec![1], IDENTITY).unwrap_err();
            assert_eq!(e.kind(), PdfErrorKind::Malformed);
            assert_eq!(e.to_string(), "PDF UserUnit must be a positive number");
        }
    }
}

#[test]
fn inherited_geometry_and_indirect_units_compose_before_placement() {
    let mut overlay = drawing([0, 0, 200, 120], &["1 0 0 rg 30 20 40 20 re f"]);
    let page = overlay.get_pages()[&1];
    let parent = overlay
        .get_dictionary(page)
        .unwrap()
        .get(b"Parent")
        .unwrap()
        .as_reference()
        .unwrap();
    let turn = overlay.add_object(90);
    let crop = overlay.add_object(vec![20.into(), 10.into(), 180.into(), 110.into()]);
    let unit = overlay.add_object(Object::Real(1.5));
    let inherited = overlay.get_dictionary_mut(parent).unwrap();
    inherited.set("Rotate", turn);
    inherited.set("CropBox", crop);
    page_mut(&mut overlay, 1).set("UserUnit", unit);
    let mut target = drawing([0, 0, 300, 300], &[""]);
    let unit = target.add_object(3);
    page_mut(&mut target, 1).set("UserUnit", unit);
    let target = bytes(target);
    let placement = [1., 0., 0., 1., 40., 30.];
    let out = stamped(&target, &bytes(overlay.clone()), vec![1], placement).unwrap();
    // (50,30) turns to (40,140), then scales by 1.5/3 and translates.
    assert_eq!(colour(&pixels(&out, 0), (60, 100)), RED);
    assert_eq!(colour(&pixels(&out, 0), (65, 45)), CLEAR);
    // Leaf entries override both inherited values.
    let p = page_mut(&mut overlay, 1);
    p.set("Rotate", 0);
    p.set("CropBox", vec![0.into(), 0.into(), 100.into(), 60.into()]);
    let out = stamped(&target, &bytes(overlay), vec![1], placement).unwrap();
    assert_eq!(colour(&pixels(&out, 0), (65, 45)), RED);
    assert_eq!(colour(&pixels(&out, 0), (60, 100)), CLEAR);
}

#[test]
fn target_crop_and_rotation_do_not_change_placement_coordinates() {
    let overlay = bytes(drawing([0, 0, 100, 60], &["1 0 0 rg 10 10 40 20 re f"]));
    let mut target = drawing([0, 0, 200, 120], &["", ""]);
    let p = page_mut(&mut target, 1);
    p.set("Rotate", 90);
    p.set(
        "CropBox",
        vec![20.into(), 10.into(), 180.into(), 110.into()],
    );
    let original = bytes(target.clone());
    let out = stamped(&original, &overlay, vec![1], [1., 0., 0., 1., 30., 20.]).unwrap();
    // Independent drawing on the same target page: shifted rectangle [40,30,40,20].
    let stream = target.add_object(lopdf::Stream::new(
        lopdf::Dictionary::new(),
        b"1 0 0 rg 40 30 40 20 re f".to_vec(),
    ));
    page_mut(&mut target, 1).set("Contents", stream);
    assert_eq!(pixels(&out, 0), pixels(&bytes(target), 0));
    assert_eq!(pixels(&out, 1), pixels(&original, 1));
    let mut reopened = reopen(&out, 2);
    let p = page_mut(&mut reopened, 1);
    assert_eq!(p.get(b"Rotate").unwrap().as_i64().unwrap(), 90);
}

#[test]
fn invalid_overlay_rotation_names_the_problem() {
    for (value, message) in [
        (Object::Integer(45), "PDF Rotate must be a multiple of 90"),
        (
            Object::string_literal("90"),
            "PDF Rotate must be an integer",
        ),
    ] {
        let mut overlay = drawing([0, 0, 100, 60], &[""]);
        page_mut(&mut overlay, 1).set("Rotate", value);
        let target = bytes(drawing([0, 0, 100, 60], &[""]));
        let e = stamped(&target, &bytes(overlay), vec![1], IDENTITY).unwrap_err();
        assert_eq!(e.kind(), PdfErrorKind::Malformed);
        assert_eq!(e.to_string(), message);
    }
}

#[test]
fn target_unit_scaling_refuses_unrepresentable_operands() {
    let overlay = bytes(drawing([0, 0, 100, 60], &[""]));
    for (unit, scale) in [(1e-30, f32::MAX), (1e10, 1e-40)] {
        let mut target = drawing([0, 0, 100, 60], &[""]);
        page_mut(&mut target, 1).set("UserUnit", Object::Real(unit));
        let e = stamped(
            &bytes(target),
            &overlay,
            vec![1],
            [scale, 0., 0., scale, 0., 0.],
        )
        .unwrap_err();
        assert_eq!(e.kind(), PdfErrorKind::Malformed);
        assert_eq!(
            e.to_string(),
            "PDF stamp placement is out of range for the page UserUnit",
            "target unit {unit}, placement scale {scale}"
        );
    }
}
