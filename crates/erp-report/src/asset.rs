// SPDX-License-Identifier: MIT OR Apache-2.0
//! Image admission and trusted vectors. Raster dimensions are read from the container header so the
//! pixel cap holds before any decoder runs; SVG bytes from users are never trusted (Decision 7).
use crate::{LimitKind, Limits, ReportError};
use std::sync::Arc;

/// Format of admitted image bytes, detected from their content, never from a caller's claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    /// User or uploaded SVG: printed by the in-process engine as a placeholder with a warning.
    UntrustedSvg,
}
/// Detected format and header dimensions (zero for SVG, which has no pixel grid).
#[derive(Debug, Clone, Copy)]
pub struct ImageInfo {
    format: ImageFormat,
    width: u32,
    height: u32,
}
impl ImageInfo {
    pub fn format(&self) -> ImageFormat {
        self.format
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
}
fn image(reason: &str) -> ReportError {
    ReportError::Image {
        reason: reason.into(),
    }
}
fn be16(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([
        *d.get(at)?,
        *d.get(at + 1)?,
    ])))
}
fn le16(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes([
        *d.get(at)?,
        *d.get(at + 1)?,
    ])))
}
fn le24(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *d.get(at)?,
        *d.get(at + 1)?,
        *d.get(at + 2)?,
        0,
    ]))
}
fn png(d: &[u8]) -> Option<(u32, u32)> {
    (d.get(12..16)? == b"IHDR").then_some(())?;
    let w = u32::from_be_bytes(d.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(d.get(20..24)?.try_into().ok()?);
    Some((w, h))
}
fn jpeg(d: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        if *d.get(at)? != 0xFF {
            return None;
        }
        let marker = *d.get(at + 1)?;
        if marker == 0xFF {
            at += 1; // fill byte
            continue;
        }
        // SOF0..SOF15 carry the frame size; C4 (DHT), C8 (JPG) and CC (DAC) share the range.
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some((be16(d, at + 7)?, be16(d, at + 5)?));
        }
        if (0xD8..=0xDA).contains(&marker) {
            return None; // no frame header before the image data
        }
        at = at
            .checked_add(2)?
            .checked_add(usize::try_from(be16(d, at + 2)?).ok()?)?;
    }
}
fn webp(d: &[u8]) -> Option<(u32, u32)> {
    match d.get(12..16)? {
        b"VP8X" => Some((le24(d, 24)? + 1, le24(d, 27)? + 1)),
        b"VP8L" => {
            (*d.get(20)? == 0x2F).then_some(())?;
            let bits = u32::from_le_bytes(d.get(21..25)?.try_into().ok()?);
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        b"VP8 " => {
            (d.get(23..26)? == [0x9D, 0x01, 0x2A]).then_some(())?;
            Some((le16(d, 26)? & 0x3FFF, le16(d, 28)? & 0x3FFF))
        }
        _ => None,
    }
}
fn looks_like_svg(d: &[u8]) -> bool {
    let head = &d[..d.len().min(4096)];
    let start = head.iter().position(|c| !c.is_ascii_whitespace());
    start.is_some_and(|s| head[s] == b'<') && head.windows(4).any(|w| w == b"<svg")
}
/// Detect the format and read the header dimensions of image bytes under `limits`, without decoding.
/// Encoded size is checked first, then the pixel count (`width * height`, refused on overflow).
pub fn probe_image(data: &[u8], limits: &Limits) -> Result<ImageInfo, ReportError> {
    limits.check(LimitKind::ImageBytes, data.len())?;
    let (format, size) = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        (ImageFormat::Png, png(data))
    } else if data.starts_with(&[0xFF, 0xD8]) {
        (ImageFormat::Jpeg, jpeg(data))
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        (ImageFormat::Gif, le16(data, 6).zip(le16(data, 8)))
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        (ImageFormat::WebP, webp(data))
    } else if looks_like_svg(data) {
        return Ok(ImageInfo {
            format: ImageFormat::UntrustedSvg,
            width: 0,
            height: 0,
        });
    } else {
        return Err(image("image bytes are not PNG, JPEG, GIF, WebP or SVG"));
    };
    let (width, height) = size.ok_or_else(|| image("image header is malformed or truncated"))?;
    if width == 0 || height == 0 {
        return Err(image("image header declares an empty image"));
    }
    let pixels = width
        .checked_mul(height)
        .and_then(|p| usize::try_from(p).ok())
        .ok_or(ReportError::DocumentLimit {
            kind: LimitKind::ImagePixels,
            actual: usize::MAX,
            max: limits.get(LimitKind::ImagePixels),
        })?;
    limits.check(LimitKind::ImagePixels, pixels)?;
    Ok(ImageInfo {
        format,
        width,
        height,
    })
}
/// A vector graphic whose SVG markup ERPForAll writes itself from numbers, so the in-process engine
/// may parse it. There is no constructor from SVG text: caller bytes go through `Node::Image`.
#[derive(Debug, Clone)]
pub struct Vector {
    svg: Arc<[u8]>,
    width: f64,
    height: f64,
}
impl Vector {
    /// Filled rectangles `[x, y, width, height]` inside a `width` × `height` box, in one RGB colour:
    /// the shape of every 1D barcode and QR code.
    pub fn rectangles(
        width: f64,
        height: f64,
        rectangles: &[[f64; 4]],
        color: [u8; 3],
    ) -> Result<Self, ReportError> {
        let size_ok = |v: f64| v.is_finite() && v > 0.0 && v <= 14400.0;
        if !size_ok(width) || !size_ok(height) {
            return Err(crate::error::invalid(
                "vector box must be finite and in (0, 14400] units each way",
            ));
        }
        if rectangles.iter().flatten().any(|v| !v.is_finite()) {
            return Err(crate::error::invalid("vector rectangles must be finite"));
        }
        use std::fmt::Write;
        let [r, g, b] = color;
        let mut svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\" width=\"{width}\" height=\"{height}\"><path fill=\"#{r:02x}{g:02x}{b:02x}\" d=\""
        );
        for [x, y, w, h] in rectangles {
            // Writing to a String cannot fail.
            let _ = write!(svg, "M{x} {y}h{w}v{h}h{}z", -w);
        }
        svg.push_str("\"/></svg>");
        Ok(Self {
            svg: svg.into_bytes().into(),
            width,
            height,
        })
    }
    /// The generated SVG markup.
    pub fn svg(&self) -> &Arc<[u8]> {
        &self.svg
    }
    pub fn width(&self) -> f64 {
        self.width
    }
    pub fn height(&self) -> f64 {
        self.height
    }
}
