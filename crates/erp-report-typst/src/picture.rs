// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pictures before layout. A picture with more pixels than its printed size needs at the document's
//! resolution is decoded, scaled down and encoded once; one whose coding a PDF reader may not show
//! (a CMYK JPEG) is converted to RGB. A picture that needs neither is embedded as stored. This runs
//! where the renderer runs: on a served node, inside a render worker.
use erp_report::{ImageFit, ImageFormat, ImageInfo, LimitKind, Limits, PictureQuality};
use image::{
    DynamicImage, ImageDecoder, ImageReader,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
    imageops::FilterType,
    metadata::Orientation,
};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io::Cursor, sync::Arc};

/// Points in an inch: a resolution is pixels per inch, a printed size is points.
const POINTS_PER_INCH: f64 = 72.0;
/// Part of a pixel a need may exceed a whole number by and still be that number (binary rounding).
const PIXEL_ALLOWANCE: f64 = 1e-6;
/// Bytes of the widest pixel a decoder produces (16-bit RGBA), for the decoder's memory bound.
const BYTES_PER_PIXEL: u64 = 8;

/// The bytes embedded for a picture and their format, or why the picture cannot be printed.
type Embedded = Result<(Arc<[u8]>, ImageFormat), String>;

/// The pictures of one document: each content is converted once per pixel size, so the same logo
/// on every page, or the same product picture on several lines, is encoded and embedded once.
#[derive(Default)]
pub(crate) struct Pictures {
    converted: HashMap<([u8; 32], u32, u32), Embedded>,
    /// One byte buffer per content: nodes that hold equal bytes hand the renderer the same value.
    stored: HashMap<[u8; 32], Arc<[u8]>>,
}

/// Pixels of the whole picture when the part shown in a `wide` × `high` point box has `dpi` pixels
/// per inch. `Contain` shows the whole picture inside the box, `Cover` fills the box with it.
pub(crate) fn target_pixels(
    info: &ImageInfo,
    (wide, high): (f64, f64),
    fit: ImageFit,
    dpi: u16,
) -> (u32, u32) {
    let (across, down) = (f64::from(info.width()), f64::from(info.height()));
    let (x, y) = (wide / across, high / down);
    let points_per_pixel = if fit == ImageFit::Cover {
        x.max(y)
    } else {
        x.min(y)
    };
    let scale = points_per_pixel * f64::from(dpi) / POINTS_PER_INCH;
    // Rounded up to a whole pixel; the small allowance keeps a need that is a whole number in exact
    // arithmetic (100 pixels) from becoming the next one through binary rounding. A box is at most
    // 14400 points and a resolution 600: the product fits `u32` many times over.
    let pixels = |side: f64| {
        (side * scale - PIXEL_ALLOWANCE)
            .ceil()
            .clamp(1.0, f64::from(u32::MAX)) as u32
    };
    (pixels(across), pixels(down))
}

impl Pictures {
    /// What to embed for `data` printed in a `wide` × `high` point box. The stored bytes when the
    /// picture has no more pixels than the box needs and every reader shows its coding; otherwise
    /// the picture scaled down to the pixels the box needs, encoded once. `Err` says why the
    /// picture cannot be printed: the caller prints a placeholder and returns the reason.
    pub(crate) fn embed(
        &mut self,
        data: &Arc<[u8]>,
        info: &ImageInfo,
        printed: (f64, f64),
        fit: ImageFit,
        quality: PictureQuality,
        limits: &Limits,
    ) -> Embedded {
        let content: [u8; 32] = Sha256::digest(data).into();
        let (across, down) = target_pixels(info, printed, fit, quality.dpi());
        let shrink = across < info.width() && down < info.height();
        if !shrink && info.conversion().is_none() {
            let stored = self.stored.entry(content).or_insert_with(|| data.clone());
            return Ok((stored.clone(), info.format()));
        }
        let size = if shrink {
            (across, down)
        } else {
            (info.width(), info.height())
        };
        self.converted
            .entry((content, size.0, size.1))
            .or_insert_with(|| convert(data, info, size, quality, limits))
            .clone()
    }
}

/// `data` decoded, turned as its EXIF orientation says, scaled to `size` pixels (the size of the
/// picture as it is seen) and encoded: with an alpha channel as PNG, so transparency is kept; a
/// JPEG or WebP without one as JPEG at the document's quality; a PNG or GIF without one as PNG.
fn convert(
    data: &[u8],
    info: &ImageInfo,
    size: (u32, u32),
    quality: PictureQuality,
    limits: &Limits,
) -> Embedded {
    let kind = info.conversion().unwrap_or("picture");
    let failed = |error: image::ImageError| format!("the decoder cannot read this {kind}: {error}");
    let format = match info.format() {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::WebP => image::ImageFormat::WebP,
        _ => return Err(format!("no decoder reads this {kind}")),
    };
    let mut reader = ImageReader::with_format(Cursor::new(data), format);
    // The pixel limit was checked on the header; the decoder may allocate no more than those pixels.
    let mut bounds = image::Limits::no_limits();
    bounds.max_alloc =
        Some((limits.get(LimitKind::ImagePixels) as u64).saturating_mul(BYTES_PER_PIXEL));
    // The decoder is also held to the stored size of the header: a body that declares more fails.
    let (stored_wide, stored_high) = match info.orientation() {
        5..=8 => (info.height(), info.width()),
        _ => (info.width(), info.height()),
    };
    bounds.max_image_width = Some(stored_wide);
    bounds.max_image_height = Some(stored_high);
    reader.limits(bounds);
    let mut decoder = reader.into_decoder().map_err(failed)?;
    // An unreadable orientation prints the picture as stored: its pixels are still right.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut picture = DynamicImage::from_decoder(decoder).map_err(failed)?;
    picture.apply_orientation(orientation);
    // The header gave the size for the layout; a decoder that disagrees with it (an orientation in
    // a format whose header the probe does not read for one) keeps its own proportions.
    let (across, down) = if (picture.width() >= picture.height()) == (size.0 >= size.1) {
        size
    } else {
        (size.1, size.0)
    };
    if (picture.width(), picture.height()) != (across, down) {
        picture = picture.resize_exact(across, down, FilterType::CatmullRom);
    }
    let color = picture.color();
    let lossy = matches!(info.format(), ImageFormat::Jpeg | ImageFormat::WebP);
    let mut out = Vec::new();
    let format = if color.has_alpha() || !lossy {
        // A picture already in one of the three layouts is encoded as it is, with no second copy.
        let picture = match picture {
            DynamicImage::ImageRgba8(_)
            | DynamicImage::ImageRgb8(_)
            | DynamicImage::ImageLuma8(_) => picture,
            _ if color.has_alpha() => DynamicImage::ImageRgba8(picture.to_rgba8()),
            _ if color.has_color() => DynamicImage::ImageRgb8(picture.to_rgb8()),
            _ => DynamicImage::ImageLuma8(picture.to_luma8()),
        };
        picture
            .write_with_encoder(PngEncoder::new(&mut out))
            .map_err(failed)?;
        ImageFormat::Png
    } else {
        let picture = if color.has_color() {
            DynamicImage::ImageRgb8(picture.to_rgb8())
        } else {
            DynamicImage::ImageLuma8(picture.to_luma8())
        };
        let encoder = JpegEncoder::new_with_quality(&mut out, quality.jpeg_quality());
        picture.write_with_encoder(encoder).map_err(failed)?;
        ImageFormat::Jpeg
    };
    Ok((out.into(), format))
}

#[cfg(test)]
mod tests {
    use super::*;
    use erp_report::probe_image;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(image::RgbImage::new(width, height))
            .write_with_encoder(PngEncoder::new(&mut out))
            .unwrap();
        out
    }

    /// header said)
    #[test]
    fn the_decoder_is_bound_by_the_probed_size() {
        let limits = Limits::default();
        let (small, large) = (png(4, 4), png(8, 8));
        let info = probe_image(&small, &limits).unwrap();
        let size = (info.width(), info.height());
        assert!(convert(&small, &info, size, PictureQuality::default(), &limits).is_ok());
        let error = convert(&large, &info, size, PictureQuality::default(), &limits).unwrap_err();
        assert!(
            error.starts_with("the decoder cannot read this "),
            "{error}"
        );
    }
}
