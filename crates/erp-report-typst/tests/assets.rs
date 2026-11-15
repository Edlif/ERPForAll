// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! Images, trusted vectors and table spans through the native renderer.
use erp_report::{
    DocumentBuilder, DocumentOptions, LayoutEngine, Limits, Node, NodeId, PageSetup, ReportError,
    Vector,
};
use erp_report_typst::TypstEngine;
use std::{path::Path, sync::Arc};

fn engine() -> TypstEngine {
    TypstEngine::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts"),
        Limits::default(),
    )
    .unwrap()
}
fn options() -> DocumentOptions {
    DocumentOptions::new(
        "assets",
        "Assets",
        PageSetup::new(400.0, 500.0, 35.0).unwrap(),
    )
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend(kind);
    out.extend(data);
    let crc = crc32(&out[start..]);
    out.extend(crc.to_be_bytes());
}
/// A valid RGB PNG of one colour, written with a stored (uncompressed) zlib stream.
fn png(width: u32, height: u32) -> Vec<u8> {
    // Each row: filter byte 0 (none), then the pixels.
    let row: Vec<u8> = std::iter::once(0)
        .chain([200, 40, 40].repeat(width as usize))
        .collect();
    let raw = row.repeat(height as usize);
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        z.push(u8::from(last));
        let len = block.len() as u16;
        z.extend(len.to_le_bytes());
        z.extend((!len).to_le_bytes());
        z.extend(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
fn image(data: Vec<u8>, alt: &str) -> Node {
    Node::Image {
        data: Arc::from(data),
        width: Some(60.0),
        height: None,
        alt: alt.into(),
    }
}
fn render(b: DocumentBuilder, body: NodeId) -> Result<erp_report::RenderResult, ReportError> {
    engine().render(&b.finish(body, options())?)
}
/// Image XObjects of the PDF, as (width, height).
fn raster_objects(bytes: &[u8]) -> Vec<(i64, i64)> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    let mut found = Vec::new();
    for object in pdf.objects.values() {
        if let Ok(stream) = object.as_stream()
            && stream.dict.get(b"Subtype").and_then(|s| s.as_name()).ok() == Some(&b"Image"[..])
        {
            let w = stream.dict.get(b"Width").unwrap().as_i64().unwrap();
            let h = stream.dict.get(b"Height").unwrap().as_i64().unwrap();
            found.push((w, h));
        }
    }
    found
}
fn page_texts(bytes: &[u8]) -> Vec<String> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    pdf.get_pages()
        .keys()
        .map(|page| pdf.extract_text(&[*page]).unwrap())
        .collect()
}
/// Decompressed content of every stream that is not an image.
fn drawing(bytes: &[u8]) -> Vec<u8> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    let mut all = Vec::new();
    for object in pdf.objects.values() {
        if let Ok(stream) = object.as_stream()
            && stream.dict.get(b"Subtype").and_then(|s| s.as_name()).ok() != Some(&b"Image"[..])
        {
            all.extend(
                stream
                    .decompressed_content()
                    .unwrap_or_else(|_| stream.content.clone()),
            );
            all.push(b'\n');
        }
    }
    all
}

/// in the body and in a running header)
#[test]
fn raster_images_reach_the_pdf() {
    let mut b = DocumentBuilder::new(Limits::default());
    let logo = b.add(image(png(7, 5), "logo")).unwrap();
    b.set_header(logo).unwrap();
    let body_image = b.add(image(png(12, 9), "photo")).unwrap();
    let text = b.add(Node::Text("after image".into())).unwrap();
    let body = b
        .add(Node::Flow {
            children: vec![body_image, text],
        })
        .unwrap();
    let out = render(b, body).unwrap();
    let mut objects = raster_objects(out.bytes());
    objects.sort_unstable();
    assert_eq!(objects, vec![(7, 5), (12, 9)]);
    assert!(page_texts(out.bytes())[0].contains("after image"));
    assert!(out.warnings().is_empty(), "{:?}", out.warnings());
}

/// before layout)
#[test]
fn corrupt_raster_is_an_image_error() {
    let mut data = png(8, 8);
    let idat = data.windows(4).position(|w| w == b"IDAT").unwrap();
    data.truncate(idat + 10);
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(image(data, "broken")).unwrap();
    let e = render(b, body).unwrap_err();
    assert!(matches!(e, ReportError::Image { .. }), "{e:?}");
    assert!(
        e.to_string()
            .starts_with("report image: image node 0 cannot be decoded: "),
        "{e}"
    );
}

/// warning; its markup never reaches the renderer)
#[test]
fn user_svg_is_a_placeholder_with_a_warning() {
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"50\" height=\"20\"><text y=\"15\">SVGINNERTEXT</text></svg>";
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(image(svg.to_vec(), "Company stamp")).unwrap();
    let out = render(b, body).unwrap();
    assert_eq!(
        out.warnings(),
        [
            "image node 0: SVG from a user or upload is rendered only by the WorkerPool runner (a later release); printed as a placeholder"
        ]
    );
    let text = &page_texts(out.bytes())[0];
    // The placeholder wraps its alt text inside the box.
    let words: String = text.split_whitespace().collect();
    assert!(words.contains("Companystamp"), "{text:?}");
    assert!(!text.contains("SVGINNERTEXT"), "{text:?}");
    assert!(raster_objects(out.bytes()).is_empty());
}

/// rectangles draws fewer paths)
#[test]
fn trusted_vectors_draw() {
    let draw = |rects: &[[f64; 4]]| {
        let mut b = DocumentBuilder::new(Limits::default());
        let vector = Vector::rectangles(20.0, 10.0, rects, [0, 0, 0]).unwrap();
        let body = b
            .add(Node::Vector {
                vector,
                width: Some(100.0),
                height: Some(50.0),
                alt: "barcode".into(),
            })
            .unwrap();
        let out = render(b, body).unwrap();
        assert!(out.warnings().is_empty(), "{:?}", out.warnings());
        drawing(out.bytes())
    };
    let bars = draw(&[
        [0.0, 0.0, 2.0, 10.0],
        [4.0, 0.0, 1.0, 10.0],
        [7.0, 0.0, 3.0, 10.0],
    ]);
    let empty = draw(&[]);
    let fills = |d: &[u8]| {
        d.split(|c| *c == b'\n')
            .filter(|l| l.ends_with(b" f") || *l == b"f")
            .count()
    };
    assert!(
        fills(&bars) > fills(&empty),
        "{}",
        String::from_utf8_lossy(&bars)
    );
}

fn spanned(b: &mut DocumentBuilder, label: &str, colspan: usize, rowspan: usize) -> NodeId {
    let child = b.add(Node::Text(label.into())).unwrap();
    b.add(Node::Cell {
        child,
        colspan,
        rowspan,
    })
    .unwrap()
}

/// crosses page boundaries without losing a row)
#[test]
fn table_spans_cross_pages() {
    let rows = 60;
    let mut b = DocumentBuilder::new(Limits::default());
    let header = spanned(&mut b, "HEADWIDE", 2, 1);
    let mut cells = vec![spanned(&mut b, "TALLSPAN", 1, rows)];
    for row in 0..rows {
        cells.push(b.add(Node::Text(format!("r{row:03}"))).unwrap());
    }
    let pair = [
        spanned(&mut b, "LEFTPAIR", 1, 1),
        spanned(&mut b, "RIGHTPAIR", 1, 1),
    ];
    let below = spanned(&mut b, "BOTTOMWIDE", 2, 1);
    cells.extend(pair);
    cells.push(below);
    let body = b
        .add(Node::Table {
            columns: 2,
            header: vec![header],
            cells,
        })
        .unwrap();
    let out = render(b, body).unwrap();
    let pages = page_texts(out.bytes());
    assert!(pages.len() >= 2, "{} pages", pages.len());
    for page in &pages {
        assert_eq!(page.matches("HEADWIDE").count(), 1, "{page:?}");
    }
    let all = pages.concat();
    for row in 0..rows {
        assert_eq!(all.matches(&format!("r{row:03}")).count(), 1, "row {row}");
    }
    for label in ["TALLSPAN", "LEFTPAIR", "RIGHTPAIR", "BOTTOMWIDE"] {
        assert_eq!(all.matches(label).count(), 1, "{label}");
    }
}
