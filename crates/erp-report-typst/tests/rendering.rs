// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
use erp_report::{
    Alignment, CrossAlignment, DocTree, DocumentBuilder, DocumentOptions, LayoutEngine, LimitKind,
    Limits, Node, PageCounter, PageSetup, PdfStandard, RenderDate, ReportError, Style,
};
use erp_report_typst::{RENDERER, TypstEngine};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn fonts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts")
}
fn engine() -> TypstEngine {
    TypstEngine::new(fonts(), Limits::default()).unwrap()
}
fn options() -> DocumentOptions {
    DocumentOptions::new(
        "fixture",
        "Fixture",
        PageSetup::new(400.0, 500.0, 35.0).unwrap(),
    )
}
fn text_pdf(text: &str) -> Vec<u8> {
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(Node::Text(text.into())).unwrap();
    engine()
        .render(&b.finish(body, options()).unwrap())
        .unwrap()
        .bytes()
        .to_vec()
}
fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}
/// Text of every page, extracted by the independent parser after reopening the bytes.
fn page_texts(bytes: &[u8]) -> Vec<String> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    pdf.get_pages()
        .keys()
        .map(|page| pdf.extract_text(&[*page]).unwrap())
        .collect()
}
fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle)
}
/// Running header, `current / total` footer and one page per section, labelled with letters only so
/// that every digit on a page comes from the counters.
fn counted_document(pages: usize) -> DocTree {
    let mut b = DocumentBuilder::new(Limits::default());
    let header = b.add(Node::Text("RUNNING-HEADER".into())).unwrap();
    b.set_header(header).unwrap();
    let current = b.add(Node::PageCounter(PageCounter::Current)).unwrap();
    let slash = b.add(Node::Text(" / ".into())).unwrap();
    let total = b.add(Node::PageCounter(PageCounter::Total)).unwrap();
    let footer = b
        .add(Node::Inline {
            children: vec![current, slash, total],
            style: Style::default(),
        })
        .unwrap();
    b.set_footer(footer).unwrap();
    let mut children = Vec::new();
    for page in 0..pages {
        if page > 0 {
            children.push(b.add(Node::PageBreak).unwrap());
        }
        let letter = char::from(b'A' + u8::try_from(page).unwrap());
        children.push(b.add(Node::Text(format!("Section-{letter}"))).unwrap());
    }
    let body = b.add(Node::Flow { children }).unwrap();
    b.finish(body, options()).unwrap()
}
/// One-column table with a header cell and uniquely labelled rows. Also the constructor of the
/// 2,000 and 10,000 row inputs of the deferred measurements.
fn table_document(rows: usize) -> DocTree {
    let mut b = DocumentBuilder::new(Limits::default());
    let header = b.add(Node::Text("HEADER".into())).unwrap();
    let mut cells = Vec::new();
    for row in 0..rows {
        cells.push(b.add(Node::Text(format!("row-{row:05}"))).unwrap());
    }
    let body = b
        .add(Node::Table {
            columns: 1,
            header: vec![header],
            cells,
        })
        .unwrap();
    b.finish(body, options()).unwrap()
}
/// A document using running regions, counters, styles, a list and a multi-page table.
fn rich_document(id: &str, title: &str, word: &str) -> DocTree {
    let mut b = DocumentBuilder::new(Limits::default());
    let header = b.add(Node::Text("Replica header".into())).unwrap();
    b.set_header(header).unwrap();
    let footer = b.add(Node::PageCounter(PageCounter::Total)).unwrap();
    b.set_footer(footer).unwrap();
    let text = b.add(Node::Text(word.into())).unwrap();
    let block = b
        .add(Node::Block {
            children: vec![text],
            style: Style::default().with_bold(true).with_color([20, 60, 120]),
        })
        .unwrap();
    let mut cells = Vec::new();
    for row in 0..80 {
        cells.push(b.add(Node::Text(format!("line {row}"))).unwrap());
    }
    let table = b
        .add(Node::Table {
            columns: 2,
            header: vec![],
            cells,
        })
        .unwrap();
    let body = b
        .add(Node::Flow {
            children: vec![block, table],
        })
        .unwrap();
    let options = DocumentOptions::new(id, title, PageSetup::new(400.0, 500.0, 35.0).unwrap())
        .with_date(RenderDate::new(2020, 2, 29).unwrap());
    b.finish(body, options).unwrap()
}

#[test]
fn markup_is_text() {
    let literal = "literal #read(\"/etc/passwd\") #panic(\"sentinel\") [abc] $x$ *bold*";
    let pages = page_texts(&text_pdf(literal));
    assert_eq!(pages.len(), 1);
    assert_eq!(compact(&pages[0]), compact(literal));
}

const REPLICA_SWITCH: &str = "ERP_REPORT_REPLICA_CHILD";
fn replica_line() -> String {
    let output = engine()
        .render(&rich_document("replica", "Replica", "same"))
        .unwrap();
    format!("REPLICA {} {}", sha256(output.bytes()), output.page_count())
}
/// A renderer replica: this test binary started again as a separate process.
fn spawn_replica() -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "replica_child", "--nocapture"])
        .env(REPLICA_SWITCH, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap()
}
fn replica_result(child: std::process::Child) -> String {
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    stdout
        .lines()
        .find_map(|line| line.find("REPLICA ").map(|at| line[at..].to_owned()))
        .unwrap_or_else(|| panic!("replica printed no result: {stdout:?}"))
}
#[test]
fn replica_child() {
    if std::env::var_os(REPLICA_SWITCH).is_some() {
        println!("{}", replica_line());
    }
}

#[test]
fn equal_inputs_give_identical_bytes_across_eviction_and_processes() {
    let engine = engine();
    let first = engine
        .render(&rich_document("replica", "Replica", "same"))
        .unwrap();
    assert!(first.page_count() > 1);
    let again = engine
        .render(&rich_document("replica", "Replica", "same"))
        .unwrap();
    assert_eq!(first.bytes(), again.bytes());
    engine.finish_batch();
    let evicted = engine
        .render(&rich_document("replica", "Replica", "same"))
        .unwrap();
    assert_eq!(first.bytes(), evicted.bytes());

    // Two replicas render at once; one has exited before a cold replacement starts.
    let here = format!("REPLICA {} {}", sha256(first.bytes()), first.page_count());
    assert_eq!(here, replica_line());
    let (one, two) = (spawn_replica(), spawn_replica());
    assert_eq!(replica_result(one), here);
    let replacement = spawn_replica();
    assert_eq!(replica_result(two), here);
    assert_eq!(replica_result(replacement), here);

    // A changed word or title changes the output where an independent reader looks for it.
    let word = engine
        .render(&rich_document("replica", "Replica", "changed"))
        .unwrap();
    assert_ne!(first.bytes(), word.bytes());
    assert!(page_texts(first.bytes())[0].contains("same"));
    assert!(page_texts(word.bytes())[0].contains("changed"));
    let title = engine
        .render(&rich_document("replica", "Retitled", "same"))
        .unwrap();
    assert_ne!(first.bytes(), title.bytes());
    assert_eq!(info_entry(first.bytes(), b"Title"), b"Replica");
    assert_eq!(info_entry(title.bytes(), b"Title"), b"Retitled");
}
fn info_entry(bytes: &[u8], key: &[u8]) -> Vec<u8> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    let info = pdf.trailer.get(b"Info").unwrap().as_reference().unwrap();
    let entry = pdf.get_dictionary(info).unwrap().get(key).unwrap();
    entry.as_str().unwrap().to_vec()
}
fn trailer_id(bytes: &[u8]) -> String {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    format!("{:?}", pdf.trailer.get(b"ID").unwrap())
}

#[test]
fn counters_and_running_regions_paginate_exactly() {
    for total in [3, 7] {
        let output = engine().render(&counted_document(total)).unwrap();
        assert_eq!(output.page_count(), total);
        let pages = page_texts(output.bytes());
        assert_eq!(pages.len(), total);
        for (index, text) in pages.iter().enumerate() {
            let page = index + 1;
            let letter = char::from(b'A' + u8::try_from(index).unwrap());
            assert!(text.contains("RUNNING-HEADER"), "page {page}: {text:?}");
            assert!(
                text.contains(&format!("Section-{letter}")),
                "page {page}: {text:?}"
            );
            let counters: String = text
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '/')
                .collect();
            assert_eq!(counters, format!("{page}/{total}"), "page {page}: {text:?}");
        }
    }
}

#[test]
fn tables_repeat_headers_and_keep_every_row() {
    for rows in [200, 1001] {
        let output = engine().render(&table_document(rows)).unwrap();
        let pages = page_texts(output.bytes());
        assert_eq!(pages.len(), output.page_count());
        assert!(pages.len() > 1);
        let mut combined = String::new();
        for (index, text) in pages.iter().enumerate() {
            assert_eq!(text.matches("HEADER").count(), 1, "page {}", index + 1);
            combined.push_str(text);
        }
        let mut after = 0;
        for row in 0..rows {
            let label = format!("row-{row:05}");
            assert_eq!(combined.matches(&label).count(), 1, "{label}");
            let at = combined.find(&label).unwrap();
            assert!(at >= after, "{label} is out of order");
            after = at;
        }
    }
    // Inputs of the deferred measurements are admitted; they are not rendered or timed here.
    for rows in [2_000, 10_000] {
        table_document(rows)
            .check_limits(Limits::default())
            .unwrap();
    }
}

#[test]
fn depth_32_and_mixed_containers_render() {
    for grid in [false, true] {
        let mut b = DocumentBuilder::new(Limits::default());
        let mut child = b.add(Node::Text("nested".into())).unwrap();
        for depth in 1..32 {
            child = b
                .add(if grid && depth % 2 == 0 {
                    Node::Grid {
                        columns: vec![1.0],
                        gutter: 0.0,
                        cells: vec![child],
                        align: erp_report::CrossAlignment::Start,
                    }
                } else {
                    Node::Block {
                        children: vec![child],
                        style: Style::default(),
                    }
                })
                .unwrap();
        }
        let output = engine()
            .render(&b.finish(child, options()).unwrap())
            .unwrap();
        assert_eq!(output.page_count(), 1);
        assert_eq!(compact(&page_texts(output.bytes())[0]), "nested");
    }
}

/// How the text layer of a fixture is compared with its input.
enum Order {
    /// Left to right on the page is reading order.
    LeftToRight,
    /// One right-to-left run: left to right on the page is the reverse of reading order.
    RightToLeft,
    /// The font draws the dot of a letter as a glyph of its own and the writer gives that glyph the
    /// letter as its text, so every reader sees the letter twice: every input character is present
    /// and nothing foreign is.
    Shaped,
}
/// What an independent parser reads from the single line of a page: glyph text from the font's map,
/// replaced by ActualText where the writer gives one, and no reordering rule.
struct Line {
    /// Characters of the text runs, runs ordered by their horizontal position on the page.
    left_to_right: String,
    /// Glyph codes shown.
    glyphs: Vec<u16>,
    /// Runs the writer marked as stored in reverse of reading order.
    reversed_runs: usize,
}
/// Glyph code to text, from the font's ToUnicode map.
fn glyph_text(pdf: &lopdf::Document, font: &lopdf::Dictionary) -> Vec<(u16, String)> {
    let stream = font
        .get_deref(b"ToUnicode", pdf)
        .unwrap()
        .as_stream()
        .unwrap();
    let map = String::from_utf8(stream.decompressed_content().unwrap()).unwrap();
    assert!(
        !map.contains("beginbfrange"),
        "range entries are not read here"
    );
    let hex = |token: &str| -> Vec<u16> {
        let digits = token.trim_start_matches('<').trim_end_matches('>');
        assert_eq!(digits.len() % 4, 0, "{token}");
        (0..digits.len() / 4)
            .map(|i| u16::from_str_radix(&digits[4 * i..4 * i + 4], 16).unwrap())
            .collect()
    };
    let entries = map.split("beginbfchar").skip(1);
    entries
        .flat_map(|section| section.split("endbfchar").next().unwrap().lines())
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (code, text) = line.trim().split_once(' ').unwrap();
            (hex(code)[0], String::from_utf16(&hex(text)).unwrap())
        })
        .collect()
}
fn read_line(pdf: &lopdf::Document, page: lopdf::ObjectId) -> Line {
    let fonts = pdf.get_page_fonts(page).unwrap();
    assert_eq!(fonts.len(), 1, "one font per language fixture");
    let map = glyph_text(pdf, fonts.values().next().unwrap());
    let content = pdf.get_and_decode_page_content(page).unwrap();
    let mut line = Line {
        left_to_right: String::new(),
        glyphs: Vec::new(),
        reversed_runs: 0,
    };
    let mut runs: Vec<(f32, String)> = Vec::new();
    // Open marked-content sequences; one with an ActualText entry replaces the text of its glyphs
    // and takes the position of the first of them.
    let mut marked: Vec<Option<(String, Option<f32>)>> = Vec::new();
    let (mut origin, mut offset) = (0.0, 0.0);
    for operation in &content.operations {
        let operands = &operation.operands;
        match operation.operator.as_str() {
            "cm" => origin = operands[4].as_float().unwrap(),
            "BT" => offset = 0.0,
            "Tm" => offset = operands[4].as_float().unwrap(),
            "BMC" | "BDC" => {
                if operands[0].as_name().unwrap() == b"ReversedChars" {
                    line.reversed_runs += 1;
                }
                let actual = match operands.get(1) {
                    Some(lopdf::Object::Dictionary(properties))
                        if properties.has(b"ActualText") =>
                    {
                        let bytes = properties.get(b"ActualText").unwrap().as_str().unwrap();
                        assert_eq!(&bytes[..2], [0xfe, 0xff], "UTF-16 with a byte order mark");
                        let units: Vec<u16> = bytes[2..]
                            .chunks(2)
                            .map(|c| u16::from_be_bytes([c[0], c[1]]))
                            .collect();
                        Some((String::from_utf16(&units).unwrap(), None))
                    }
                    _ => None, // a tag or a structure reference: the glyphs keep their own text
                };
                marked.push(actual);
            }
            "EMC" => {
                if let Some((text, Some(at))) = marked.pop().unwrap() {
                    runs.push((at, text));
                }
            }
            "Tj" | "TJ" => {
                let mut run = String::new();
                let replaced = marked.iter_mut().flatten().next();
                for operand in operands {
                    let items = match operand {
                        lopdf::Object::Array(items) => items.as_slice(),
                        single => std::slice::from_ref(single),
                    };
                    for item in items {
                        let lopdf::Object::String(bytes, _) = item else {
                            continue; // a kerning adjustment between glyphs
                        };
                        assert_eq!(bytes.len() % 2, 0, "two-byte glyph codes");
                        for code in bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])) {
                            line.glyphs.push(code);
                            if replaced.is_none() {
                                let entry = map.iter().find(|(glyph, _)| *glyph == code);
                                let (_, chars) =
                                    entry.unwrap_or_else(|| panic!("glyph {code} has no text"));
                                run.push_str(chars);
                            }
                        }
                    }
                }
                match replaced {
                    Some((_, at)) => {
                        at.get_or_insert(origin + offset);
                    }
                    None => runs.push((origin + offset, run)),
                }
            }
            _ => {} // graphics state operators carry no text
        }
    }
    runs.sort_by(|a, b| a.0.total_cmp(&b.0));
    line.left_to_right = runs.into_iter().map(|(_, run)| run).collect();
    line
}

/// file is read, and the message names the optional font addon's task)
#[test]
fn cjk_families_are_refused_by_name_with_their_addon_task() {
    for family in [
        "Noto Sans SC",
        "Noto Sans TC",
        "Noto Sans JP",
        "Noto Sans KR",
    ] {
        let mut b = DocumentBuilder::new(Limits::default());
        let body = b.add(Node::Text("text".into())).unwrap();
        let options = options().with_fonts(vec![family.into()]).unwrap();
        let error = engine()
            .render(&b.finish(body, options).unwrap())
            .unwrap_err();
        assert!(matches!(error, ReportError::Font { .. }), "{error}");
        assert_eq!(
            error.to_string(),
            format!(
                "report font: family {family:?} is not in the shipped font manifest; the Chinese, Japanese and Korean families come as a separate optional addon (a later release)"
            )
        );
    }
}

/// missing-glyph box is shown, right-to-left runs are marked and laid out from the right, and the
/// text layer matches as far as the family allows; reading-order extraction with pdfium and the
/// raster are evidence outside this test, and the raster has no reviewer besides the implementer)
#[test]
fn shipped_language_fixtures_use_their_fonts() {
    use Order::{LeftToRight, RightToLeft, Shaped};
    let hebrew = ("שלום", "עולם");
    let mixed = format!("{} ERP 2026 {}", hebrew.0, hebrew.1);
    let reversed = |word: &str| word.chars().rev().collect::<String>();
    // In a right-to-left paragraph the Latin run keeps its own order between the two words, and the
    // first word is the rightmost.
    let mixed_on_page = format!("{}ERP2026{}", reversed(hebrew.1), reversed(hebrew.0));
    let arabic = "مرحبا بالعالم";
    let fixtures: [(&str, &str, &str, Order); 8] = [
        ("he", "Noto Sans Hebrew", "שלום עולם", RightToLeft),
        ("he", "Noto Sans Hebrew", mixed.as_str(), LeftToRight),
        // The two Noto Arabic families draw the dot of a letter as a glyph of its own.
        ("ar", "Noto Naskh Arabic", arabic, Shaped),
        ("ar", "Noto Sans Arabic", arabic, Shaped),
        ("ar", "Amiri", arabic, RightToLeft),
        ("th", "Noto Sans Thai", "สวัสดีชาวโลก", LeftToRight),
        ("hi", "Noto Sans Devanagari", "नमस्ते दुनिया", LeftToRight),
        ("en", "Noto Sans", "Invoice total 1,250.00", LeftToRight),
    ];
    let artifacts = std::env::var_os("ERP_REPORT_ARTIFACTS").map(PathBuf::from);
    let mut failures = Vec::new();
    for (index, (language, family, text, order)) in fixtures.iter().enumerate() {
        let name = format!("{index:02}-{language}-{}", family.replace(' ', ""));
        let mut b = DocumentBuilder::new(Limits::default());
        let body = b.add(Node::Text((*text).into())).unwrap();
        let options = options()
            .with_language(language)
            .unwrap()
            .with_fonts(vec![(*family).into()])
            .unwrap();
        let output = engine().render(&b.finish(body, options).unwrap()).unwrap();
        if let Some(directory) = &artifacts {
            std::fs::write(directory.join(format!("{name}.pdf")), output.bytes()).unwrap();
        }
        if output.page_count() != 1 || !output.warnings().is_empty() {
            failures.push(format!(
                "{name}: {} pages, warnings {:?}",
                output.page_count(),
                output.warnings()
            ));
            continue;
        }
        let pdf = lopdf::Document::load_mem(output.bytes()).unwrap();
        let page = pdf.get_pages()[&1];
        for font in pdf.get_page_fonts(page).unwrap().values() {
            let base = String::from_utf8_lossy(font.get(b"BaseFont").unwrap().as_name().unwrap());
            let face = base.split('+').next_back().unwrap();
            if !face.starts_with(&family.replace(' ', "")) {
                failures.push(format!("{name}: embedded {base}, requested {family}"));
            }
        }
        let line = read_line(&pdf, page);
        if line.glyphs.is_empty() || line.glyphs.contains(&0) {
            failures.push(format!(
                "{name}: missing glyph or none in {:?}",
                line.glyphs
            ));
        }
        if matches!(*language, "he" | "ar") != (line.reversed_runs > 0) {
            failures.push(format!("{name}: {} reversed runs", line.reversed_runs));
        }
        let on_page = compact(&line.left_to_right);
        let matched = match order {
            LeftToRight if *text == mixed => on_page == mixed_on_page,
            LeftToRight => on_page == compact(text),
            RightToLeft => on_page == compact(&reversed(text)),
            Shaped => {
                let input = compact(text);
                input.chars().all(|c| on_page.contains(c))
                    && on_page.chars().all(|c| input.contains(c))
            }
        };
        if !matched {
            failures.push(format!("{name}: on the page {on_page:?} for {text:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// option exists in the public API yet)
#[test]
fn output_limits_refuse_actual_results() {
    // A table that flows over several pages without a page break: only the layout knows the count.
    let doc = table_document(200);
    let make = |limits| TypstEngine::new(fonts(), limits).unwrap();
    let pages = engine().render(&doc).unwrap().page_count();
    assert!(pages > 2);
    let lowered = Limits::default()
        .lowered(LimitKind::Pages, pages - 1)
        .unwrap();
    let e = make(lowered).render(&doc).unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::Pages,
            ..
        }
    ));
    assert_eq!(
        e.to_string(),
        format!("report limit pages exceeded: {pages} > {}", pages - 1)
    );
    let exact = Limits::default().lowered(LimitKind::Pages, pages).unwrap();
    assert_eq!(make(exact).render(&doc).unwrap().page_count(), pages);
    // Explicit page breaks are refused by the same limit before any layout.
    let e = make(Limits::default().lowered(LimitKind::Pages, 1).unwrap())
        .render(&counted_document(2))
        .unwrap_err();
    assert_eq!(e.to_string(), "report limit pages exceeded: 2 > 1");
    let actual = engine().render(&doc).unwrap().bytes().len();
    let exact = make(
        Limits::default()
            .lowered(LimitKind::PdfBytes, actual)
            .unwrap(),
    );
    assert_eq!(exact.render(&doc).unwrap().bytes().len(), actual);
    let e = make(
        Limits::default()
            .lowered(LimitKind::PdfBytes, actual - 1)
            .unwrap(),
    )
    .render(&doc)
    .unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::PdfBytes,
            ..
        }
    ));
    assert_eq!(
        e.to_string(),
        format!("report limit pdf_bytes exceeded: {actual} > {}", actual - 1)
    );
}

/// mapping; PDF/A-3 and Factur-X conformance are not claimed)
#[test]
fn export_maps_identity_date_and_standard() {
    let render = |id: &str, standard| {
        let mut b = DocumentBuilder::new(Limits::default());
        let body = b.add(Node::Text("export".into())).unwrap();
        let options =
            DocumentOptions::new(id, "Fixture", PageSetup::new(400.0, 500.0, 35.0).unwrap())
                .with_date(RenderDate::new(2020, 2, 29).unwrap())
                .with_standard(standard);
        engine().render(&b.finish(body, options).unwrap()).unwrap()
    };
    let plain = render("invoice-1", PdfStandard::Pdf17);
    assert_eq!(plain.identity().document(), "invoice-1");
    assert_eq!(plain.identity().renderer(), RENDERER);
    assert!(plain.warnings().is_empty(), "{:?}", plain.warnings());
    assert_eq!(page_texts(plain.bytes()).len(), plain.page_count());
    assert_eq!(
        info_entry(plain.bytes(), b"CreationDate"),
        b"D:20200229000000Z"
    );
    assert_eq!(info_entry(plain.bytes(), b"Title"), b"Fixture");
    let same = render("invoice-1", PdfStandard::Pdf17);
    assert_eq!(trailer_id(plain.bytes()), trailer_id(same.bytes()));
    let other = render("invoice-2", PdfStandard::Pdf17);
    assert_ne!(trailer_id(plain.bytes()), trailer_id(other.bytes()));
    let archival = render("invoice-1", PdfStandard::PdfA3b);
    assert!(contains(archival.bytes(), b"<pdfaid:part>3</pdfaid:part>"));
    assert!(!contains(plain.bytes(), b"pdfaid:part"));
}

/// text; style geometry, images, vectors and spans are not asserted)
#[test]
fn admitted_nodes_and_styles_reach_output() {
    let mut b = DocumentBuilder::new(Limits::default());
    let first = b.add(Node::Text("styled".into())).unwrap();
    let bold = b
        .add(Node::Block {
            children: vec![first],
            style: Style::default()
                .with_size(16.0)
                .unwrap()
                .with_padding(4.0)
                .unwrap()
                .with_bold(true)
                .with_italic(true)
                .with_underline(true)
                .with_color([30, 80, 140])
                .with_alignment(Alignment::Center)
                .with_keep_together(true),
        })
        .unwrap();
    let a = b.add(Node::Text("alpha".into())).unwrap();
    let z = b.add(Node::Text("omega".into())).unwrap();
    let list = b
        .add(Node::List {
            ordered: true,
            items: vec![a, z],
        })
        .unwrap();
    let grid = b
        .add(Node::Grid {
            columns: vec![1.0, 2.0],
            gutter: 4.0,
            cells: vec![bold, list],
            align: erp_report::CrossAlignment::Start,
        })
        .unwrap();
    let dot = b.add(Node::Text("bullet".into())).unwrap();
    let bullets = b
        .add(Node::List {
            ordered: false,
            items: vec![dot],
        })
        .unwrap();
    let small = b.add(Node::Text("inline".into())).unwrap();
    let inline = b
        .add(Node::Inline {
            children: vec![small],
            style: Style::default().with_italic(true),
        })
        .unwrap();
    let line = b.add(Node::LineBreak).unwrap();
    let rule = b.add(Node::Rule).unwrap();
    let end = b.add(Node::Text("ending".into())).unwrap();
    let body = b
        .add(Node::Flow {
            children: vec![grid, bullets, inline, line, rule, end],
        })
        .unwrap();
    let output = engine()
        .render(&b.finish(body, options()).unwrap())
        .unwrap();
    let text = &page_texts(output.bytes())[0];
    for token in ["styled", "alpha", "omega", "bullet", "inline", "ending"] {
        assert_eq!(text.matches(token).count(), 1, "{token}: {text:?}");
    }
    // The ordered list numbers its two items.
    assert!(text.contains('1') && text.contains('2'), "{text:?}");
}

/// A text leaf wrapped in `tables` tables and `others` containers of one other kind.
fn nested_mix(
    b: &mut DocumentBuilder,
    tables: usize,
    others: usize,
    other: &str,
    arrangement: &str,
) -> erp_report::NodeId {
    let mut child = b.add(Node::Text("nested".into())).unwrap();
    let (mut tables_left, mut others_left) = (tables, others);
    while tables_left + others_left > 0 {
        let table = match arrangement {
            "tables-inner" => tables_left > 0,
            "tables-outer" => others_left == 0,
            _ => tables_left > 0 && (others_left == 0 || (tables_left + others_left) % 2 == 0),
        };
        child = if table {
            tables_left -= 1;
            b.add(Node::Table {
                columns: 1,
                header: vec![],
                cells: vec![child],
            })
        } else {
            others_left -= 1;
            b.add(match other {
                "block" => Node::Block {
                    children: vec![child],
                    style: Style::default().with_padding(1.0).unwrap(),
                },
                "grid" => Node::Grid {
                    columns: vec![1.0],
                    gutter: 0.0,
                    cells: vec![child],
                    align: erp_report::CrossAlignment::Start,
                },
                _ => Node::List {
                    ordered: other == "enum",
                    items: vec![child],
                },
            })
        }
        .unwrap();
    }
    child
}

/// running regions, for tables mixed with each other container kind; a table counts two levels and
/// a running region one)
#[test]
fn admitted_nesting_renders_in_body_and_running_regions() {
    let mut failures = Vec::new();
    for region in ["body", "header", "footer"] {
        // Levels left for containers around a text leaf under a ceiling of 32.
        let budget = if region == "body" { 31 } else { 30 };
        for tables in [15, 10, 1, 0] {
            let others = budget - 2 * tables;
            for other in ["block", "grid", "list", "enum"] {
                for arrangement in ["tables-inner", "tables-outer", "alternate"] {
                    let mut b = DocumentBuilder::new(Limits::default());
                    let nested = nested_mix(&mut b, tables, others, other, arrangement);
                    let body = match region {
                        "body" => nested,
                        "header" => {
                            b.set_header(nested).unwrap();
                            b.add(Node::Text("body".into())).unwrap()
                        }
                        _ => {
                            b.set_footer(nested).unwrap();
                            b.add(Node::Text("body".into())).unwrap()
                        }
                    };
                    let options = DocumentOptions::new(
                        "nested",
                        "Nested",
                        PageSetup::new(4000.0, 4000.0, 1000.0).unwrap(),
                    );
                    let name = format!("{region} {tables}+{others} {other} {arrangement}");
                    match engine().render(&b.finish(body, options).unwrap()) {
                        Ok(output) if output.page_count() == 1 => {
                            if !page_texts(output.bytes())[0].contains("nested") {
                                failures.push(format!("{name}: leaf text missing"));
                            }
                        }
                        Ok(output) => {
                            failures.push(format!("{name}: {} pages", output.page_count()))
                        }
                        Err(error) => failures.push(format!("{name}: {error:?}")),
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// replaced by a host font; a PDF/A export that refuses it keeps the text out of the message)
#[test]
fn uncovered_text_is_reported_not_hidden() {
    let render = |standard| {
        let mut b = DocumentBuilder::new(Limits::default());
        let body = b
            .add(Node::Text("Total \u{65e5}\u{672c}\u{8a9e}".into()))
            .unwrap();
        engine().render(&b.finish(body, options().with_standard(standard)).unwrap())
    };
    let output = render(PdfStandard::Pdf17).unwrap();
    assert_eq!(
        output.warnings(),
        ["3 glyphs are in none of the selected font families and print as empty boxes"]
    );
    let pdf = lopdf::Document::load_mem(output.bytes()).unwrap();
    let page = pdf.get_pages()[&1];
    for font in pdf.get_page_fonts(page).unwrap().values() {
        let base = String::from_utf8_lossy(font.get(b"BaseFont").unwrap().as_name().unwrap());
        assert!(base.contains("NotoSans"), "{base}");
    }
    let error = render(PdfStandard::PdfA3b).unwrap_err();
    let ReportError::Pdf { document, reason } = &error else {
        panic!("{error}")
    };
    assert_eq!(document, "fixture");
    // The exporter quotes the document text: that is why the message does not carry its diagnostics.
    assert!(reason.contains('\u{65e5}'), "{reason}");
    assert_eq!(
        error.to_string(),
        "report PDF export failed for fixture: exporter diagnostics are in the error's reason field"
    );
}

/// Each text run of the first page with the height of its origin on the page, in points from the
/// page's bottom edge: the text matrix under the transformation in force where the run is shown
/// (`q`/`Q`/`cm` for the graphics state, `BT`/`Tm`/`Td` for the text position).
fn text_heights(bytes: &[u8]) -> Vec<(String, f32)> {
    type Matrix = [f32; 6];
    fn times(m: Matrix, n: Matrix) -> Matrix {
        [
            m[0] * n[0] + m[1] * n[2],
            m[0] * n[1] + m[1] * n[3],
            m[2] * n[0] + m[3] * n[2],
            m[2] * n[1] + m[3] * n[3],
            m[4] * n[0] + m[5] * n[2] + n[4],
            m[4] * n[1] + m[5] * n[3] + n[5],
        ]
    }
    const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    let page = *pdf.get_pages().values().next().unwrap();
    let fonts = pdf.get_page_fonts(page).unwrap();
    type GlyphMap = Vec<(u16, String)>;
    let maps: Vec<(Vec<u8>, GlyphMap)> = fonts
        .iter()
        .map(|(name, font)| (name.clone(), glyph_text(&pdf, font)))
        .collect();
    let content = pdf.get_and_decode_page_content(page).unwrap();
    let numbers = |operands: &[lopdf::Object]| -> Vec<f32> {
        operands.iter().map(|o| o.as_float().unwrap()).collect()
    };
    let matrix = |operands: &[lopdf::Object]| -> Matrix {
        let n = numbers(operands);
        [n[0], n[1], n[2], n[3], n[4], n[5]]
    };
    let (mut ctm, mut saved, mut text) = (IDENTITY, Vec::new(), IDENTITY);
    let mut font: Option<Vec<u8>> = None;
    let mut runs = Vec::new();
    for operation in &content.operations {
        let operands = &operation.operands;
        match operation.operator.as_str() {
            "q" => saved.push(ctm),
            "Q" => ctm = saved.pop().unwrap(),
            "cm" => ctm = times(matrix(operands), ctm),
            "BT" => text = IDENTITY,
            "Tm" => text = matrix(operands),
            "Td" | "TD" => {
                let n = numbers(operands);
                text = times([1.0, 0.0, 0.0, 1.0, n[0], n[1]], text);
            }
            "Tf" => font = Some(operands[0].as_name().unwrap().to_vec()),
            "Tj" | "TJ" => {
                let name = font.as_ref().unwrap();
                let (_, map) = maps.iter().find(|(n, _)| n == name).unwrap();
                let mut run = String::new();
                for operand in operands {
                    let items = match operand {
                        lopdf::Object::Array(items) => items.as_slice(),
                        single => std::slice::from_ref(single),
                    };
                    for item in items {
                        let lopdf::Object::String(bytes, _) = item else {
                            continue; // a kerning adjustment between glyphs
                        };
                        for code in bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])) {
                            let entry = map.iter().find(|(glyph, _)| *glyph == code);
                            run.push_str(&entry.unwrap().1);
                        }
                    }
                }
                runs.push((run, times(text, ctm)[5]));
            }
            _ => {} // other operators neither move nor show text
        }
    }
    runs
}

/// The heights of the first line, the last line and the short cell of a two-cell grid row: a tall
/// cell of `lines` lines beside the one-line text "short", in the body or as the page header.
fn row_heights(align: CrossAlignment, lines: &[&str], header: bool) -> (f32, f32, f32) {
    let mut b = DocumentBuilder::new(Limits::default());
    let mut children = Vec::new();
    for (index, word) in lines.iter().enumerate() {
        if index > 0 {
            children.push(b.add(Node::LineBreak).unwrap());
        }
        children.push(b.add(Node::Text((*word).into())).unwrap());
    }
    let tall = b
        .add(Node::Block {
            children,
            style: Style::default(),
        })
        .unwrap();
    let short = b.add(Node::Text("short".into())).unwrap();
    let grid = b
        .add(Node::Grid {
            columns: vec![1.0, 1.0],
            gutter: 10.0,
            cells: vec![tall, short],
            align,
        })
        .unwrap();
    let body = if header {
        b.set_header(grid).unwrap();
        b.add(Node::Text("body".into())).unwrap()
    } else {
        grid
    };
    let output = engine()
        .render(&b.finish(body, options()).unwrap())
        .unwrap();
    let runs = text_heights(output.bytes());
    let height = |word: &str| {
        let found: Vec<f32> = runs
            .iter()
            .filter(|(text, _)| text == word)
            .map(|(_, height)| *height)
            .collect();
        assert_eq!(found.len(), 1, "{word} in {runs:?}");
        found[0]
    };
    (
        height(lines[0]),
        height(lines[lines.len() - 1]),
        height("short"),
    )
}

/// A grid row whose cells differ in height places the short cell at the top of the row, in its
/// middle or at its bottom, as the grid says (`align-items-*` in a report template): the same short
/// text sits lower on the page for the middle than for the top, and lower again for the bottom,
/// and the tall cell does not move. The same holds in the page header, which is itself placed at
/// the bottom of its margin: a letterhead row is at its top unless it says otherwise (it printed
/// the company name at the bottom of the row, below the logo's line).
#[test]
fn a_grid_places_its_cells_in_the_row_as_it_says() {
    let close = |a: f32, b: f32| (a - b).abs() < 0.05;
    for (lines, header) in [
        (&["one", "two", "three", "four", "five"][..], false),
        (&["one", "five"][..], true),
    ] {
        let (top_one, top_five, top_short) = row_heights(CrossAlignment::Start, lines, header);
        let (middle_one, middle_five, middle_short) =
            row_heights(CrossAlignment::Center, lines, header);
        let (bottom_one, bottom_five, bottom_short) =
            row_heights(CrossAlignment::End, lines, header);
        // the tall cell fills the row: it stays where it is
        for (one, five) in [(middle_one, middle_five), (bottom_one, bottom_five)] {
            assert!(
                close(one, top_one) && close(five, top_five),
                "header {header}: {one} {five}"
            );
        }
        assert!(top_one > top_five, "the page's bottom edge is height zero");
        // top: on the first line; bottom: on the last line; middle: half way between the two
        assert!(
            close(top_short, top_one),
            "header {header}: {top_short} {top_one}"
        );
        assert!(
            close(bottom_short, top_five),
            "header {header}: {bottom_short} {top_five}"
        );
        assert!(
            close(middle_short, f32::midpoint(top_one, top_five)),
            "header {header}: {middle_short} between {top_one} and {top_five}"
        );
    }
}
