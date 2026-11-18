// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pictures as they are laid out: the size each is drawn at on its page, what is cut off, where it
//! sits, and the pixels that are embedded. Every size is read from the layout, never from the input.
use super::*;
use crate::picture::{Pictures, target_pixels};
use erp_report::{CrossAlignment, ImageFit, ImageFormat, NodeId, PictureQuality, probe_image};
use image::{
    DynamicImage, ExtendedColorType, GenericImageView, Rgb, RgbImage, Rgba, RgbaImage,
    codecs::{gif::GifEncoder, jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
};
use std::sync::Arc;
use typst::layout::Point;

/// One picture of a laid-out document. Lengths are points, rounded to a hundredth.
#[derive(Debug, PartialEq)]
struct Placed {
    page: usize,
    /// Where its top left corner is on the page.
    at: (f64, f64),
    /// The size it is drawn at.
    drawn: (f64, f64),
    /// The box it is cut to, when part of it is cut off.
    cut: Option<(f64, f64)>,
    /// Pixels of what is embedded.
    pixels: (u32, u32),
}
fn points(length: typst::layout::Abs) -> f64 {
    (length.to_pt() * 100.0).round() / 100.0
}
fn walk(frame: &Frame, origin: Point, page: usize, cut: Option<(f64, f64)>, out: &mut Vec<Placed>) {
    for (at, item) in frame.items() {
        let at = origin + *at;
        match item {
            FrameItem::Group(group) => {
                let cut = if group.clip.is_some() {
                    Some((points(group.frame.width()), points(group.frame.height())))
                } else {
                    cut
                };
                walk(&group.frame, at, page, cut, out);
            }
            FrameItem::Image(image, size, _) => out.push(Placed {
                page,
                at: (points(at.x), points(at.y)),
                drawn: (points(size.x), points(size.y)),
                cut,
                pixels: (image.width() as u32, image.height() as u32),
            }),
            _ => {}
        }
    }
}
/// A4-like test page: 400 by 500 points with 35 points of margin, so a body of 330 by 430.
fn page() -> DocumentOptions {
    DocumentOptions::new(
        "pictures",
        "Pictures",
        PageSetup::new(400.0, 500.0, 35.0).unwrap(),
    )
}
/// The pictures of the document `build` makes, as laid out, and the warnings of its encoding.
fn laid_out(
    options: DocumentOptions,
    build: impl FnOnce(&mut DocumentBuilder) -> NodeId,
) -> (Vec<Placed>, Vec<String>) {
    let mut b = DocumentBuilder::new(Limits::default());
    let body = build(&mut b);
    let doc = b.finish(body, options).unwrap();
    let engine = engine();
    let (inputs, warnings) = encode::encode(&doc, &engine.limits).unwrap();
    let fonts = engine.fonts.select(doc.options().fonts()).unwrap();
    let world = ReportWorld::new(engine.source.clone(), inputs, fonts, date());
    let output = match typst::compile::<PagedDocument>(&world).output {
        Ok(output) => output,
        Err(errors) => panic!(
            "layout failed: {:?}",
            errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
        ),
    };
    let mut placed = Vec::new();
    for (number, page) in output.pages().iter().enumerate() {
        walk(&page.frame, Point::zero(), number, None, &mut placed);
    }
    (placed, warnings)
}
fn picture(data: &[u8], width: Option<f64>, height: Option<f64>, fit: ImageFit) -> Node {
    Node::Image {
        data: Arc::from(data),
        width,
        height,
        fit,
        alt: "picture".into(),
    }
}
/// A PNG of `width` by `height` pixels whose left half is red and right half blue.
fn png(width: u32, height: u32) -> Vec<u8> {
    let pixels = RgbImage::from_fn(width, height, |x, _| {
        if x < width / 2 {
            Rgb([200, 40, 40])
        } else {
            Rgb([40, 40, 200])
        }
    });
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(pixels)
        .write_with_encoder(PngEncoder::new(&mut out))
        .unwrap();
    out
}
/// A JPEG of `width` by `height` red pixels.
fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let pixels = RgbImage::from_pixel(width, height, Rgb([200, 40, 40]));
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(pixels)
        .write_with_encoder(JpegEncoder::new_with_quality(&mut out, 90))
        .unwrap();
    out
}
/// `jpeg` with an EXIF segment saying `orientation`, right after the start marker.
fn oriented(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
    exif.extend(orientation.to_be_bytes());
    exif.extend([0; 6]);
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, 0xE1]);
    out.extend((exif.len() as u16 + 2).to_be_bytes());
    out.extend(exif);
    out.extend(&jpeg[2..]);
    out
}
/// What the document's pictures are converted with in the tests that call the conversion directly.
fn embedded(
    data: Vec<u8>,
    printed: (f64, f64),
    fit: ImageFit,
    quality: PictureQuality,
) -> (Arc<[u8]>, ImageFormat) {
    let data: Arc<[u8]> = data.into();
    let info = probe_image(&data, &Limits::default()).unwrap();
    Pictures::default()
        .embed(&data, &info, printed, fit, quality, &Limits::default())
        .unwrap()
}
fn decoded(bytes: &[u8]) -> DynamicImage {
    image::load_from_memory(bytes).unwrap()
}

/// is embedded as stored; the maxima are in the evaluator tests)
#[test]
fn a_picture_prints_at_its_natural_size_or_follows_its_one_declared_side() {
    let data = png(200, 100);
    let (placed, warnings) = laid_out(page(), |b| {
        let children = vec![
            b.add(picture(&data, None, None, ImageFit::Contain))
                .unwrap(),
            b.add(picture(&data, Some(60.0), None, ImageFit::Contain))
                .unwrap(),
            b.add(picture(&data, None, Some(60.0), ImageFit::Contain))
                .unwrap(),
        ];
        b.add(Node::Flow { children }).unwrap()
    });
    assert!(warnings.is_empty(), "{warnings:?}");
    let drawn: Vec<_> = placed.iter().map(|p| (p.drawn, p.pixels, p.cut)).collect();
    assert_eq!(
        drawn,
        [
            ((150.0, 75.0), (200, 100), None),
            // 60 by 30 points need 167 by 84 pixels at 200 per inch; drawn at exactly 60 by 30
            ((60.0, 30.0), (167, 84), None),
            ((120.0, 60.0), (200, 100), None),
        ]
    );
    // the first picture starts at the top left corner of the page body
    assert_eq!(placed[0].at, (35.0, 35.0));
}

/// proportions, in the middle of the box; the box itself is the evaluator's)
/// outside, equally on both sides)
#[test]
fn a_picture_is_fitted_in_its_box_and_never_stretched() {
    // 200 by 100 pixels in a square box of 36 points
    let data = png(200, 100);
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&data, Some(36.0), Some(36.0), ImageFit::Contain))
            .unwrap()
    });
    // whole and twice as wide as high, 9 points below the top of its 36 point box
    assert_eq!(
        placed,
        [Placed {
            page: 0,
            at: (35.0, 44.0),
            drawn: (36.0, 18.0),
            cut: None,
            pixels: (100, 50),
        }]
    );
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&data, Some(36.0), Some(36.0), ImageFit::Cover))
            .unwrap()
    });
    // as high as the box and twice as wide, cut to the box, 18 points cut off on either side
    assert_eq!(
        placed,
        [Placed {
            page: 0,
            at: (17.0, 35.0),
            drawn: (72.0, 36.0),
            cut: Some((36.0, 36.0)),
            pixels: (200, 100),
        }]
    );
    // a tall picture in the same box: contain is drawn in the middle of its width
    let tall = png(100, 200);
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&tall, Some(36.0), Some(36.0), ImageFit::Contain))
            .unwrap()
    });
    assert_eq!(
        (placed[0].at, placed[0].drawn),
        ((44.0, 35.0), (18.0, 36.0))
    );
}

/// the width of its grid column or of the page body and into the height of the page body)
#[test]
fn a_picture_never_runs_past_the_page_or_its_column() {
    let wide = png(200, 100);
    let tall = png(100, 400);
    // 2000 points wide on a body of 330 by 430 points
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&wide, Some(2000.0), None, ImageFit::Contain))
            .unwrap()
    });
    assert_eq!(
        (placed[0].at, placed[0].drawn),
        ((35.0, 35.0), (330.0, 165.0))
    );
    // 4000 points high: as high as the body, a quarter as wide
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&tall, None, Some(4000.0), ImageFit::Contain))
            .unwrap()
    });
    assert_eq!(placed[0].drawn, (107.5, 430.0));
    assert_eq!(placed[0].page, 0);
    // a box keeps its proportions when it shrinks, and the cover crop with it
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&wide, Some(660.0), Some(660.0), ImageFit::Cover))
            .unwrap()
    });
    assert_eq!(
        (placed[0].drawn, placed[0].cut),
        ((660.0, 330.0), Some((330.0, 330.0)))
    );
    // two columns of 160 points with a gutter of 10: a 300 point picture takes its column's width
    let (placed, _) = laid_out(page(), |b| {
        let left = b
            .add(picture(&wide, Some(300.0), None, ImageFit::Contain))
            .unwrap();
        let right = b.add(Node::Text("beside".into())).unwrap();
        b.add(Node::Grid {
            columns: vec![1.0, 1.0],
            gutter: 10.0,
            cells: vec![left, right],
            align: CrossAlignment::Start,
        })
        .unwrap()
    });
    assert_eq!(
        (placed[0].at, placed[0].drawn),
        ((35.0, 35.0), (160.0, 80.0))
    );
    // a picture that fits is not touched
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&wide, Some(330.0), None, ImageFit::Contain))
            .unwrap()
    });
    assert_eq!(placed[0].drawn, (330.0, 165.0));
    // a table of a text column and a picture column: three rows of a 300 point picture stay inside
    // their column and the page body (its right edge is at 365 points), each as wide as the others
    // and twice as wide as high; a row is not split, so every picture is whole on its page
    let (placed, _) = laid_out(page(), |b| {
        let mut cells = Vec::new();
        for row in 0..3 {
            let text = format!("Line {row} with a description long enough to want room");
            cells.push(b.add(Node::Text(text)).unwrap());
            cells.push(
                b.add(picture(&wide, Some(300.0), None, ImageFit::Contain))
                    .unwrap(),
            );
        }
        b.add(Node::Table {
            columns: 2,
            header: Vec::new(),
            cells,
        })
        .unwrap()
    });
    assert_eq!(placed.len(), 3);
    for picture in &placed {
        let (wide, high) = picture.drawn;
        assert!(
            wide < 300.0 && (wide - 2.0 * high).abs() < 0.02,
            "{picture:?}"
        );
        assert!(
            picture.at.0 >= 35.0 && picture.at.0 + wide <= 365.01,
            "{picture:?}"
        );
        assert!(
            picture.at.1 >= 35.0 && picture.at.1 + high <= 465.01,
            "{picture:?}"
        );
        assert_eq!(picture.drawn, placed[0].drawn);
        assert_eq!(picture.at.0, placed[0].at.0);
    }
}

/// document's resolution is embedded with exactly the pixels it needs, one with fewer as stored,
/// and equal bytes at one size are converted once; one image object per picture in the PDF is
/// tested in `tests/assets.rs`)
#[test]
fn a_picture_is_embedded_at_the_resolution_of_the_document() {
    let data = png(400, 200);
    let at = |dpi: u16| {
        let quality = PictureQuality::new(dpi, 85).unwrap();
        let (placed, warnings) = laid_out(page().with_pictures(quality), |b| {
            b.add(picture(&data, Some(36.0), None, ImageFit::Contain))
                .unwrap()
        });
        assert!(warnings.is_empty(), "{warnings:?}");
        (placed[0].drawn, placed[0].pixels)
    };
    // 36 by 18 points are half an inch by a quarter: 100 by 50 pixels at 200 per inch
    assert_eq!(at(200), ((36.0, 18.0), (100, 50)));
    assert_eq!(at(72), ((36.0, 18.0), (36, 18)));
    assert_eq!(at(600), ((36.0, 18.0), (300, 150)));
    // the default of a document is 200
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&data, Some(36.0), None, ImageFit::Contain))
            .unwrap()
    });
    assert_eq!(placed[0].pixels, (100, 50));
    // a picture with no more pixels than its box needs is embedded as stored, never enlarged
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&data, Some(300.0), None, ImageFit::Contain))
            .unwrap()
    });
    assert_eq!(placed[0].pixels, (400, 200));
    let stored: Arc<[u8]> = data.clone().into();
    let info = probe_image(&stored, &Limits::default()).unwrap();
    let quality = PictureQuality::default();
    let limits = Limits::default();
    let mut pictures = Pictures::default();
    let large = (300.0, 150.0);
    let (kept, format) = pictures
        .embed(&stored, &info, large, ImageFit::Contain, quality, &limits)
        .unwrap();
    assert!(Arc::ptr_eq(&kept, &stored));
    assert_eq!(format, ImageFormat::Png);
    // the pixels a box needs: the whole picture inside it, or the picture filling it
    let need = |printed, fit| target_pixels(&info, printed, fit, 200);
    assert_eq!(need((36.0, 18.0), ImageFit::Contain), (100, 50));
    assert_eq!(need((36.0, 36.0), ImageFit::Contain), (100, 50));
    assert_eq!(need((36.0, 36.0), ImageFit::Cover), (200, 100));
    assert_eq!(need((0.1, 0.1), ImageFit::Contain), (1, 1));
    // equal bytes held by two nodes, printed at one size, are converted once
    let again: Arc<[u8]> = data.into();
    let small = (36.0, 18.0);
    let (first, _) = pictures
        .embed(&stored, &info, small, ImageFit::Contain, quality, &limits)
        .unwrap();
    let (second, _) = pictures
        .embed(&again, &info, small, ImageFit::Contain, quality, &limits)
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert!(!Arc::ptr_eq(&first, &stored));
    let (other, _) = pictures
        .embed(
            &again,
            &info,
            (18.0, 9.0),
            ImageFit::Contain,
            quality,
            &limits,
        )
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &other));
}

/// and a WebP without one to a JPEG; a PNG and a GIF without one to a PNG)
/// embedded as stored is decoded by the renderer, which this test does not read)
#[test]
fn a_converted_picture_keeps_its_transparency_and_its_kind() {
    let quality = PictureQuality::default();
    let small = (9.0, 9.0);
    // left half transparent, right half opaque
    let pixels = RgbaImage::from_fn(64, 64, |x, _| {
        Rgba([200, 40, 40, if x < 32 { 0 } else { 255 }])
    });
    let mut transparent = Vec::new();
    DynamicImage::ImageRgba8(pixels.clone())
        .write_with_encoder(PngEncoder::new(&mut transparent))
        .unwrap();
    let (bytes, format) = embedded(transparent, small, ImageFit::Contain, quality);
    assert_eq!(format, ImageFormat::Png);
    let picture = decoded(&bytes).to_rgba8();
    assert_eq!(picture.dimensions(), (25, 25));
    assert_eq!(picture.get_pixel(2, 12).0[3], 0);
    assert_eq!(picture.get_pixel(22, 12).0, [200, 40, 40, 255]);

    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .encode(pixels.as_raw(), 64, 64, ExtendedColorType::Rgba8)
        .unwrap();
    let (bytes, format) = embedded(webp, small, ImageFit::Contain, quality);
    assert_eq!(format, ImageFormat::Png);
    assert_eq!(decoded(&bytes).to_rgba8().get_pixel(2, 12).0[3], 0);

    let opaque = RgbImage::from_pixel(64, 64, Rgb([200, 40, 40]));
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .encode(opaque.as_raw(), 64, 64, ExtendedColorType::Rgb8)
        .unwrap();
    let (bytes, format) = embedded(webp, small, ImageFit::Contain, quality);
    assert_eq!(format, ImageFormat::Jpeg);
    assert_eq!(decoded(&bytes).dimensions(), (25, 25));

    let (bytes, format) = embedded(jpeg(64, 64), small, ImageFit::Contain, quality);
    assert_eq!(format, ImageFormat::Jpeg);
    let picture = decoded(&bytes);
    assert_eq!(picture.dimensions(), (25, 25));
    assert!(!picture.color().has_alpha());

    let (bytes, format) = embedded(png(64, 64), small, ImageFit::Contain, quality);
    assert_eq!(format, ImageFormat::Png);
    let picture = decoded(&bytes);
    assert!(!picture.color().has_alpha());
    // the halves are still red and blue after scaling
    assert_eq!(picture.to_rgb8().get_pixel(2, 12).0, [200, 40, 40]);
    assert_eq!(picture.to_rgb8().get_pixel(22, 12).0, [40, 40, 200]);

    // an animation of a red frame, then a blue one
    let mut gif = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut gif);
        for color in [[200, 40, 40, 255], [40, 40, 200, 255]] {
            let frame = RgbaImage::from_pixel(64, 64, Rgba(color));
            encoder.encode_frame(image::Frame::new(frame)).unwrap();
        }
    }
    let (bytes, format) = embedded(gif, small, ImageFit::Contain, quality);
    assert_eq!(format, ImageFormat::Png);
    let first = decoded(&bytes).to_rgba8();
    assert_eq!(first.dimensions(), (25, 25));
    let [red, _, blue, _] = first.get_pixel(12, 12).0;
    assert!(red > 150 && blue < 100, "{:?}", first.get_pixel(12, 12));
}

/// drawn as it is seen, embedded as stored or converted; the header half is in `erp-report`)
#[test]
fn a_turned_jpeg_prints_as_it_is_seen() {
    // stored 64 wide and 32 high, seen 32 wide and 64 high
    let data = oriented(&jpeg(64, 32), 6);
    let info = probe_image(&data, &Limits::default()).unwrap();
    assert_eq!(
        (info.width(), info.height(), info.orientation()),
        (32, 64, 6)
    );
    let (placed, warnings) = laid_out(page(), |b| {
        b.add(picture(&data, None, None, ImageFit::Contain))
            .unwrap()
    });
    assert!(warnings.is_empty(), "{warnings:?}");
    // 32 by 64 pixels are 24 by 48 points; the renderer turns the stored picture itself
    assert_eq!(
        (placed[0].drawn, placed[0].pixels),
        ((24.0, 48.0), (32, 64))
    );
    // converted: 4.5 by 9 points need 13 by 25 pixels, upright
    let quality = PictureQuality::default();
    let (bytes, format) = embedded(data.clone(), (4.5, 9.0), ImageFit::Contain, quality);
    assert_eq!(format, ImageFormat::Jpeg);
    assert_eq!(decoded(&bytes).dimensions(), (13, 25));
    let (placed, _) = laid_out(page(), |b| {
        b.add(picture(&data, Some(4.5), None, ImageFit::Contain))
            .unwrap()
    });
    assert_eq!((placed[0].drawn, placed[0].pixels), ((4.5, 9.0), (13, 25)));
}

/// its stored size, and one the decoder cannot read prints as an empty box with a warning naming
/// its kind; no CMYK picture the decoder reads is among the fixtures, so the conversion to RGB is
/// not tested)
#[test]
fn a_jpeg_the_decoder_cannot_read_is_a_named_placeholder() {
    // the header of a four-component frame, then nothing a decoder could read
    let mut data = vec![0xFF, 0xD8];
    data.extend([0xFF, 0xC0, 0x00, 0x14, 0x08, 0x00, 0x20, 0x00, 0x40, 0x04]);
    data.extend([0; 32]);
    let info = probe_image(&data, &Limits::default()).unwrap();
    assert_eq!(info.conversion(), Some("CMYK JPEG"));
    let (placed, warnings) = laid_out(page(), |b| {
        let broken = b
            .add(picture(&data, Some(48.0), Some(24.0), ImageFit::Contain))
            .unwrap();
        let fine = b
            .add(picture(&png(8, 8), None, None, ImageFit::Contain))
            .unwrap();
        b.add(Node::Flow {
            children: vec![broken, fine],
        })
        .unwrap()
    });
    let [warning] = warnings.as_slice() else {
        panic!("one warning, got {warnings:?}");
    };
    assert!(
        warning.starts_with("image node 0: the decoder cannot read this CMYK JPEG: ")
            && warning.ends_with("; an empty box is printed"),
        "{warning}"
    );
    // the picture after it still prints, below the 24 points of the empty box
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].drawn, (6.0, 6.0));
    assert!(placed[0].at.1 >= 35.0 + 24.0, "{:?}", placed[0]);
}
