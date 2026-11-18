// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
//! Barcodes and QR codes: the encoders against worked examples of their standards, print-accurate
//! sizing, and the template surface (the `barcode` widget and the code address of an `<img>`).
//! Whether a printed code reads is checked on the rendered page (erp-report-typst `tests/scan.rs`).
use erp_expr::EvalContext;
use erp_report::{
    Code, DocTree, DocumentOptions, EvalOptions, FieldValue, Language, LimitKind, Limits,
    ModuleGrid, Node, PageSetup, QrLevel, RecordStore, ReportError, ReportLimits, ReportProgram,
    Symbology, TemplateSource, render_html,
};
use std::sync::Arc;

fn encode(symbology: Symbology, value: &str) -> Result<Code, ReportError> {
    Code::encode(symbology, value, QrLevel::Low, &Limits::default())
}
/// The modules of a linear code as `1` and `0`, without the quiet zone.
fn row(code: &Code) -> String {
    let quiet = if code.symbology() == Symbology::Ean13 {
        18
    } else {
        20
    };
    (0..code.modules_across() - quiet)
        .map(|x| if code.is_dark(x, 0) { '1' } else { '0' })
        .collect()
}
fn refusal(symbology: Symbology, value: &str) -> String {
    match encode(symbology, value).unwrap_err() {
        e @ ReportError::Code { .. } => e.to_string(),
        other => panic!("{other:?}"),
    }
}

/// for a value given without it and verified for one given with it; the standards' worked numbers)
#[test]
fn gtin_check_digits_are_computed_and_verified() {
    for (symbology, body, full) in [
        (Symbology::Ean13, "400638133393", "4006381333931"),
        (Symbology::Ean8, "9638507", "96385074"),
        (Symbology::UpcA, "03600029145", "036000291452"),
        (Symbology::Itf14, "1540014128876", "15400141288763"),
    ] {
        assert_eq!(encode(symbology, body).unwrap().text(), full);
        assert_eq!(encode(symbology, full).unwrap().text(), full);
        // The same number with another last digit is refused, not printed and not re-typed.
        let last = full.as_bytes()[full.len() - 1];
        let wrong = format!(
            "{}{}",
            &full[..full.len() - 1],
            char::from(if last == b'9' { b'0' } else { last + 1 })
        );
        let message = refusal(symbology, &wrong);
        assert!(
            message.starts_with(&format!(
                "report {symbology} code: the check digit of `{wrong}` is"
            )),
            "{message}"
        );
    }
}

#[test]
fn an_ean_asked_for_by_name_with_an_invalid_number_is_refused() {
    assert_eq!(
        refusal(Symbology::Ean13, "4006381333932"),
        "report EAN13 code: the check digit of `4006381333932` is 2; its other digits give 1"
    );
    assert_eq!(
        refusal(Symbology::Ean13, "12345"),
        "report EAN13 code: it takes 12 digits (the check digit is computed) or 13 digits with it; `12345` has 5"
    );
    assert_eq!(
        refusal(Symbology::Ean8, "96A85074"),
        "report EAN8 code: it prints digits only; `96A85074` has `A`"
    );
}

/// and UPC-A is the scan check)
#[test]
fn ean13_and_upca_modules_follow_the_standard_tables() {
    // 95 modules: guard 101, six left digits, centre 01010, six right digits, guard 101.
    let ean = row(&encode(Symbology::Ean13, "4006381333931").unwrap());
    assert_eq!(ean.len(), 95);
    assert_eq!(&ean[..3], "101");
    // First digit 4 selects the sets L G L L G G for 0 0 6 3 8 1.
    assert_eq!(&ean[3..10], "0001101"); // 0 in set L
    assert_eq!(&ean[10..17], "0100111"); // 0 in set G
    assert_eq!(&ean[17..24], "0101111"); // 6 in set L
    assert_eq!(&ean[45..50], "01010");
    assert_eq!(&ean[50..57], "1000010"); // 3 in set R
    assert_eq!(&ean[92..], "101");
    // A 13-digit number that starts with 0 is an EAN-13 when asked for by name.
    let zero = encode(Symbology::Ean13, "0036000291452").unwrap();
    assert_eq!(zero.text(), "0036000291452");
    // UPC-A is the same bars as that EAN-13, with its own 9-module quiet zones and 12 digits.
    let upc = encode(Symbology::UpcA, "036000291452").unwrap();
    assert_eq!(upc.symbology(), Symbology::UpcA);
    assert_eq!(upc.text(), "036000291452");
    assert_eq!(upc.modules_across(), 95 + 18);
    assert_eq!(row(&upc)[..93], row(&zero)[..93]);
}

#[test]
fn auto_is_ean_for_valid_digits_and_code128_otherwise() {
    for (value, symbology) in [
        ("4006381333931", Symbology::Ean13),
        ("96385074", Symbology::Ean8),
        ("ABC-123", Symbology::Code128),
        ("400638133393", Symbology::Code128),
        ("96385075", Symbology::Code128),
        ("0036000291452", Symbology::Code128),
    ] {
        assert_eq!(Symbology::auto(value), symbology, "{value}");
    }
    // The leading-zero number is a valid EAN-13: `auto` passes it by, the name prints it.
    assert!(encode(Symbology::Ean13, "0036000291452").is_ok());
}

#[test]
fn a_value_the_symbology_cannot_carry_is_refused_by_name() {
    assert_eq!(
        refusal(Symbology::Code128, "Größe"),
        "report Code128 code: it prints ASCII characters; `Größe` has `ö`"
    );
    assert_eq!(
        refusal(Symbology::Code39, "lot-1"),
        "report Code39 code: it prints digits, capital letters, space and `-.$/+%`; `lot-1` has `l`"
    );
    assert_eq!(
        refusal(Symbology::Code39, "A*B"),
        "report Code39 code: it prints digits, capital letters, space and `-.$/+%`; `A*B` has `*`"
    );
    assert_eq!(
        refusal(Symbology::Interleaved2of5, "123"),
        "report I2of5 code: it prints digits in pairs; `123` has 3 digits"
    );
    assert_eq!(
        refusal(Symbology::Qr, ""),
        "report QR code: there is no value to encode"
    );
}

#[test]
fn linear_codes_have_the_module_counts_of_their_standards() {
    // Code 128: 11 modules a symbol (start, 7 characters, check), a 13-module stop, 10 quiet each side.
    assert_eq!(
        encode(Symbology::Code128, "PJJ123C")
            .unwrap()
            .modules_across(),
        9 * 11 + 13 + 20
    );
    // Digit pairs share a symbol in code set C: start, 5 pairs, check.
    assert_eq!(
        encode(Symbology::Code128, "0123456789")
            .unwrap()
            .modules_across(),
        7 * 11 + 13 + 20
    );
    // Code 39: 15 modules a character with `*` at both ends and a 1-module gap between characters.
    assert_eq!(
        encode(Symbology::Code39, "A1").unwrap().modules_across(),
        4 * 15 + 3 + 20
    );
    assert_eq!(
        encode(Symbology::Ean8, "96385074")
            .unwrap()
            .modules_across(),
        67 + 14
    );
    // ITF-14: start 4, seven pairs of 18, stop 5 (wide elements of 3 modules).
    let itf = encode(Symbology::Itf14, "15400141288763").unwrap();
    assert_eq!(itf.modules_across(), 4 + 7 * 18 + 5 + 20);
    assert_eq!(&row(&itf)[..4], "1010");
    assert!(row(&itf).ends_with("11101"));
    for code in [
        encode(Symbology::Code128, "WH/OUT/00012").unwrap(),
        encode(Symbology::Code39, "LOT-7").unwrap(),
        itf,
    ] {
        assert_eq!(code.rows(), 1);
        let bars = row(&code);
        assert!(bars.starts_with('1') && bars.ends_with('1'), "{bars}");
    }
}

#[test]
fn qr_codes_have_finders_a_border_and_the_chosen_level() {
    let code = encode(Symbology::Qr, "HELLO").unwrap();
    // Version 1 is 21 modules a side; the border adds 4 on every side.
    assert_eq!((code.rows(), code.modules_across()), (21, 29));
    assert_eq!(code.clone().without_quiet_zone().modules_across(), 21);
    // The three finder patterns: a dark 7 × 7 ring around a light ring around a dark 3 × 3 core.
    for (left, top) in [(0, 0), (14, 0), (0, 14)] {
        for y in 0..7 {
            for x in 0..7 {
                let ring = x.max(y).max(6 - x).max(6 - y) - 3; // 0 core edge .. 3 outer ring
                let dark = ring != 2 && ring <= 3;
                assert_eq!(
                    code.is_dark(left + x, top + y),
                    dark,
                    "({x}, {y}) of ({left}, {top})"
                );
            }
        }
    }
    // A higher level needs more modules for the same content, and is never raised or lowered.
    let text = "https://erpforall.example/web#id=42&model=stock.picking";
    let side = |level| {
        Code::encode(Symbology::Qr, text, level, &Limits::default())
            .unwrap()
            .rows()
    };
    assert!(side(QrLevel::Low) < side(QrLevel::High));
    assert!(side(QrLevel::Medium) <= side(QrLevel::Quartile));
}

#[test]
fn payload_and_capacity_limits_are_refused_by_name() {
    let limits = Limits::default();
    let long = "A".repeat(2_954);
    let e = Code::encode(Symbology::Qr, &long, QrLevel::Low, &limits).unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::CodeBytes,
            actual: 2_954,
            max: 2_953
        }
    ));
    assert_eq!(
        e.to_string(),
        "report limit code_bytes exceeded: 2954 > 2953"
    );
    // Within the byte limit but past what level H holds.
    let e = Code::encode(Symbology::Qr, &"A".repeat(2_000), QrLevel::High, &limits).unwrap_err();
    assert_eq!(
        e.to_string(),
        "report QR code: 2000 bytes do not fit the largest QR code at error correction level H"
    );
    // A host lowers the payload limit.
    let small = limits.lowered(LimitKind::CodeBytes, 8).unwrap();
    let e = Code::encode(Symbology::Code128, "123456789", QrLevel::Low, &small).unwrap_err();
    assert_eq!(e.to_string(), "report limit code_bytes exceeded: 9 > 8");
    assert!(Code::encode(Symbology::Code128, "12345678", QrLevel::Low, &small).is_ok());
}

/// A module is a whole number of printer dots, every bar a whole number of modules, and the code
/// is the largest that fits its box: never wider, never stretched.
#[test]
fn a_code_is_fitted_in_whole_dots_and_whole_modules() {
    let code = encode(Symbology::Ean13, "4006381333931").unwrap();
    assert_eq!(code.modules_across(), 113);
    for (dpi, width) in [(300, 225.0), (203, 225.0), (600, 160.0), (300, 54.24)] {
        let grid = ModuleGrid::new(dpi).unwrap();
        let fitted = code.fit(width, 37.5, grid).unwrap();
        let dot = 72.0 / f64::from(dpi);
        let dots = fitted.module() / dot;
        assert!((dots - dots.round()).abs() < 1e-9, "{dpi} dpi: {dots} dots");
        assert!(fitted.width() <= width + 1e-9);
        // One more dot a module would not fit.
        assert!(113.0 * (fitted.module() + dot) > width, "{dpi} dpi");
        assert!((fitted.width() - 113.0 * fitted.module()).abs() < 1e-9);
        assert!((fitted.height() - 37.5).abs() < 1e-9);
        let vector = fitted.vector();
        // 30 bars; every edge is a whole module, and one scale maps modules to points both ways.
        assert_eq!(vector.rectangle_list().len(), 30);
        for [x, y, w, h] in vector.rectangle_list() {
            assert_eq!((x.fract(), w.fract(), *y), (0.0, 0.0, 0.0));
            assert_eq!(*h, vector.height());
        }
        let scale = fitted.width() / vector.width();
        assert!((fitted.height() / vector.height() - scale).abs() < 1e-9);
        // The quiet zone is inside the vector: 11 modules before the first bar, 7 after the last.
        let first = vector.rectangle_list().first().unwrap();
        let last = vector.rectangle_list().last().unwrap();
        assert_eq!(first[0], 11.0);
        assert_eq!(last[0] + last[2], 113.0 - 7.0);
    }
    // 300 dpi in 225 points: 8 dots a module (1.92 points); 203 dpi: 5 dots.
    let at = |dpi| {
        code.fit(225.0, 37.5, ModuleGrid::new(dpi).unwrap())
            .unwrap()
            .module()
    };
    assert!((at(300) - 8.0 * 0.24).abs() < 1e-9);
    assert!((at(203) - 5.0 * 72.0 / 203.0).abs() < 1e-9);
    // A QR code is square in the smaller side of its box.
    let qr = encode(Symbology::Qr, "HELLO").unwrap();
    let fitted = qr.fit(200.0, 96.0, ModuleGrid::default()).unwrap();
    assert_eq!(fitted.width(), fitted.height());
    assert!(fitted.width() <= 96.0 && fitted.width() > 96.0 - 29.0 * 0.24);
    assert_eq!(fitted.vector().width(), 29.0);
}

#[test]
fn a_box_too_small_for_a_readable_module_is_refused() {
    let code = encode(Symbology::Code128, "WH/OUT/00012").unwrap();
    // The shortest encoding: start in code set B, `WH/OUT/0`, a switch to set C, the pairs `00` and
    // `12`, the check symbol; then the stop and the quiet zones.
    assert_eq!(code.modules_across(), (1 + 8 + 1 + 2 + 1) * 11 + 13 + 20);
    let e = code.fit(60.0, 30.0, ModuleGrid::default()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "report Code128 code: `WH/OUT/00012` is 176 modules and needs 84.5 points at the smallest module (2 dots at 300 dpi); its box gives 60.0"
    );
    assert!(code.fit(84.48, 30.0, ModuleGrid::default()).is_ok());
    let e = code.fit(0.0, 30.0, ModuleGrid::default()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "report Code128 code: the box of a code must be in (0, 14400] points each way"
    );
    for dpi in [71, 2_401] {
        assert_eq!(
            ModuleGrid::new(dpi).unwrap_err().to_string(),
            format!(
                "report barcode code: the printer resolution must be in 72..=2400 dots per inch; got {dpi}"
            )
        );
    }
    assert_eq!(ModuleGrid::default().dpi(), 300);
}

#[test]
fn a_linear_code_too_tall_for_its_module_is_a_code_error() {
    let code = encode(Symbology::Code128, "WH/OUT/00012").unwrap();
    let e = code.fit(84.48, 7500.0, ModuleGrid::default()).unwrap_err();
    assert!(
        matches!(
            e,
            ReportError::Code {
                symbology: "Code128",
                ..
            }
        ),
        "{e:?}"
    );
    assert_eq!(
        e.to_string(),
        "report Code128 code: the box is 7500.0 points high for a module of 0.48 points; the height must be at most 14400 modules"
    );
}

// ---- the template surface

fn page() -> DocumentOptions {
    DocumentOptions::new("doc-1", "Test", PageSetup::new(595.0, 842.0, 40.0).unwrap())
}
fn store() -> Arc<RecordStore> {
    let mut store = RecordStore::new();
    store
        .insert(
            "stock.picking",
            1,
            "WH/OUT/00012",
            vec![
                ("name", FieldValue::char("WH/OUT/00012")),
                ("origin", FieldValue::char("")),
                ("barcode", FieldValue::char("4006381333931")),
            ],
        )
        .unwrap();
    Arc::new(store)
}
fn print(arch: &str) -> Result<DocTree, ReportError> {
    print_with(arch, &EvalOptions::default())
}
fn print_with(arch: &str, options: &EvalOptions) -> Result<DocTree, ReportError> {
    let language = Language::new("en_US").unwrap();
    let source = [TemplateSource::new("test.report", arch)];
    let program =
        ReportProgram::compile("test.report", &source, &language, ReportLimits::default())?;
    let store = store();
    let context = EvalContext::standard().with_var("doc", store.recordset("stock.picking", &[1]));
    program.evaluate(&context, &store, options, page())
}
/// (alt, printed width, printed height, caption, modules across) of every code of a document.
fn codes(doc: &DocTree) -> Vec<(String, f64, f64, Option<String>, f64)> {
    doc.nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Vector {
                vector,
                width,
                height,
                alt,
                caption,
            } => Some((
                alt.clone(),
                width.unwrap(),
                height.unwrap(),
                caption.clone(),
                vector.width(),
            )),
            _ => None,
        })
        .collect()
}
fn message(arch: &str) -> String {
    match print(arch).unwrap_err() {
        e @ ReportError::Evaluate { .. } => e.to_string(),
        other => panic!("{other:?}"),
    }
}

/// page is the scan check)
#[test]
fn the_widget_prints_a_code128_of_a_field_and_of_a_computed_text() {
    let doc = print(
        "<div><span t-field='doc.name' t-options=\"{'widget': 'barcode', 'width': 600, 'height': 100, \
         'img_style': 'width:300px;height:50px;'}\"/><span t-out=\"'A' + doc.name\" \
         t-options=\"{'widget': 'barcode', 'width': 1200, 'height': 200, 'img_style': \
         'width:300px;height:50px;'}\"/></div>",
    )
    .unwrap();
    let found = codes(&doc);
    assert_eq!(found.len(), 2);
    let expected = Code::encode(
        Symbology::Code128,
        "WH/OUT/00012",
        QrLevel::Low,
        &Limits::default(),
    )
    .unwrap()
    .fit(225.0, 37.5, ModuleGrid::default())
    .unwrap();
    // 300px by 50px is 225 by 37.5 points whatever the two pixel options say.
    let (alt, width, height, caption, modules) = &found[0];
    assert_eq!(alt, "Barcode WH/OUT/00012");
    assert_eq!((*width, *height), (expected.width(), 37.5));
    assert!(*width <= 225.0);
    assert_eq!((caption, *modules), (&None, 176.0));
    assert_eq!(found[1].0, "Barcode AWH/OUT/00012");
    assert_eq!(found[1].2, 37.5);
    // One character more in code set B: 11 modules more.
    assert_eq!(found[1].4, 187.0);
}

#[test]
fn an_empty_value_prints_no_code() {
    let doc = print(
        "<div><span t-field='doc.origin' t-options=\"{'widget': 'barcode'}\"/><span t-out='False' \
         t-options=\"{'widget': 'barcode'}\"/></div>",
    )
    .unwrap();
    assert!(codes(&doc).is_empty());
}

#[test]
fn widget_options_size_the_code_and_add_its_text() {
    let doc = print(
        "<div>\
         <span t-field='doc.name' t-options=\"{'widget': 'barcode'}\"/>\
         <span t-field='doc.name' t-options=\"{'widget': 'barcode', 'quiet': 0}\"/>\
         <span t-field='doc.name' t-options=\"{'widget': 'barcode', 'humanreadable': 1}\"/>\
         <span t-field='doc.name' t-options=\"{'widget': 'barcode', 'width': 400, 'height': 100, \
         'img_style': 'height:35px;'}\"/>\
         <span t-field='doc.name' t-options=\"{'widget': 'barcode', 'symbology': 'QR', 'width': 100, \
         'height': 100}\"/>\
         <span t-field='doc.name' t-options=\"{'widget': 'barcode', 'img_alt': 'Transfer'}\"/>\
         </div>",
    )
    .unwrap();
    let found = codes(&doc);
    assert_eq!(found.len(), 6);
    // Defaults: 600 by 100 pixels is a box of 450 by 75 points; the quiet zone is in the code.
    assert_eq!((found[0].2, found[0].4), (75.0, 176.0));
    assert!(found[0].1 <= 450.0 && found[0].1 > 450.0 - 176.0 * 0.24);
    assert_eq!(found[0].3, None);
    // Without the quiet zone the bars run edge to edge: 20 modules fewer in the same box.
    assert_eq!(found[1].4, 156.0);
    // The text is part of the node and comes out of the height: 75 − 12 points of bars.
    assert_eq!(found[2].3.as_deref(), Some("WH/OUT/00012"));
    assert_eq!(found[2].2, 63.0);
    // One side of the style keeps the proportion of the two options: 35px high, 140px wide.
    assert_eq!(found[3].2, 26.25);
    assert!(found[3].1 <= 105.0 && found[3].1 > 105.0 - 176.0 * 0.24);
    // A QR code of 100 by 100 pixels is a square inside 75 points.
    assert_eq!(found[4].1, found[4].2);
    assert!(found[4].1 <= 75.0);
    assert_eq!(found[5].0, "Transfer");
    // The preview shows the same codes: one generated graphic each.
    let html = render_html(&doc, &[]).unwrap();
    assert_eq!(html.matches("<svg ").count(), 6);
    assert!(html.contains(">WH/OUT/00012</span></span>"), "{html}");
}

/// printed)
#[test]
fn symbology_names_of_the_reference_and_auto_are_accepted() {
    for (name, value, modules) in [
        ("Code128", "doc.name", 176.0),
        ("auto", "doc.name", 176.0),
        ("auto", "doc.barcode", 113.0),
        ("EAN13", "doc.barcode", 113.0),
        ("EAN8", "'96385074'", 81.0),
        ("UPCA", "'036000291452'", 113.0),
        ("Standard39", "'LOT-7'", 7.0 * 15.0 + 6.0 + 20.0),
        ("I2of5", "'1234'", 4.0 + 2.0 * 18.0 + 5.0 + 20.0),
        ("ITF14", "'15400141288763'", 155.0),
        ("QR", "doc.name", 29.0),
    ] {
        let doc = print(&format!(
            "<div><span t-out=\"{value}\" t-options=\"{{'widget': 'barcode', 'symbology': '{name}'}}\"/></div>"
        ))
        .unwrap();
        assert_eq!(codes(&doc)[0].4, modules, "{name}");
    }
}

#[test]
fn what_cannot_be_printed_as_asked_is_refused_at_its_template_line() {
    let widget = |options: &str| {
        message(&format!(
            "<div><span t-field='doc.name' t-options=\"{{'widget': 'barcode', {options}}}\"/></div>"
        ))
    };
    assert_eq!(
        widget("'symbology': 'Aztec'"),
        "test.report:1: barcode code: symbology `Aztec` is unknown; the symbologies are auto, Code128, EAN13, EAN8, UPCA, Code39, ITF14, I2of5, QR"
    );
    assert_eq!(
        widget("'symbology': 'ECC200DataMatrix'"),
        "test.report:1: barcode code: symbology `ECC200DataMatrix` is not printed; the symbologies are auto, Code128, EAN13, EAN8, UPCA, Code39, ITF14, I2of5, QR"
    );
    assert_eq!(
        widget("'symbology': 'EAN13'"),
        "test.report:1: EAN13 code: it prints digits only; `WH/OUT/00012` has `W`"
    );
    assert_eq!(
        widget("'width': '600'"),
        "test.report:1: widget option `width` must be a whole number"
    );
    assert_eq!(
        widget("'width': 0"),
        "test.report:1: widget option `width` must be a number of pixels in 1..=10000"
    );
    assert_eq!(
        widget("'width': 10001, 'height': 1"),
        "test.report:1: widget option `width` must be a number of pixels in 1..=10000"
    );
    assert_eq!(
        widget("'width': 1201, 'height': 1000"),
        "test.report:1: widget options `width` and `height` of a code multiply to at most 1200000 pixels"
    );
    assert_eq!(
        widget("'symbology': 'QR', 'barLevel': 'X'"),
        "test.report:1: widget option `barLevel` must be L, M, Q or H; got `X`"
    );
    assert_eq!(
        widget("'symbology': 'QR', 'mask': 'ch_cross'"),
        "test.report:1: widget option `mask` (a picture over a QR code) is not printed"
    );
    assert_eq!(
        widget("'img_style': 'width:100%;height:35px'"),
        "test.report:1: the width of a code must be a length in px, pt, mm, cm or in; got `100%`"
    );
    assert_eq!(
        widget("'img_style': 'margin:4px'"),
        "test.report:1: the img_style of a code takes width and height; got `margin`"
    );
    assert_eq!(
        widget("'img_style': 'width:40px;height:50px'"),
        "test.report:1: Code128 code: `WH/OUT/00012` is 176 modules and needs 84.5 points at the smallest module (2 dots at 300 dpi); its box gives 30.0"
    );
    assert_eq!(
        widget("'humanreadable': 1, 'img_style': 'width:300px;height:20px'"),
        "test.report:1: Code128 code: its box is 15.0 points high and the text under the code takes 12.0 of them"
    );
    assert_eq!(
        widget("'dpi': 50"),
        "test.report:1: barcode code: the printer resolution must be in 72..=2400 dots per inch; got 50"
    );
    assert_eq!(
        widget("'dpi': -5"),
        "test.report:1: barcode code: the printer resolution must be in 72..=2400 dots per inch; got -5"
    );
    // A value that is not text: the non-ASCII case of the reference prints plain text instead.
    assert_eq!(
        message("<div><span t-out=\"'Größe'\" t-options=\"{'widget': 'barcode'}\"/></div>"),
        "test.report:1: Code128 code: it prints ASCII characters; `Größe` has `ö`"
    );
    assert_eq!(
        message("<div><span t-out='1.5' t-options=\"{'widget': 'barcode'}\"/></div>"),
        "test.report:1: the barcode widget prints a text or a whole number, got a value of type float"
    );
}

#[test]
fn the_qr_level_and_the_printer_resolution_are_options() {
    let side = |options: &str| {
        let doc = print(&format!(
            "<div><span t-out=\"'https://erpforall.example/web#id=42&amp;model=stock.picking'\" \
             t-options=\"{{'widget': 'barcode', 'symbology': 'QR', 'width': 400, 'height': 400{options}}}\"/></div>"
        ))
        .unwrap();
        codes(&doc)[0].4
    };
    assert!(side("") < side(", 'barLevel': 'H'"));
    assert_eq!(side(""), side(", 'barLevel': 'L'"));
    // The module follows the printer: whole dots of 203 dpi from the option, or from the caller.
    let module = |doc: &DocTree| {
        let (_, width, _, _, modules) = codes(doc).remove(0);
        width / modules
    };
    let arch = |options: &str| {
        format!(
            "<div><span t-field='doc.name' t-options=\"{{'widget': 'barcode', 'img_style': 'width:300px;height:50px'{options}}}\"/></div>"
        )
    };
    let by_option = module(&print(&arch(", 'dpi': 203")).unwrap());
    let by_caller = module(
        &print_with(
            &arch(""),
            &EvalOptions::default().with_code_grid(ModuleGrid::new(203).unwrap()),
        )
        .unwrap(),
    );
    assert!((by_option - 3.0 * 72.0 / 203.0).abs() < 1e-9, "{by_option}");
    assert_eq!(by_option, by_caller);
    assert!((module(&print(&arch("")).unwrap()) - 5.0 * 0.24).abs() < 1e-9);
}

/// place; nothing is fetched)
#[test]
fn an_image_whose_source_is_a_code_address_prints_that_code() {
    let doc = print(
        "<div>\
         <img t-att-src=\"'/report/barcode/?barcode_type=QR&amp;value=' + 'BCD%0A002+x' + '&amp;width=128&amp;height=128&amp;quiet=0'\"/>\
         <img src='/report/barcode/Code128/WH%2FOUT%2F00012?humanreadable=1' style='width:300px;height:50px' alt='Transfer'/>\
         <img t-att-src=\"'/report/barcode/EAN13/' + doc.barcode\" width='200' height='60'/>\
         </div>",
    )
    .unwrap();
    let found = codes(&doc);
    assert_eq!(found.len(), 3);
    // `%0A` is a line feed and `+` a space in a query: 11 bytes fit version 1, without a border.
    assert_eq!(found[0].0, "Barcode BCD\n002 x");
    assert_eq!(found[0].4, 21.0);
    assert!(found[0].1 <= 96.0 && found[0].1 == found[0].2);
    assert_eq!(found[1].0, "Transfer");
    assert_eq!(found[1].3.as_deref(), Some("WH/OUT/00012"));
    assert_eq!(found[1].2, 37.5 - 12.0);
    // Bare numbers on the element are pixels.
    assert_eq!((found[2].2, found[2].4), (45.0, 113.0));
    assert!(found[2].1 <= 150.0);
    for (source, text) in [
        (
            "/report/barcode/",
            "test.report:1: barcode code: `/report/barcode/` is not the address of a code",
        ),
        (
            "/report/barcode/QR/x?width=0",
            "test.report:1: barcode code: parameter `width` must be a whole number in 1..=10000; got `0`",
        ),
        (
            "/report/barcode/QR/x?mask=ch_cross",
            "test.report:1: barcode code: parameter `mask` of a code address is not printed",
        ),
        (
            "/report/barcode/EAN13/123",
            "test.report:1: EAN13 code: it takes 12 digits (the check digit is computed) or 13 digits with it; `123` has 3",
        ),
    ] {
        assert_eq!(message(&format!("<div><img src='{source}'/></div>")), text);
    }
}
