// SPDX-License-Identifier: MIT OR Apache-2.0
//! Image admission and trusted vectors. Raster dimensions, the EXIF orientation of a JPEG and its
//! coding are read from the container header, so the pixel cap holds before any decoder runs; SVG bytes from users are never trusted (Decision 7).
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
/// How a picture sits in a box whose proportions differ from its own. Its proportions are kept
/// either way: a picture is never stretched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ImageFit {
    /// Scaled to fit inside the box and centred in it.
    #[default]
    Contain,
    /// Scaled to fill the box; what falls outside is cut off, equally on both sides.
    Cover,
}
/// Detected format and header dimensions (zero for SVG, which has no pixel grid).
#[derive(Debug, Clone, Copy)]
pub struct ImageInfo {
    format: ImageFormat,
    width: u32,
    height: u32,
    orientation: u8,
    conversion: Option<&'static str>,
}
impl ImageInfo {
    pub fn format(&self) -> ImageFormat {
        self.format
    }
    /// Pixels across as the picture is seen: after the EXIF orientation of a JPEG.
    pub fn width(&self) -> u32 {
        self.width
    }
    /// Pixels down as the picture is seen: after the EXIF orientation of a JPEG.
    pub fn height(&self) -> u32 {
        self.height
    }
    /// The EXIF orientation of a JPEG, `1..=8`; `1` (stored as seen) for every other picture.
    pub fn orientation(&self) -> u8 {
        self.orientation
    }
    /// Why the stored bytes cannot be embedded as they are, as the name of their kind (a CMYK
    /// JPEG, a lossless JPEG): a renderer decodes such a picture and encodes it again, or prints
    /// a placeholder naming the kind. `None` for a picture every PDF reader shows.
    pub fn conversion(&self) -> Option<&'static str> {
        self.conversion
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
/// The EXIF orientation (`1..=8`) in the payload of an APP1 segment: tag 0x0112 of the first
/// image directory. `None` when the payload is not EXIF, is cut short or holds no such tag.
fn exif_orientation(payload: &[u8]) -> Option<u8> {
    let tiff = payload.strip_prefix(b"Exif\0\0")?;
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let short = |at: usize| -> Option<u16> {
        let bytes: [u8; 2] = tiff.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let long = |at: usize| -> Option<u32> {
        let bytes: [u8; 4] = tiff.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    (short(2)? == 42).then_some(())?;
    let directory = usize::try_from(long(4)?).ok()?;
    // At most 65535 entries of 12 bytes; a read past the payload ends the walk.
    for entry in 0..usize::from(short(directory)?) {
        let at = directory
            .checked_add(2)?
            .checked_add(entry.checked_mul(12)?)?;
        if short(at)? == 0x0112 {
            // one SHORT: it sits in the first two bytes of the value field
            let value = short(at.checked_add(8)?)?;
            return u8::try_from(value).ok().filter(|v| (1..=8).contains(v));
        }
    }
    None
}
/// What the header of a JPEG says: the stored frame size, the EXIF orientation and whether the
/// coding is one a PDF reader may not show.
struct Jpeg {
    width: u32,
    height: u32,
    orientation: u8,
    conversion: Option<&'static str>,
}
fn jpeg(d: &[u8]) -> Option<Jpeg> {
    let mut at = 2;
    let mut orientation = 1;
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
            let (precision, components) = (*d.get(at + 4)?, *d.get(at + 9)?);
            // Baseline, extended and progressive Huffman coding of 8-bit grey or colour samples is
            // what every PDF reader decodes; anything else is converted before it is embedded.
            let conversion = match (marker, precision, components) {
                (0xC0..=0xC2, 8, 1 | 3) => None,
                (_, _, 4) => Some("CMYK JPEG"),
                (0xC0..=0xC2, _, 1 | 3) => Some("JPEG with samples wider than 8 bits"),
                (0xC0..=0xC2, ..) => Some("JPEG with an unusual number of colour components"),
                (0xC3 | 0xC7 | 0xCB | 0xCF, ..) => Some("lossless JPEG"),
                (0xC9..=0xCE, ..) => Some("arithmetic-coded JPEG"),
                _ => Some("hierarchical JPEG"),
            };
            return Some(Jpeg {
                width: be16(d, at + 7)?,
                height: be16(d, at + 5)?,
                orientation,
                conversion,
            });
        }
        if (0xD8..=0xDA).contains(&marker) {
            return None; // no frame header before the image data
        }
        let length = usize::try_from(be16(d, at + 2)?).ok()?;
        if marker == 0xE1
            && let Some(found) = d.get(at + 4..at + 2 + length).and_then(exif_orientation)
        {
            orientation = found;
        }
        at = at.checked_add(2)?.checked_add(length)?;
    }
}
/// Skip GIF data sub-blocks starting at `at`; the position after the zero-length terminator.
fn gif_skip_sub_blocks(d: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = usize::from(*d.get(at)?);
        at += 1;
        if len == 0 {
            return Some(at);
        }
        at += len;
    }
}
/// The logical screen of a GIF after walking its blocks to every image descriptor, without
/// decoding. `Some(Err(..))` when a frame is empty or lies outside the screen (the decoder would
/// allocate the frame's own size, which the pixel cap never saw); `None` when the structure is
/// truncated, malformed or holds no frame. The same walk as the ORM's image-field probe
/// (`erp_orm::image`); the two crates share no dependency to keep one copy in.
fn gif(d: &[u8]) -> Option<Result<(u32, u32), ReportError>> {
    let (width, height) = (le16(d, 6)?, le16(d, 8)?);
    let colour_table = |packed: u8| {
        if packed & 0x80 == 0 {
            0
        } else {
            3usize << ((packed & 0x07) + 1)
        }
    };
    let mut at = 13 + colour_table(*d.get(10)?);
    let mut frames = 0usize;
    loop {
        match *d.get(at)? {
            0x3B => break,
            0x21 => at = gif_skip_sub_blocks(d, at + 2)?,
            0x2C => {
                let (left, top) = (le16(d, at + 1)?, le16(d, at + 3)?);
                let (w, h) = (le16(d, at + 5)?, le16(d, at + 7)?);
                if w == 0 || h == 0 || left + w > width || top + h > height {
                    return Some(Err(image(
                        "a GIF frame is empty or lies outside its logical screen",
                    )));
                }
                at += 10 + colour_table(*d.get(at + 9)?) + 1;
                at = gif_skip_sub_blocks(d, at)?;
                frames += 1;
            }
            _ => return None,
        }
    }
    (frames > 0).then_some(Ok((width, height)))
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
    let (mut orientation, mut conversion) = (1, None);
    let (format, size) = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        (ImageFormat::Png, png(data))
    } else if data.starts_with(&[0xFF, 0xD8]) {
        let header = jpeg(data);
        if let Some(header) = &header {
            (orientation, conversion) = (header.orientation, header.conversion);
        }
        // Orientations 5 to 8 turn the picture a quarter: it is seen as high as it is stored wide.
        let size = header.map(|header| match header.orientation {
            5..=8 => (header.height, header.width),
            _ => (header.width, header.height),
        });
        (ImageFormat::Jpeg, size)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        (ImageFormat::Gif, gif(data).transpose()?)
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        (ImageFormat::WebP, webp(data))
    } else if looks_like_svg(data) {
        return Ok(ImageInfo {
            format: ImageFormat::UntrustedSvg,
            width: 0,
            height: 0,
            orientation: 1,
            conversion: None,
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
        orientation,
        conversion,
    })
}
/// The value of one base64 character of the standard alphabet.
fn sextet(c: u8) -> Option<u32> {
    Some(u32::from(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    }))
}
/// Strict standard base64: the alphabet only, padded to a multiple of four, no white space.
fn base64(text: &[u8]) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let padding = text.iter().rev().take_while(|c| **c == b'=').count();
    if padding > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    // The length is a multiple of four, so the remainder is empty.
    for (index, group) in text.as_chunks::<4>().0.iter().enumerate() {
        let last = (index + 1) * 4 == text.len();
        let kept = if last { 4 - padding } else { 4 };
        let mut bits: u32 = 0;
        for c in &group[..kept] {
            bits = (bits << 6) | sextet(*c)?;
        }
        let bytes = (bits << (6 * (4 - kept))).to_be_bytes();
        // Bits the padding drops must be zero, so one image has one spelling.
        if bytes[kept..].iter().any(|byte| *byte != 0) {
            return None;
        }
        out.extend_from_slice(&bytes[1..kept]);
    }
    Some(out)
}
/// The bytes of an embedded image (`data:image/<type>;base64,<data>`). The declared type is not
/// read: the format comes from the bytes when they are admitted. The size is checked against
/// `limits` before anything is decoded.
pub(crate) fn embedded_bytes(value: &str, limits: &Limits) -> Result<Vec<u8>, ReportError> {
    let malformed = || image("an embedded image must be `data:image/<type>;base64,<data>`");
    let (header, data) = value.split_once(',').ok_or_else(malformed)?;
    let encoding = header.rsplit(';').next().unwrap_or_default();
    if !crate::validate::embedded_image(value) || !encoding.eq_ignore_ascii_case("base64") {
        return Err(malformed());
    }
    limits.check(LimitKind::ImageBytes, data.len() / 4 * 3)?;
    base64(data.as_bytes()).ok_or_else(|| image("the data of an embedded image is not base64"))
}
/// A vector graphic whose SVG markup ERPForAll writes itself from numbers, so the in-process engine
/// may parse it. There is no constructor from SVG text: caller bytes go through `Node::Image`. The
/// numbers stay with it: they, never the markup, are what crosses to a render worker.
#[derive(Debug, Clone, PartialEq)]
pub struct Vector {
    svg: Arc<[u8]>,
    width: f64,
    height: f64,
    rectangles: Arc<[[f64; 4]]>,
    color: [u8; 3],
}
impl Vector {
    /// Filled rectangles `[x, y, width, height]` inside a `width` × `height` box, in one RGB colour:
    /// the shape of every 1D barcode and QR code. A rectangle is positive in size and lies inside
    /// the box; there are at most `vector_rectangles` of them (the hard bound of that limit).
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
        Limits::default().check(LimitKind::VectorRectangles, rectangles.len())?;
        if rectangles.iter().flatten().any(|v| !v.is_finite()) {
            return Err(crate::error::invalid("vector rectangles must be finite"));
        }
        // The slack forgives the rounding of an edge that is computed, not a rectangle outside.
        const SLACK: f64 = 1e-6;
        let inside = |[x, y, w, h]: &[f64; 4]| {
            *x >= 0.0
                && *y >= 0.0
                && *w > 0.0
                && *h > 0.0
                && x + w <= width + SLACK
                && y + h <= height + SLACK
        };
        if !rectangles.iter().all(inside) {
            return Err(crate::error::invalid(
                "vector rectangles must have a positive size and lie inside the vector box",
            ));
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
            rectangles: rectangles.into(),
            color,
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
    /// The rectangles the vector was made from, `[x, y, width, height]` each.
    pub fn rectangle_list(&self) -> &[[f64; 4]] {
        &self.rectangles
    }
    pub fn color(&self) -> [u8; 3] {
        self.color
    }
    /// Bytes the vector holds: its markup and its rectangles.
    pub(crate) fn payload(&self) -> usize {
        self.svg
            .len()
            .saturating_add(std::mem::size_of_val(&*self.rectangles))
    }
}
