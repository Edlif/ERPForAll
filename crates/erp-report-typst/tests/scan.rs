// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! The scan check: a code that prints is a code that reads. Each code is laid out into a PDF, the
//! page is rasterised in this process and read back by a decoder that shares no code with the
//! generator (rxing, a port of ZXing). Every compared text comes out of the rendered page.
use erp_expr::EvalContext;
use erp_report::{
    Code, DocumentBuilder, DocumentOptions, EvalOptions, FieldValue, Language, LayoutEngine,
    Limits, ModuleGrid, Node, PageSetup, QrLevel, RecordStore, ReportLimits, ReportProgram,
    Symbology, TemplateSource,
};
use erp_report_typst::TypstEngine;
use rxing::BarcodeFormat;
use std::{path::Path, sync::Arc};

fn engine() -> TypstEngine {
    let fonts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts");
    TypstEngine::new(fonts, Limits::default()).unwrap()
}
fn page() -> DocumentOptions {
    DocumentOptions::new(
        "codes",
        "Codes",
        PageSetup::new(420.0, 300.0, 20.0).unwrap(),
    )
}
/// The only page of `pdf` as 8-bit grey on white paper, `scale` pixels a point.
fn raster(pdf: &[u8], scale: f32) -> (Vec<u8>, u32, u32) {
    let (grey, width, height, pages) = raster_page(pdf, 0, scale);
    assert_eq!(pages, 1, "one code on one page");
    (grey, width, height)
}
/// Page `index` of `pdf` as 8-bit grey on white paper, and the number of pages.
fn raster_page(pdf: &[u8], index: usize, scale: f32) -> (Vec<u8>, u32, u32, usize) {
    let parsed = hayro::hayro_syntax::Pdf::new(pdf.to_vec()).unwrap();
    let pages = parsed.pages().len();
    let settings = hayro::PixmapSettings {
        x_scale: scale,
        y_scale: scale,
        ..Default::default()
    };
    let pixmap = hayro::render(
        &parsed.pages()[index],
        &hayro::RenderCache::new(),
        &Default::default(),
        &Default::default(),
        &settings,
    );
    // Premultiplied colour over white.
    let grey = pixmap
        .data_as_u8_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|[red, green, blue, alpha]| {
            let ink = (u32::from(*red) + u32::from(*green) + u32::from(*blue)) / 3;
            (ink + 255 - u32::from(*alpha)).min(255) as u8
        })
        .collect();
    (grey, pixmap.width().into(), pixmap.height().into(), pages)
}
fn read(pdf: &[u8], format: BarcodeFormat) -> String {
    let (grey, width, height) = raster(pdf, 4.0);
    match rxing::helpers::detect_in_luma(grey, width, height, Some(format)) {
        Ok(found) => {
            assert_eq!(*found.getBarcodeFormat(), format);
            found.getText().to_owned()
        }
        Err(error) => panic!("no {format:?} was read from the page: {error}"),
    }
}
fn format_of(symbology: Symbology) -> BarcodeFormat {
    match symbology {
        Symbology::Code128 => BarcodeFormat::CODE_128,
        Symbology::Ean13 => BarcodeFormat::EAN_13,
        Symbology::Ean8 => BarcodeFormat::EAN_8,
        Symbology::UpcA => BarcodeFormat::UPC_A,
        Symbology::Code39 | Symbology::Extended39 => BarcodeFormat::CODE_39,
        Symbology::Itf14 | Symbology::Interleaved2of5 => BarcodeFormat::ITF,
        Symbology::Qr => BarcodeFormat::QR_CODE,
        Symbology::DataMatrix => BarcodeFormat::DATA_MATRIX,
        other => panic!("no reader format for {other}"),
    }
}
/// A page with one code of `value` in a box of `width` × `height` points, with or without the text
/// under it.
fn printed(
    symbology: Symbology,
    value: &str,
    level: QrLevel,
    (width, height): (f64, f64),
    grid: ModuleGrid,
    caption: bool,
) -> Vec<u8> {
    let limits = Limits::default();
    let code = Code::encode(symbology, value, level, &limits).unwrap();
    printed_code(&code, (width, height), grid, caption)
}
fn printed_code(
    code: &Code,
    (width, height): (f64, f64),
    grid: ModuleGrid,
    caption: bool,
) -> Vec<u8> {
    let limits = Limits::default();
    let fitted = code.fit(width, height, grid).unwrap();
    let mut b = DocumentBuilder::new(limits);
    let body = b
        .add(Node::Vector {
            width: Some(fitted.width()),
            height: Some(fitted.height()),
            vector: fitted.into_vector(),
            alt: format!("Barcode {}", code.text()),
            caption: caption.then(|| code.text().to_owned()),
        })
        .unwrap();
    let out = engine().render(&b.finish(body, page()).unwrap()).unwrap();
    assert!(out.warnings().is_empty(), "{:?}", out.warnings());
    out.bytes().to_vec()
}
fn scan(symbology: Symbology, value: &str) -> String {
    let pdf = printed(
        symbology,
        value,
        QrLevel::Low,
        (380.0, 60.0),
        ModuleGrid::default(),
        false,
    );
    read(&pdf, format_of(symbology))
}

/// in erp-report `tests/code.rs` and below)
#[test]
fn code128_reads_in_every_code_set() {
    for value in [
        "WH/OUT/00012",
        "0123456789",
        "AB1234",
        "LOT-0001",
        "A\tB\rC",
    ] {
        assert_eq!(scan(Symbology::Code128, value), value, "{value:?}");
    }
    // Every symbol value of the table: the 95 characters of code set B, then the 100 digit pairs
    // of code set C.
    let set_b: Vec<u8> = (32u8..127).collect();
    for chunk in set_b.chunks(19) {
        let value = std::str::from_utf8(chunk).unwrap();
        assert_eq!(scan(Symbology::Code128, value), value, "{value:?}");
    }
    let pairs: Vec<String> = (0..100).map(|n| format!("{n:02}")).collect();
    for chunk in pairs.chunks(20) {
        let value = chunk.concat();
        assert_eq!(scan(Symbology::Code128, &value), value);
    }
}

#[test]
fn ean_and_upc_read_with_their_check_digits() {
    for (symbology, value, read_as) in [
        (Symbology::Ean13, "400638133393", "4006381333931"),
        (Symbology::Ean13, "4006381333931", "4006381333931"),
        // Every digit in every set: first digits 1 to 9 choose the nine mixed parities.
        (Symbology::Ean13, "123456789012", "1234567890128"),
        (Symbology::Ean13, "297865403162", ""),
        (Symbology::Ean13, "380142675950", ""),
        (Symbology::Ean13, "501234987650", ""),
        (Symbology::Ean13, "690123876540", ""),
        (Symbology::Ean13, "789012765430", ""),
        (Symbology::Ean13, "878901654320", ""),
        (Symbology::Ean13, "967890543210", ""),
        // A leading zero is an EAN-13 when asked for by name.
        (Symbology::Ean13, "0036000291452", "0036000291452"),
        (Symbology::Ean8, "9638507", "96385074"),
        (Symbology::Ean8, "12345670", "12345670"),
        (Symbology::UpcA, "03600029145", "036000291452"),
        (Symbology::UpcA, "725272730706", "725272730706"),
    ] {
        let expected = if read_as.is_empty() {
            // The text of our own code: the reader must agree with the check digit we computed.
            Code::encode(symbology, value, QrLevel::Low, &Limits::default())
                .unwrap()
                .text()
                .to_owned()
        } else {
            read_as.to_owned()
        };
        let got = scan(symbology, value);
        assert_eq!(got, expected, "{symbology} {value}");
        assert!(
            got.starts_with(value) || got == value,
            "{symbology} {value}"
        );
    }
}

#[test]
fn code39_and_interleaved_read() {
    for value in [
        "LOT-7 A.B$/+%",
        "0123456789ABCDEFGHIJ",
        "KLMNOPQRSTUVWXYZ-. ",
    ] {
        // A trailing space is part of the value: compare what the reader returns unchanged.
        assert_eq!(scan(Symbology::Code39, value), value, "{value:?}");
    }
    assert_eq!(scan(Symbology::Itf14, "1540014128876"), "15400141288763");
    assert_eq!(scan(Symbology::Itf14, "15400141288763"), "15400141288763");
    // All ten digits as bars and as spaces.
    assert_eq!(scan(Symbology::Interleaved2of5, "0123456789"), "0123456789");
    assert_eq!(scan(Symbology::Interleaved2of5, "98765432"), "98765432");
}

#[test]
fn qr_codes_read_at_every_level() {
    let link = "https://erpforall.example/web#id=42&model=stock.picking";
    let long: String = (0..600)
        .map(|n| char::from(b'a' + (n % 26) as u8))
        .collect();
    for (value, level) in [
        (link, QrLevel::Low),
        (link, QrLevel::Medium),
        (link, QrLevel::Quartile),
        (link, QrLevel::High),
        (
            "BCD\n002\n1\nSCT\n\nArkana\nID00000000000000\nEUR12.50",
            QrLevel::Medium,
        ),
        (long.as_str(), QrLevel::Low),
    ] {
        let pdf = printed(
            Symbology::Qr,
            value,
            level,
            (250.0, 250.0),
            ModuleGrid::default(),
            false,
        );
        assert_eq!(read(&pdf, BarcodeFormat::QR_CODE), value, "{level:?}");
    }
}

#[test]
fn a_code_with_its_text_reads_and_shows_the_value() {
    let pdf = printed(
        Symbology::Code128,
        "LOT-0001",
        QrLevel::Low,
        (225.0, 37.5),
        ModuleGrid::default(),
        true,
    );
    assert_eq!(read(&pdf, BarcodeFormat::CODE_128), "LOT-0001");
    let parsed = lopdf::Document::load_mem(&pdf).unwrap();
    let pages: Vec<u32> = parsed.get_pages().keys().copied().collect();
    let text = parsed.extract_text(&pages).unwrap();
    assert!(text.contains("LOT-0001"), "{text:?}");
    let pdf = printed(
        Symbology::Ean13,
        "4006381333931",
        QrLevel::Low,
        (54.24, 40.0),
        ModuleGrid::default(),
        true,
    );
    // The smallest module: 2 dots at 300 dpi; the text is set smaller so it stays on one line.
    assert_eq!(read(&pdf, BarcodeFormat::EAN_13), "4006381333931");
}

/// The printed bars at the printer's own resolution: a row of pixels across the code has dark and
/// light runs that are whole multiples of the module's dots, at 300 and at 203 dots per inch.
#[test]
fn bars_are_whole_dots_at_the_printer_resolution() {
    for (dpi, dots) in [(300u32, 8usize), (203, 5)] {
        let grid = ModuleGrid::new(dpi).unwrap();
        let pdf = printed(
            Symbology::Ean13,
            "4006381333931",
            QrLevel::Low,
            (225.0, 37.5),
            grid,
            false,
        );
        let (grey, width, _) = raster(&pdf, dpi as f32 / 72.0);
        // The code starts at the top margin (20 points): a row 10 points into it.
        let y = (30.0 * f64::from(dpi) / 72.0) as usize;
        let row = &grey[y * width as usize..(y + 1) * width as usize];
        let mut runs: Vec<(bool, usize)> = Vec::new();
        for pixel in row {
            let dark = *pixel < 128;
            match runs.last_mut() {
                Some((last, count)) if *last == dark => *count += 1,
                _ => runs.push((dark, 1)),
            }
        }
        // Without the paper before and after: 30 bars and the 29 spaces between them.
        let inner = &runs[1..runs.len() - 1];
        assert_eq!(inner.len(), 59, "{dpi} dpi: {runs:?}");
        for (dark, count) in inner {
            assert_eq!(
                count % dots,
                0,
                "{dpi} dpi: a run of {count} pixels (dark: {dark})"
            );
            assert!((1..=4).contains(&(count / dots)));
        }
        let total: usize = inner.iter().map(|(_, count)| count).sum();
        assert_eq!(total, 95 * dots);
        assert_eq!(read(&pdf, BarcodeFormat::EAN_13), "4006381333931");
    }
}

/// reads), (partial: a transfer name printed by the widget in a table cell and
/// in a heading reads back; the shipped document is covered where it is installed)
#[test]
fn codes_printed_by_a_template_read_back() {
    let mut store = RecordStore::new();
    store
        .insert(
            "stock.picking",
            1,
            "WH/OUT/00012",
            vec![
                ("name", FieldValue::char("WH/OUT/00012")),
                ("barcode", FieldValue::char("4006381333931")),
            ],
        )
        .unwrap();
    let store = Arc::new(store);
    let print = |arch: &str| {
        let language = Language::new("en_US").unwrap();
        let source = [TemplateSource::new("test.report", arch)];
        let program =
            ReportProgram::compile("test.report", &source, &language, ReportLimits::default())
                .unwrap();
        let context =
            EvalContext::standard().with_var("doc", store.recordset("stock.picking", &[1]));
        let document = program
            .evaluate(&context, &store, &EvalOptions::default(), page())
            .unwrap();
        let out = engine().render(&document).unwrap();
        assert!(out.warnings().is_empty(), "{:?}", out.warnings());
        out.bytes().to_vec()
    };
    for (arch, format, value) in [
        (
            "<div><h2>Transfer</h2><div class='text-end'><span t-field='doc.name' t-options=\"{'widget': \
             'barcode', 'width': 600, 'height': 100, 'img_style': 'width:300px;height:50px;'}\"/></div></div>",
            BarcodeFormat::CODE_128,
            "WH/OUT/00012",
        ),
        (
            "<table><thead><tr><th>Product</th><th>Code</th></tr></thead><tbody><tr><td>Desk</td><td \
             class='text-center'><span t-field='doc.barcode' t-options=\"{'widget': 'barcode', 'symbology': \
             'auto', 'quiet': 0, 'humanreadable': 1, 'img_style': 'width:150px;height:60px'}\"/></td></tr>\
             </tbody></table>",
            BarcodeFormat::EAN_13,
            "4006381333931",
        ),
        (
            "<div><span t-out=\"'00' + doc.barcode\" t-options=\"{'widget': 'barcode', 'symbology': 'QR', \
             'width': 160, 'height': 160, 'barLevel': 'M'}\"/></div>",
            BarcodeFormat::QR_CODE,
            "004006381333931",
        ),
        (
            "<div><img src='/report/barcode/?barcode_type=QR&amp;value=BCD%0A002&amp;width=128&amp;height=128'/></div>",
            BarcodeFormat::QR_CODE,
            "BCD\n002",
        ),
    ] {
        assert_eq!(read(&print(arch), format), value, "{arch}");
    }
}

/// A code is one piece: when the rest of a page is too short for it, the whole code prints on the
/// next page and reads there; no part of it stays behind.
#[test]
fn a_code_that_does_not_fit_the_page_moves_whole_to_the_next() {
    let limits = Limits::default();
    let value = "https://erpforall.example/t/12";
    let code = Code::encode(Symbology::Qr, value, QrLevel::Medium, &limits).unwrap();
    let fitted = code.fit(200.0, 200.0, ModuleGrid::default()).unwrap();
    assert!(fitted.height() > 180.0);
    let mut b = DocumentBuilder::new(limits);
    // Eight lines of text use about a third of the 260-point page body; the code needs more than
    // what is left.
    let mut children = Vec::new();
    for line in 0..8 {
        children.push(b.add(Node::Text(format!("Line {line}"))).unwrap());
        children.push(b.add(Node::LineBreak).unwrap());
    }
    children.push(
        b.add(Node::Vector {
            width: Some(fitted.width()),
            height: Some(fitted.height()),
            vector: fitted.into_vector(),
            alt: "Barcode".into(),
            caption: None,
        })
        .unwrap(),
    );
    let body = b.add(Node::Flow { children }).unwrap();
    let out = engine().render(&b.finish(body, page()).unwrap()).unwrap();
    let (first, width, height, pages) = raster_page(out.bytes(), 0, 4.0);
    assert_eq!(pages, 2);
    assert!(
        rxing::helpers::detect_in_luma(first, width, height, Some(BarcodeFormat::QR_CODE)).is_err(),
        "no code, whole or in part, reads on the first page"
    );
    let (second, width, height, _) = raster_page(out.bytes(), 1, 4.0);
    let found = rxing::helpers::detect_in_luma(second, width, height, Some(BarcodeFormat::QR_CODE))
        .unwrap();
    assert_eq!(found.getText(), value);
}

#[test]
fn datamatrix_pdf_scans_preserve_text_and_format() {
    let large = "erpforall-label-".repeat(40);
    for value in [
        "WH/OUT/00012",
        "01234567890123456789",
        "Café",
        "東京–Arkana",
        large.as_str(),
        "[)>\u{1e}05\u{1d}LOT-7\u{1e}\u{4}",
    ] {
        let code = Code::encode(
            Symbology::DataMatrix,
            value,
            QrLevel::Low,
            &Limits::default(),
        )
        .unwrap();
        for dpi in [203, 300] {
            let pdf = printed_code(&code, (250.0, 210.0), ModuleGrid::new(dpi).unwrap(), false);
            assert_eq!(read(&pdf, BarcodeFormat::DATA_MATRIX), value);
        }
    }
}

#[test]
fn datamatrix_gs1_pdf_scans_preserve_separators_and_signal_fnc1() {
    use rxing::{RXingResultMetadataType, RXingResultMetadataValue};
    for value in [
        "010950110102091710LOT-1",
        "010950110102091721SERIAL-1",
        "0012345678901234567513260305",
        "010950110102091710LOT-1\u{1d}2110",
    ] {
        let code = Code::encode_gs1_datamatrix(value, &Limits::default()).unwrap();
        let pdf = printed_code(&code, (160.0, 80.0), ModuleGrid::default(), false);
        let (grey, width, height) = raster(&pdf, 4.0);
        let found =
            rxing::helpers::detect_in_luma(grey, width, height, Some(BarcodeFormat::DATA_MATRIX))
                .unwrap();
        assert_eq!(*found.getBarcodeFormat(), BarcodeFormat::DATA_MATRIX);
        // rxing 0.9.4 consumes the leading FNC1 into metadata, retaining interior GS bytes.
        assert_eq!(found.getText(), value);
        assert_eq!(
            found
                .getRXingResultMetadata()
                .get(&RXingResultMetadataType::SYMBOLOGY_IDENTIFIER),
            Some(&RXingResultMetadataValue::SymbologyIdentifier("]d2".into()))
        );
    }
}

#[test]
fn datamatrix_template_scans_keep_the_requested_encoding() {
    use rxing::{RXingResultMetadataType, RXingResultMetadataValue};
    for (arch, value, identifier) in [
        (
            "<div><span t-out=\"'LOT-7'\" t-options=\"{'widget': 'barcode', 'symbology': 'ECC200DataMatrix', 'humanreadable': 1, 'width': 160, 'height': 160}\"/></div>",
            "LOT-7",
            "]d1",
        ),
        (
            "<div><span t-out=\"'010950110102091710LOT-1'\" t-options=\"{'widget': 'barcode', 'symbology': 'DataMatrix', 'gs1': True, 'width': 160, 'height': 160}\"/></div>",
            "010950110102091710LOT-1",
            "]d2",
        ),
        (
            "<div><img src='/report/barcode/ECC200DataMatrix/10LOT-7%1D2110?gs1=1&amp;width=160&amp;height=160'/></div>",
            "10LOT-7\u{1d}2110",
            "]d2",
        ),
        (
            "<div><img src='/report/barcode/DataMatrix/10LOT-7%1D2110?gs1=0&amp;width=160&amp;height=160'/></div>",
            "10LOT-7\u{1d}2110",
            // The string encoder uses UTF-8 ECI for controls; this is ordinary ECI,
            // not the leading-FNC1 identifier of GS1.
            "]d4",
        ),
    ] {
        let language = Language::new("en_US").unwrap();
        let sources = [TemplateSource::new("test.datamatrix", arch)];
        let program = ReportProgram::compile(
            "test.datamatrix",
            &sources,
            &language,
            ReportLimits::default(),
        )
        .unwrap();
        let document = program
            .evaluate(
                &EvalContext::standard(),
                &RecordStore::new(),
                &EvalOptions::default(),
                page(),
            )
            .unwrap();
        let pdf = engine().render(&document).unwrap();
        assert!(pdf.warnings().is_empty());
        let (grey, width, height) = raster(pdf.bytes(), 4.0);
        let found =
            rxing::helpers::detect_in_luma(grey, width, height, Some(BarcodeFormat::DATA_MATRIX))
                .unwrap();
        assert_eq!(*found.getBarcodeFormat(), BarcodeFormat::DATA_MATRIX);
        assert_eq!(found.getText(), value);
        assert_eq!(
            found
                .getRXingResultMetadata()
                .get(&RXingResultMetadataType::SYMBOLOGY_IDENTIFIER),
            Some(&RXingResultMetadataValue::SymbologyIdentifier(
                identifier.into()
            ))
        );
    }
}

#[test]
fn extended39_full_ascii_pdf_reads_with_extended_decoder() {
    use rxing::{
        BinaryBitmap, Luma8LuminanceSource, Reader, common::HybridBinarizer, oned::Code39Reader,
    };
    let ascii: Vec<u8> = (0u8..=127).collect();
    for bytes in ascii.chunks(8) {
        let value = std::str::from_utf8(bytes).unwrap();
        let code = Code::encode(
            Symbology::Extended39,
            value,
            QrLevel::Low,
            &Limits::default(),
        )
        .unwrap();
        let pdf = printed_code(&code, (380.0, 60.0), ModuleGrid::default(), false);
        let (grey, width, height) = raster(&pdf, 4.0);
        let source = Luma8LuminanceSource::new(grey, width, height).unwrap();
        let mut image = BinaryBitmap::new(HybridBinarizer::new(source));
        // The barcode sits near the page top, outside the reader's default middle-row scan.
        let hints = rxing::DecodeHints {
            TryHarder: Some(true),
            ..Default::default()
        };
        let found = Code39Reader::with_all_config(false, true)
            .decode_with_hints(&mut image, &hints)
            .unwrap();
        assert_eq!(*found.getBarcodeFormat(), BarcodeFormat::CODE_39);
        assert_eq!(found.getText(), value, "ASCII batch {bytes:?}");
    }
}

#[test]
fn gs1_code128_pdf_preserves_function_symbols_and_weighted_checksum() {
    use rxing::{DecodeHints, RXingResultMetadataType, RXingResultMetadataValue};
    for value in [
        "12345678",
        "12345\u{1d}67890",
        "10LOT-7\u{1d}2110",
        "AB12\u{1d}345678CD",
    ] {
        for dpi in [203, 300, 600] {
            for gs1 in [false, true] {
                let code = if gs1 {
                    Code::encode_gs1_code128(value, &Limits::default()).unwrap()
                } else {
                    Code::encode(Symbology::Code128, value, QrLevel::Low, &Limits::default())
                        .unwrap()
                };
                let pdf = printed_code(&code, (380.0, 60.0), ModuleGrid::new(dpi).unwrap(), false);
                let (grey, width, height) = raster(&pdf, dpi as f32 / 72.0);
                let mut hints = DecodeHints {
                    AssumeGs1: Some(true),
                    ..Default::default()
                };
                let found = rxing::helpers::detect_in_luma_with_hints(
                    grey,
                    width,
                    height,
                    Some(BarcodeFormat::CODE_128),
                    &mut hints,
                )
                .unwrap();
                assert_eq!(
                    found.getText(),
                    if gs1 {
                        format!("]C1{value}")
                    } else {
                        value.to_owned()
                    }
                );
                assert_eq!(
                    found
                        .getRXingResultMetadata()
                        .get(&RXingResultMetadataType::SYMBOLOGY_IDENTIFIER),
                    Some(&RXingResultMetadataValue::SymbologyIdentifier(
                        if gs1 { "]C1" } else { "]C0" }.into()
                    ))
                );
                let words = found.getRawBytes();
                assert_eq!(words.last(), Some(&106));
                let checksum_at = words.len() - 2;
                let weighted = usize::from(words[0])
                    + words[1..checksum_at]
                        .iter()
                        .enumerate()
                        .map(|(index, word)| (index + 1) * usize::from(*word))
                        .sum::<usize>();
                assert_eq!(usize::from(words[checksum_at]), weighted % 103);
                let functions = words[1..checksum_at]
                    .iter()
                    .filter(|word| **word == 102)
                    .count();
                if gs1 {
                    assert_eq!(words[1], 102, "FNC1 must immediately follow start");
                    assert_eq!(functions, 1 + value.bytes().filter(|b| *b == 29).count());
                } else {
                    assert_eq!(functions, 0, "ordinary ASCII GS must not become FNC1");
                }
            }
        }
    }
}

#[test]
fn itf14_bearer_pdf_reads_at_printer_resolution() {
    for dpi in [203, 300, 600] {
        for bearer in [2, 10] {
            for caption in [false, true] {
                let code = Code::encode(
                    Symbology::Itf14,
                    "1540014128876",
                    QrLevel::Low,
                    &Limits::default(),
                )
                .unwrap()
                .with_bearer_modules(bearer)
                .unwrap();
                let pdf =
                    printed_code(&code, (300.0, 80.0), ModuleGrid::new(dpi).unwrap(), caption);
                let (grey, width, height) = raster(&pdf, dpi as f32 / 72.0);
                let found =
                    rxing::helpers::detect_in_luma(grey, width, height, Some(BarcodeFormat::ITF))
                        .unwrap();
                assert_eq!(found.getText(), "15400141288763");
            }
        }
    }
}

#[test]
fn extended39_printable_pdf_reads_at_printer_resolution() {
    use rxing::{
        BinaryBitmap, Luma8LuminanceSource, Reader, common::HybridBinarizer, oned::Code39Reader,
    };
    for dpi in [203, 300, 600] {
        for caption in [false, true] {
            let value = "aZ*$+%/@";
            let code = Code::encode(
                Symbology::Extended39,
                value,
                QrLevel::Low,
                &Limits::default(),
            )
            .unwrap();
            let pdf = printed_code(&code, (380.0, 60.0), ModuleGrid::new(dpi).unwrap(), caption);
            let (grey, width, height) = raster(&pdf, dpi as f32 / 72.0);
            let source = Luma8LuminanceSource::new(grey, width, height).unwrap();
            let mut image = BinaryBitmap::new(HybridBinarizer::new(source));
            let hints = rxing::DecodeHints {
                TryHarder: Some(true),
                ..Default::default()
            };
            let found = Code39Reader::with_all_config(false, true)
                .decode_with_hints(&mut image, &hints)
                .unwrap();
            assert_eq!(found.getText(), value);
        }
    }
}
