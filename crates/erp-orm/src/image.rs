// SPDX-License-Identifier: MIT OR Apache-2.0
//! Image fields (B6, column-backed mode): what an `Image` field accepts and stores.
//!
//! Adopted as is. Modified: the type comes from the bytes and SVG is refused for every
//! user; the size is read from the file header before any pixel is decoded; the pixel limit is a
//! bounded setting that no declaration switches off; a bounded PNG or JPEG is encoded again so the
//! metadata of the uploaded file is not stored.
//!
//! The limits are database settings (system parameters, declared by addon `base`), and one node
//! setting:
//!
//! | Setting | Default | Floor | Hard bound |
//! |---|---|---|---|
//! | `base.image_max_pixels` | 40,000,000 | 1,000,000 | 100,000,000 |
//! | `base.image_max_bytes` | 10,485,760 | 65,536 | 134,217,728 |
//! | `base.image_jpeg_quality` | 95 | 1 | 100 |
//! | `base.image_decode_timeout_secs` | 10 | 1 | 120 |
//! | `attachments.image_decode_permits` (node configuration) | 2 | 1 | 64 |
//!
//! Decoding and scaling run on the blocking pool, at most `attachments.image_decode_permits` at
//! once in this process, and a write waits for a permit and its decode no longer than
//! `base.image_decode_timeout_secs` (B6). A decode that has started keeps its permit
//! until it ends, also when its caller stopped waiting.

use std::io::Cursor;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use erp_fields::convert::FieldRef;
use erp_fields::{Blob, Cell, Error, ErrorKind, FieldType, ImageBounds, Result};
use erp_registry::{FieldSpec, ModelSpec};

use crate::Transaction;

/// System parameter: the most pixels (`width * height`) an image field accepts.
pub const IMAGE_MAX_PIXELS_KEY: &str = "base.image_max_pixels";
/// System parameter: the most bytes of a picture an image field accepts.
pub const IMAGE_MAX_BYTES_KEY: &str = "base.image_max_bytes";
/// System parameter: the quality of a JPEG the server encodes.
pub const IMAGE_JPEG_QUALITY_KEY: &str = "base.image_jpeg_quality";
/// System parameter: how long a write waits for a decode permit and its decode, in seconds.
pub const IMAGE_DECODE_TIMEOUT_KEY: &str = "base.image_decode_timeout_secs";
/// Node configuration (`[attachments]`): the decodes this process runs at once.
pub const IMAGE_DECODE_PERMITS_KEY: &str = "attachments.image_decode_permits";

/// One integer system parameter of the image limits, with its default, floor and hard bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageParameter {
    /// The `ir.config_parameter` key.
    pub key: &'static str,
    /// The label of its declaration.
    pub label: &'static str,
    /// The value of an absent or empty parameter.
    pub default: u64,
    /// The smallest accepted value.
    pub min: u64,
    /// The largest accepted value.
    pub max: u64,
}

impl ImageParameter {
    fn parse(&self, text: Option<&str>) -> Result<u64> {
        let value = match text.map(str::trim) {
            None | Some("") => return Ok(self.default),
            Some(text) => text.parse::<u64>().map_err(|_| {
                Error::validation(format!(
                    "{} must be a whole number, got a malformed value",
                    self.key
                ))
            })?,
        };
        self.check(value)
    }

    fn check(&self, value: u64) -> Result<u64> {
        if (self.min..=self.max).contains(&value) {
            Ok(value)
        } else {
            Err(Error::validation(format!(
                "{} is outside the allowed range {}..={}: {value}",
                self.key, self.min, self.max
            )))
        }
    }
}

/// The image limits of one database. Every value has a default, a floor and a hard bound; a value
/// outside them is refused by name and zero never means unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImagePolicy {
    max_pixels: u64,
    max_bytes: u64,
    jpeg_quality: u8,
    decode_timeout_secs: u64,
}

impl Default for ImagePolicy {
    fn default() -> Self {
        Self {
            max_pixels: Self::DEFAULT_MAX_PIXELS,
            max_bytes: Self::MAX_BYTES.default,
            jpeg_quality: Self::DEFAULT_JPEG_QUALITY,
            decode_timeout_secs: Self::DECODE_TIMEOUT.default,
        }
    }
}

impl ImagePolicy {
    /// Default of `base.image_max_pixels` (40 megapixels).
    pub const DEFAULT_MAX_PIXELS: u64 = 40_000_000;
    /// Floor of `base.image_max_pixels`.
    pub const MIN_MAX_PIXELS: u64 = 1_000_000;
    /// Hard bound of `base.image_max_pixels`.
    pub const MAX_MAX_PIXELS: u64 = 100_000_000;
    /// Default of `base.image_jpeg_quality`.
    pub const DEFAULT_JPEG_QUALITY: u8 = 95;
    /// Floor of `base.image_jpeg_quality`.
    pub const MIN_JPEG_QUALITY: u8 = 1;
    /// Hard bound of `base.image_jpeg_quality`.
    pub const MAX_JPEG_QUALITY: u8 = 100;

    /// `base.image_max_pixels`.
    pub const MAX_PIXELS: ImageParameter = ImageParameter {
        key: IMAGE_MAX_PIXELS_KEY,
        label: "Maximum pixels of an image (width x height)",
        default: Self::DEFAULT_MAX_PIXELS,
        min: Self::MIN_MAX_PIXELS,
        max: Self::MAX_MAX_PIXELS,
    };
    /// `base.image_max_bytes`. The default is the most bytes of a picture a report prints, so a
    /// stored logo always prints; the hard bound is the default attachment file limit.
    pub const MAX_BYTES: ImageParameter = ImageParameter {
        key: IMAGE_MAX_BYTES_KEY,
        label: "Maximum bytes of an image",
        default: 10 * 1024 * 1024,
        min: 64 * 1024,
        max: 128 * 1024 * 1024,
    };
    /// `base.image_jpeg_quality`.
    pub const JPEG_QUALITY: ImageParameter = ImageParameter {
        key: IMAGE_JPEG_QUALITY_KEY,
        label: "JPEG quality of an image the server encodes",
        default: Self::DEFAULT_JPEG_QUALITY as u64,
        min: Self::MIN_JPEG_QUALITY as u64,
        max: Self::MAX_JPEG_QUALITY as u64,
    };
    /// `base.image_decode_timeout_secs`; the hard bound is the default RPC deadline.
    pub const DECODE_TIMEOUT: ImageParameter = ImageParameter {
        key: IMAGE_DECODE_TIMEOUT_KEY,
        label: "Seconds a write waits for an image decode",
        default: 10,
        min: 1,
        max: 120,
    };
    /// Every parameter of the policy, in declaration order.
    pub const PARAMETERS: [ImageParameter; 4] = [
        Self::MAX_PIXELS,
        Self::MAX_BYTES,
        Self::JPEG_QUALITY,
        Self::DECODE_TIMEOUT,
    ];

    /// A policy from the stored text of the parameters, looked up by key. An absent or empty
    /// parameter is its default; anything that is not a whole number inside the range is refused
    /// by name.
    ///
    /// # Errors
    /// A validation error naming the parameter.
    pub fn from_parameters<'a>(value: impl Fn(&str) -> Option<&'a str>) -> Result<Self> {
        let get = |parameter: ImageParameter| parameter.parse(value(parameter.key));
        let quality = get(Self::JPEG_QUALITY)?;
        Ok(Self {
            max_pixels: get(Self::MAX_PIXELS)?,
            max_bytes: get(Self::MAX_BYTES)?,
            // inside 1..=100 after the range check
            jpeg_quality: u8::try_from(quality)
                .map_err(|_| Error::programming("image JPEG quality bound exceeds u8"))?,
            decode_timeout_secs: get(Self::DECODE_TIMEOUT)?,
        })
    }

    /// The most pixels an image may have.
    pub fn max_pixels(&self) -> u64 {
        self.max_pixels
    }

    /// The most bytes a picture may have.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// The quality of a JPEG the server encodes.
    pub fn jpeg_quality(&self) -> u8 {
        self.jpeg_quality
    }

    /// How long a write waits for a decode permit and its decode.
    pub fn decode_timeout(&self) -> Duration {
        Duration::from_secs(self.decode_timeout_secs)
    }
}

/// The decodes one process runs at once (B6 and S4). A counting semaphore that a
/// synchronous admission step can wait on with a deadline.
#[derive(Debug)]
pub struct DecodePool {
    /// (permits, in use)
    state: Mutex<(u32, u32)>,
    freed: Condvar,
}

/// One held decode permit; released when dropped, also when the decode panics.
struct DecodePermit(&'static DecodePool);

impl Drop for DecodePermit {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.1 -= 1;
        self.0.freed.notify_one();
    }
}

/// Why a bounded decode did not give a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unfinished {
    /// No permit became free before the deadline.
    NoPermit(u32),
    /// The decode did not end before the deadline.
    TimedOut,
    /// The decoder stopped without a result (it panicked).
    Stopped,
}

static DECODE_POOL: DecodePool = DecodePool::new(DecodePool::DEFAULT_PERMITS);

impl DecodePool {
    /// Default of `attachments.image_decode_permits`.
    pub const DEFAULT_PERMITS: u32 = 2;
    /// Floor and hard bound of `attachments.image_decode_permits`.
    pub const PERMITS_RANGE: (u32, u32) = (1, 64);

    const fn new(permits: u32) -> Self {
        Self {
            state: Mutex::new((permits, 0)),
            freed: Condvar::new(),
        }
    }

    /// The pool of this process.
    pub fn global() -> &'static Self {
        &DECODE_POOL
    }

    /// Set the permits of this process from the node configuration. A decode that already holds
    /// a permit keeps it; the new count applies to the next wait.
    ///
    /// # Errors
    /// A validation error naming `attachments.image_decode_permits` for a value outside its range.
    pub fn set_permits(&self, permits: u32) -> Result<()> {
        let (low, high) = Self::PERMITS_RANGE;
        if !(low..=high).contains(&permits) {
            return Err(Error::validation(format!(
                "{IMAGE_DECODE_PERMITS_KEY} is outside the allowed range {low}..={high}: {permits}"
            )));
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = permits;
        self.freed.notify_all();
        Ok(())
    }

    /// The permits of this process.
    pub fn permits(&self) -> u32 {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0
    }

    fn acquire(&'static self, until: Instant) -> std::result::Result<DecodePermit, Unfinished> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut state, _) = self
            .freed
            .wait_timeout_while(
                state,
                until.saturating_duration_since(Instant::now()),
                |s| s.1 >= s.0,
            )
            .unwrap_or_else(PoisonError::into_inner);
        if state.1 >= state.0 {
            return Err(Unfinished::NoPermit(state.0));
        }
        state.1 += 1;
        Ok(DecodePermit(self))
    }

    /// Run `job` on the blocking pool under one permit, waiting for the permit and the result no
    /// longer than `timeout` together. On a multi-thread runtime the waiting worker hands its other
    /// tasks on first (`block_in_place`), so the runtime keeps serving while a picture decodes.
    fn run<T: Send + 'static>(
        &'static self,
        timeout: Duration,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> std::result::Result<T, Unfinished> {
        let until = Instant::now() + timeout;
        let wait = move || {
            let permit = self.acquire(until)?;
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            let task = move || {
                let _permit = permit;
                // a failed send means the caller stopped waiting (timed out) and has already
                // returned its own error; the result is dropped with the permit
                let _ = send.send(job());
            };
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => drop(runtime.spawn_blocking(task)),
                Err(_) => drop(std::thread::spawn(task)),
            }
            receive
                .recv_timeout(until.saturating_duration_since(Instant::now()))
                .map_err(|error| match error {
                    std::sync::mpsc::RecvTimeoutError::Timeout => Unfinished::TimedOut,
                    std::sync::mpsc::RecvTimeoutError::Disconnected => Unfinished::Stopped,
                })
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime)
                if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread =>
            {
                tokio::task::block_in_place(wait)
            }
            _ => wait(),
        }
    }
}

/// The raster formats an image field accepts, detected from the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
    /// WebP.
    WebP,
}

impl ImageFormat {
    /// The media type.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
        }
    }
}

/// Format and header dimensions of image content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    /// Detected format.
    pub format: ImageFormat,
    /// Width in pixels, from the header.
    pub width: u32,
    /// Height in pixels, from the header.
    pub height: u32,
}

impl ImageInfo {
    /// `width * height`.
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// Why content is not an acceptable image. The messages never carry content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProbeError {
    /// SVG or other markup.
    Svg,
    /// Not one of the four raster formats.
    UnknownFormat,
    /// One of the four formats, with a header that is truncated or declares no pixels.
    BadHeader(ImageFormat),
    /// A GIF frame that is empty or lies outside the logical screen its header declares.
    GifFrame,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Svg => {
                f.write_str("SVG and other markup is not accepted; use PNG, JPEG, GIF or WebP")
            }
            Self::UnknownFormat => f.write_str("the file is not a PNG, JPEG, GIF or WebP image"),
            Self::BadHeader(format) => write!(
                f,
                "the {} header is truncated or declares no pixels",
                format.mime()
            ),
            Self::GifFrame => {
                f.write_str("a frame of the GIF is empty or lies outside its logical screen")
            }
        }
    }
}

impl std::error::Error for ProbeError {}

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

fn png_size(d: &[u8]) -> Option<(u32, u32)> {
    (d.get(12..16)? == b"IHDR").then_some(())?;
    let w = u32::from_be_bytes(d.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(d.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

/// The frame size of a JPEG and whether it carries segments other than the ones a bare picture
/// needs (an application segment beyond JFIF and Adobe colour, or a comment): Exif, XMP, ICC.
fn jpeg_scan(d: &[u8]) -> Option<((u32, u32), bool)> {
    let mut at = 2;
    let mut extra = false;
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
            // metadata may also sit between the frame header and the first scan: those segments
            // are walked too, and a malformed walk counts as metadata (one more re-encode)
            let size = (be16(d, at + 7)?, be16(d, at + 5)?);
            return Some((size, extra || jpeg_extra_after(d, at)));
        }
        if (0xD8..=0xDA).contains(&marker) {
            return None; // no frame header before the image data
        }
        // APP0 is JFIF, APP14 Adobe colour (the decoder needs it); every other APPn and COM is
        // metadata
        if ((0xE1..=0xEF).contains(&marker) && marker != 0xEE) || marker == 0xFE {
            extra = true;
        }
        at = at
            .checked_add(2)?
            .checked_add(usize::try_from(be16(d, at + 2)?).ok()?)?;
    }
}

/// Whether metadata segments follow the frame header at `at` before the first scan.
fn jpeg_extra_after(d: &[u8], mut at: usize) -> bool {
    loop {
        let (Some(0xFF), Some(&marker)) = (d.get(at).copied(), d.get(at + 1)) else {
            return true; // malformed: let the decoder judge, and encode again
        };
        if marker == 0xFF {
            at += 1;
            continue;
        }
        if marker == 0xDA {
            return false;
        }
        if ((0xE1..=0xEF).contains(&marker) && marker != 0xEE) || marker == 0xFE {
            return true;
        }
        let Some(len) = be16(d, at + 2).and_then(|n| usize::try_from(n).ok()) else {
            return true;
        };
        let Some(next) = at.checked_add(2).and_then(|n| n.checked_add(len)) else {
            return true;
        };
        at = next;
    }
}

/// Skip the data sub-blocks that start at `at` (a length byte, that many bytes, until a zero
/// length) and return the position after the terminator.
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

/// The logical screen of a GIF, after walking its blocks to every image descriptor without
/// decoding: each frame must declare pixels and lie inside the screen, so the screen size bounds
/// every frame a decoder allocates. `None` for a truncated or malformed block structure or a GIF
/// without a frame.
fn gif_size(d: &[u8]) -> Option<std::result::Result<(u32, u32), ProbeError>> {
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
            // extension: introducer, label, data sub-blocks
            0x21 => at = gif_skip_sub_blocks(d, at + 2)?,
            0x2C => {
                let (left, top) = (le16(d, at + 1)?, le16(d, at + 3)?);
                let (w, h) = (le16(d, at + 5)?, le16(d, at + 7)?);
                if w == 0 || h == 0 || left + w > width || top + h > height {
                    return Some(Err(ProbeError::GifFrame));
                }
                // descriptor, local colour table, LZW minimum code size, then the data
                at += 10 + colour_table(*d.get(at + 9)?) + 1;
                at = gif_skip_sub_blocks(d, at)?;
                frames += 1;
            }
            _ => return None,
        }
    }
    (frames > 0).then_some(Ok((width, height)))
}

fn webp_size(d: &[u8]) -> Option<(u32, u32)> {
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

fn looks_like_markup(d: &[u8]) -> bool {
    let head = &d[..d.len().min(4096)];
    head.iter()
        .find(|c| !c.is_ascii_whitespace() && ![0xEF, 0xBB, 0xBF].contains(*c))
        .is_some_and(|c| *c == b'<')
}

/// Detect the format of `data` from its first bytes and read the size from its header. Nothing is
/// decoded.
///
/// # Errors
/// [`ProbeError`] for markup (SVG included), an unknown format, a truncated or empty header, or a
/// GIF frame outside its logical screen.
pub fn probe(data: &[u8]) -> std::result::Result<ImageInfo, ProbeError> {
    let (format, size) = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        (ImageFormat::Png, png_size(data))
    } else if data.starts_with(&[0xFF, 0xD8]) {
        (ImageFormat::Jpeg, jpeg_scan(data).map(|(size, _)| size))
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        (ImageFormat::Gif, gif_size(data).transpose()?)
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        (ImageFormat::WebP, webp_size(data))
    } else if looks_like_markup(data) {
        return Err(ProbeError::Svg);
    } else {
        return Err(ProbeError::UnknownFormat);
    };
    match size {
        Some((width, height)) if width > 0 && height > 0 => Ok(ImageInfo {
            format,
            width,
            height,
        }),
        _ => Err(ProbeError::BadHeader(format)),
    }
}

/// ```
/// use erp_orm::image::human_size;
/// assert_eq!(human_size(532), "532.00 bytes");
/// assert_eq!(human_size(1536), "1.50 Kb");
/// ```
pub fn human_size(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["bytes", "Kb", "Mb", "Gb", "Tb"];
    // f64 holds every size a value can have exactly enough for two decimals
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.2} {}", UNITS[unit])
}

fn refusal(field: FieldRef<'_>, reason: std::fmt::Arguments<'_>) -> Error {
    Error::validation(format!(
        "Invalid image for {}.{}: {reason}",
        field.model, field.field
    ))
}

/// What an image field with `bounds` stores for `content`, under `policy`:
///
/// - the content must be at most `policy.max_bytes()` bytes;
/// - it must be PNG, JPEG, GIF or WebP by its bytes; SVG and everything else is refused;
/// - its header size, and every GIF frame, must be at most `policy.max_pixels()`;
/// - an unbounded field keeps the bytes as they came;
/// - a bounded field keeps a GIF or WebP that fits its bounds as it came (an animation stays
///   whole); a larger one is decoded (first frame), scaled down and stored as PNG;
/// - a bounded PNG or JPEG is decoded, turned upright, scaled down to the bounds (never up, aspect
///   ratio kept) and encoded again in its own format. A JPEG that already fits and carries no
///   metadata is kept as it is, so storing the server's own output again changes nothing.
///
/// Decoding runs on the blocking pool under a permit of [`DecodePool::global`], waiting no longer
/// than `policy.decode_timeout()`.
///
/// # Errors
/// A validation error (a user error) that names the field and the reason, never the content: too
/// large, not an accepted image, above the pixel limit (naming the parameter), or not decodable. A
/// resource error naming the setting when no permit became free or the decode did not end in time.
pub fn admit(
    field: FieldRef<'_>,
    content: &Blob,
    bounds: ImageBounds,
    policy: &ImagePolicy,
) -> Result<Blob> {
    admit_in(DecodePool::global(), field, content, bounds, policy)
}

fn admit_in(
    pool: &'static DecodePool,
    field: FieldRef<'_>,
    content: &Blob,
    bounds: ImageBounds,
    policy: &ImagePolicy,
) -> Result<Blob> {
    if u64::try_from(content.len()).unwrap_or(u64::MAX) > policy.max_bytes() {
        return Err(refusal(
            field,
            format_args!(
                "{} bytes is above the limit of {} bytes ({IMAGE_MAX_BYTES_KEY})",
                content.len(),
                policy.max_bytes()
            ),
        ));
    }
    let info = probe(content).map_err(|error| refusal(field, format_args!("{error}")))?;
    if info.pixels() > policy.max_pixels() {
        return Err(refusal(
            field,
            format_args!(
                "{} x {} pixels is above the limit of {} pixels ({IMAGE_MAX_PIXELS_KEY})",
                info.width,
                info.height,
                policy.max_pixels()
            ),
        ));
    }
    if bounds.is_unbounded() {
        return Ok(content.clone());
    }
    let fits = bounds.fit(info.width, info.height) == (info.width, info.height);
    let keep = match info.format {
        ImageFormat::Gif | ImageFormat::WebP => fits,
        ImageFormat::Jpeg => fits && jpeg_scan(content).is_some_and(|(_, extra)| !extra),
        ImageFormat::Png => false,
    };
    if keep {
        return Ok(content.clone());
    }
    let (source, policy_copy) = (content.clone(), *policy);
    let decoded = pool.run(policy.decode_timeout(), move || {
        reencode(&source, info, bounds, &policy_copy).map_err(|error| error.to_string())
    });
    match decoded {
        Ok(Ok(encoded)) => Ok(Blob::from(encoded)),
        // the decoder's text names the defect (a format detail), not the content
        Ok(Err(error)) => Err(refusal(
            field,
            format_args!("the picture cannot be decoded ({error})"),
        )),
        Err(Unfinished::NoPermit(permits)) => Err(Error::new(
            ErrorKind::ResourceExhausted,
            format!(
                "{}.{}: no picture decode became free within {} s ({IMAGE_DECODE_TIMEOUT_KEY}; \
                 this node decodes {permits} pictures at once, {IMAGE_DECODE_PERMITS_KEY}); try \
                 again",
                field.model,
                field.field,
                policy.decode_timeout().as_secs()
            ),
        )),
        Err(Unfinished::TimedOut) => Err(Error::new(
            ErrorKind::ResourceExhausted,
            format!(
                "{}.{}: decoding the picture took longer than {} s ({IMAGE_DECODE_TIMEOUT_KEY})",
                field.model,
                field.field,
                policy.decode_timeout().as_secs()
            ),
        )),
        Err(Unfinished::Stopped) => Err(refusal(
            field,
            format_args!("the picture cannot be decoded (the decoder stopped)"),
        )),
    }
}

fn reencode(
    content: &[u8],
    info: ImageInfo,
    bounds: ImageBounds,
    policy: &ImagePolicy,
) -> std::result::Result<Vec<u8>, image::ImageError> {
    use image::{DynamicImage, ImageDecoder, ImageReader};
    let format = match info.format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::WebP => image::ImageFormat::WebP,
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(info.width);
    limits.max_image_height = Some(info.height);
    // one 8-bit RGBA raster of the pixel limit, as B9 sizes a decode; a file whose decode
    // needs more (16-bit channels at the limit) is refused by the decoder
    limits.max_alloc = Some(policy.max_pixels().saturating_mul(4));
    let mut reader = ImageReader::with_format(Cursor::new(content), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut picture = DynamicImage::from_decoder(decoder)?;
    picture.apply_orientation(orientation);
    // the orientation may have swapped the edges: fit what is stored, not what the header said
    let (width, height) = bounds.fit(picture.width(), picture.height());
    if (width, height) != (picture.width(), picture.height()) {
        picture = picture.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    }
    let mut out = Vec::new();
    match info.format {
        // a GIF or WebP larger than the bounds is stored as a still PNG
        ImageFormat::Png | ImageFormat::Gif | ImageFormat::WebP => {
            let encoder = image::codecs::png::PngEncoder::new(&mut out);
            picture.write_with_encoder(encoder)?;
        }
        ImageFormat::Jpeg => {
            // JPEG has no alpha and 8 bits a channel
            let picture = match picture {
                DynamicImage::ImageLuma8(_) | DynamicImage::ImageRgb8(_) => picture,
                DynamicImage::ImageLuma16(_) | DynamicImage::ImageLumaA8(_) => {
                    DynamicImage::ImageLuma8(picture.into_luma8())
                }
                other => DynamicImage::ImageRgb8(other.into_rgb8()),
            };
            let encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, policy.jpeg_quality());
            picture.write_with_encoder(encoder)?;
        }
    }
    Ok(out)
}

impl Transaction {
    /// The image limits this transaction applies, loaded by [`Transaction::ensure_image_policy`].
    ///
    /// # Errors
    /// A programming error when the registry has `ir.config_parameter` and the limits were not
    /// loaded: an admission path that forgot to load them fails loudly instead of using the
    /// defaults. Without that model (a registry without addon `base`) the declared defaults apply.
    pub fn image_policy(&self) -> Result<ImagePolicy> {
        match self.image_policy {
            Some(policy) => Ok(policy),
            None if self.registry().model_id("ir.config_parameter").is_none() => {
                Ok(ImagePolicy::default())
            }
            None => Err(Error::programming(
                "the image limits of the database were not loaded before a picture was admitted \
                 (the async caller must call Transaction::ensure_image_policy first)",
            )),
        }
    }

    /// Set the image limits for the rest of this transaction attempt (a host that already knows
    /// them, and tests).
    pub fn set_image_policy(&mut self, policy: ImagePolicy) {
        self.image_policy = Some(policy);
    }

    /// Load the image limits of the database once per transaction attempt, when `spec` has an
    /// image field and they are not loaded yet. Reads the system parameters with one statement
    /// when the registry has `ir.config_parameter`; without that model (a registry without addon
    /// `base`) the declared defaults apply.
    ///
    /// # Errors
    /// A database error, or a validation error naming a stored parameter outside its range.
    pub async fn ensure_image_policy(&mut self, spec: &ModelSpec) -> Result<()> {
        if self.image_policy.is_some() || !spec.fields.iter().any(|f| f.ty.image_bounds().is_some())
        {
            return Ok(());
        }
        self.load_image_policy().await
    }

    /// Load the image limits of the database once per transaction attempt, whatever the model: a
    /// related picture field has no bounds of its own but writes into one that has.
    ///
    /// # Errors
    /// A database error, or a validation error naming a stored parameter outside its range.
    pub(crate) async fn load_image_policy(&mut self) -> Result<()> {
        if self.image_policy.is_some() {
            return Ok(());
        }
        let registry = std::sync::Arc::clone(self.registry());
        let Some(params) = registry.model_id("ir.config_parameter") else {
            self.image_policy = Some(ImagePolicy::default());
            return Ok(());
        };
        self.flush_model(params).await?;
        let keys: Vec<String> = ImagePolicy::PARAMETERS
            .iter()
            .map(|p| p.key.to_owned())
            .collect();
        let rows = self
            .query_typed(
                // a stored value longer than a number is reported as malformed, never loaded
                "SELECT key, CASE WHEN value IS NULL OR octet_length(value) <= 32 THEN value \
                 ELSE 'invalid' END FROM ir_config_parameter WHERE key = ANY($1)",
                &[(&keys, tokio_postgres::types::Type::TEXT_ARRAY)],
            )
            .await?;
        let mut stored: Vec<(String, Option<String>)> = Vec::with_capacity(rows.len());
        for row in &rows {
            let key: String = row
                .try_get(0)
                .map_err(|e| Error::db(format!("image policy key: {e}")))?;
            let value: Option<String> = row
                .try_get(1)
                .map_err(|e| Error::db(format!("image policy value of {key}: {e}")))?;
            stored.push((key, value));
        }
        self.image_policy = Some(ImagePolicy::from_parameters(|key| {
            stored
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| v.as_deref())
        })?);
        Ok(())
    }

    /// Admit the value of an image field: checked and resized per [`admit`]. Other fields and
    /// empty values pass unchanged.
    pub(crate) fn admit_image_cell(
        &self,
        spec: &ModelSpec,
        field: &FieldSpec,
        cell: Cell,
    ) -> Result<Cell> {
        let FieldType::Binary {
            image: Some(bounds),
        } = field.ty
        else {
            return Ok(cell);
        };
        match cell {
            Cell::Bytes(content) => {
                let policy = self.image_policy()?;
                admit(
                    crate::html::field_ref(spec, field),
                    &content,
                    bounds,
                    &policy,
                )
                .map(Cell::Bytes)
            }
            other => Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELD: FieldRef<'static> = FieldRef {
        model: "test.model",
        field: "image_128",
        label: "Image",
    };

    fn png(width: u32, height: u32) -> Vec<u8> {
        let picture = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 120, 255])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(picture)
            .write_with_encoder(image::codecs::png::PngEncoder::new(&mut out))
            .unwrap();
        out
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let picture = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 60])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(picture)
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut out, 90,
            ))
            .unwrap();
        out
    }

    fn bounds(edge: u32) -> ImageBounds {
        ImageBounds::new(edge, edge).unwrap()
    }

    #[test]
    fn a_bounded_field_scales_down_and_keeps_the_aspect_ratio() {
        let source = Blob::from(png(400, 200));
        let stored = admit(FIELD, &source, bounds(128), &ImagePolicy::default()).unwrap();
        let info = probe(&stored).unwrap();
        assert_eq!(
            (info.format, info.width, info.height),
            (ImageFormat::Png, 128, 64)
        );
    }

    #[test]
    fn a_small_picture_is_never_enlarged() {
        for content in [png(40, 20), jpeg(40, 20)] {
            let stored = admit(
                FIELD,
                &Blob::from(content),
                bounds(128),
                &ImagePolicy::default(),
            )
            .unwrap();
            let info = probe(&stored).unwrap();
            assert_eq!((info.width, info.height), (40, 20));
        }
    }

    #[test]
    fn a_jpeg_stays_a_jpeg_and_the_servers_own_output_is_a_fixed_point() {
        let policy = ImagePolicy::default();
        let first = admit(FIELD, &Blob::from(jpeg(300, 300)), bounds(128), &policy).unwrap();
        let info = probe(&first).unwrap();
        assert_eq!(
            (info.format, info.width, info.height),
            (ImageFormat::Jpeg, 128, 128)
        );
        let second = admit(FIELD, &first, bounds(128), &policy).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn an_unbounded_field_keeps_the_bytes() {
        let source = Blob::from(png(300, 100));
        let stored = admit(
            FIELD,
            &source,
            ImageBounds::default(),
            &ImagePolicy::default(),
        )
        .unwrap();
        assert_eq!(stored, source);
    }

    #[test]
    fn a_picture_above_the_pixel_limit_is_refused_by_name() {
        // a PNG header that declares 20000 x 20000 pixels; no pixel data follows, and none is read
        let mut header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
        header.extend_from_slice(&20_000u32.to_be_bytes());
        header.extend_from_slice(&20_000u32.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        let error = admit(
            FIELD,
            &Blob::from(header),
            bounds(128),
            &ImagePolicy::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), erp_fields::ErrorKind::Validation);
        assert_eq!(
            error.to_string(),
            "Invalid image for test.model.image_128: 20000 x 20000 pixels is above the limit of \
             40000000 pixels (base.image_max_pixels)"
        );
    }

    #[test]
    fn svg_and_other_content_are_refused() {
        let svg = Blob::from(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec());
        let error = admit(FIELD, &svg, bounds(128), &ImagePolicy::default()).unwrap_err();
        assert_eq!(error.kind(), erp_fields::ErrorKind::Validation);
        assert_eq!(
            error.to_string(),
            "Invalid image for test.model.image_128: SVG and other markup is not accepted; use \
             PNG, JPEG, GIF or WebP"
        );
        let text = Blob::from(b"just some text".to_vec());
        let error = admit(FIELD, &text, bounds(128), &ImagePolicy::default()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Invalid image for test.model.image_128: the file is not a PNG, JPEG, GIF or WebP image"
        );
    }

    #[test]
    fn a_truncated_picture_is_a_user_error_without_content() {
        let mut content = png(64, 64);
        content.truncate(60);
        let error = admit(
            FIELD,
            &Blob::from(content),
            bounds(32),
            &ImagePolicy::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), erp_fields::ErrorKind::Validation);
        assert!(
            error.to_string().starts_with(
                "Invalid image for test.model.image_128: the picture cannot be decoded"
            ),
            "{error}"
        );
    }

    fn gif(width: u16, height: u16) -> Vec<u8> {
        let picture = image::RgbaImage::from_fn(u32::from(width), u32::from(height), |x, _| {
            image::Rgba([(x % 2 * 200) as u8, 30, 60, 255])
        });
        let mut out = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut out)
            .encode_frame(image::Frame::new(picture))
            .unwrap();
        out
    }

    fn webp(width: u32, height: u32) -> Vec<u8> {
        let picture = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 10, 255])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(picture)
            .write_with_encoder(image::codecs::webp::WebPEncoder::new_lossless(&mut out))
            .unwrap();
        out
    }

    #[test]
    fn a_gif_or_webp_that_fits_is_kept_and_a_larger_one_is_stored_as_png() {
        let policy = ImagePolicy::default();
        for (small, large) in [
            (gif(100, 50), gif(300, 150)),
            (webp(100, 50), webp(300, 150)),
        ] {
            let small = Blob::from(small);
            assert_eq!(admit(FIELD, &small, bounds(128), &policy).unwrap(), small);
            let stored = admit(FIELD, &Blob::from(large), bounds(128), &policy).unwrap();
            let info = probe(&stored).unwrap();
            assert_eq!(
                (info.format, info.width, info.height),
                (ImageFormat::Png, 128, 64)
            );
        }
    }

    /// A GIF of a `width` x `height` screen with one frame of `fw` x `fh` at the origin; the
    /// frame's data is a bare LZW stream (clear, end).
    fn gif_with_frame(width: u16, height: u16, fw: u16, fh: u16) -> Vec<u8> {
        let mut out = b"GIF89a".to_vec();
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(&[0x2C, 0, 0, 0, 0]);
        out.extend_from_slice(&fw.to_le_bytes());
        out.extend_from_slice(&fh.to_le_bytes());
        out.extend_from_slice(&[0, 2, 1, 0x44, 0, 0x3B]);
        out
    }

    /// Malformed and oversized files are refused by name, never with a panic.
    #[test]
    fn a_malformed_file_corpus_is_refused_by_name() {
        let policy = ImagePolicy::default();
        let mut truncated = png(64, 64);
        truncated.truncate(60);
        let header = |width: u32, height: u32| {
            let mut out = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
            out.extend_from_slice(&width.to_be_bytes());
            out.extend_from_slice(&height.to_be_bytes());
            out.extend_from_slice(&[8, 6, 0, 0, 0]);
            out
        };
        let mut magic = png(8, 8);
        magic[1] = b'Q';
        let corpus: [(&str, Vec<u8>, &str); 6] = [
            ("truncated PNG", truncated, "the picture cannot be decoded"),
            (
                "GIF frame larger than its screen",
                gif_with_frame(10, 10, 4_000, 4_000),
                "a frame of the GIF is empty or lies outside its logical screen",
            ),
            (
                "empty GIF frame",
                gif_with_frame(10, 10, 0, 4),
                "a frame of the GIF is empty or lies outside its logical screen",
            ),
            (
                "0 x 0 image",
                header(0, 0),
                "the image/png header is truncated or declares no pixels",
            ),
            (
                "wrong magic bytes",
                magic,
                "the file is not a PNG, JPEG, GIF or WebP image",
            ),
            (
                "over the cap with a valid header",
                header(10_000, 5_000),
                "10000 x 5000 pixels is above the limit of 40000000 pixels (base.image_max_pixels)",
            ),
        ];
        let prefix = "Invalid image for test.model.image_128: ";
        for (case, content, reason) in corpus {
            let error = admit(FIELD, &Blob::from(content), bounds(128), &policy).unwrap_err();
            assert_eq!(error.kind(), erp_fields::ErrorKind::Validation, "{case}");
            assert!(
                error.to_string().starts_with(&format!("{prefix}{reason}")),
                "{case}: {error}"
            );
        }
        // a frame inside its screen is a GIF like any other
        assert_eq!(
            probe(&gif_with_frame(10, 10, 4, 4)).unwrap(),
            ImageInfo {
                format: ImageFormat::Gif,
                width: 10,
                height: 10
            }
        );
    }

    #[test]
    fn a_picture_above_the_byte_limit_is_refused_by_name() {
        let policy =
            ImagePolicy::from_parameters(|key| (key == IMAGE_MAX_BYTES_KEY).then_some("65536"))
                .unwrap();
        let mut content = gif(10, 10);
        content.resize(65_537, 0);
        let error = admit(FIELD, &Blob::from(content), bounds(128), &policy).unwrap_err();
        assert_eq!(error.kind(), erp_fields::ErrorKind::Validation);
        assert_eq!(
            error.to_string(),
            "Invalid image for test.model.image_128: 65537 bytes is above the limit of 65536 \
             bytes (base.image_max_bytes)"
        );
    }

    fn leaked_pool(permits: u32) -> &'static DecodePool {
        Box::leak(Box::new(DecodePool::new(permits)))
    }

    fn in_use(pool: &DecodePool) -> u32 {
        pool.state.lock().unwrap().1
    }

    #[test]
    fn a_pool_runs_no_more_decodes_than_its_permits() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
        let pool = leaked_pool(2);
        let running = Arc::new(AtomicU32::new(0));
        let most = Arc::new(AtomicU32::new(0));
        let callers: Vec<_> = (0..6)
            .map(|_| {
                let (running, most) = (Arc::clone(&running), Arc::clone(&most));
                std::thread::spawn(move || {
                    pool.run(Duration::from_secs(30), move || {
                        let now = running.fetch_add(1, SeqCst) + 1;
                        most.fetch_max(now, SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        running.fetch_sub(1, SeqCst);
                    })
                })
            })
            .collect();
        for caller in callers {
            caller.join().unwrap().unwrap();
        }
        assert_eq!(most.load(SeqCst), 2);
    }

    #[test]
    fn a_wait_for_a_permit_or_a_decode_ends_at_the_timeout() {
        let pool = leaked_pool(1);
        let (release, held) = std::sync::mpsc::channel::<()>();
        let holder =
            std::thread::spawn(move || pool.run(Duration::from_secs(30), move || held.recv()));
        while in_use(pool) == 0 {
            std::thread::yield_now();
        }
        // the only permit is held: the next caller gives up at its deadline
        assert_eq!(
            pool.run(Duration::from_millis(50), || ()).unwrap_err(),
            Unfinished::NoPermit(1)
        );
        release.send(()).unwrap();
        holder.join().unwrap().unwrap().unwrap();
        // a decode slower than the deadline: the caller stops waiting, the decode keeps its
        // permit until it ends
        assert_eq!(
            pool.run(Duration::from_millis(50), || {
                std::thread::sleep(Duration::from_millis(300));
            })
            .unwrap_err(),
            Unfinished::TimedOut
        );
        assert_eq!(in_use(pool), 1);
        while in_use(pool) != 0 {
            std::thread::yield_now();
        }
        // a decoder that panics frees its permit and gives no result
        assert_eq!(
            pool.run::<()>(Duration::from_secs(5), || panic!("decoder defect"))
                .unwrap_err(),
            Unfinished::Stopped
        );
        assert_eq!(in_use(pool), 0);
    }

    #[test]
    fn admission_names_the_field_and_the_setting_when_the_pool_is_busy() {
        let pool = leaked_pool(1);
        let (release, held) = std::sync::mpsc::channel::<()>();
        let holder =
            std::thread::spawn(move || pool.run(Duration::from_secs(30), move || held.recv()));
        while in_use(pool) == 0 {
            std::thread::yield_now();
        }
        let policy =
            ImagePolicy::from_parameters(|key| (key == IMAGE_DECODE_TIMEOUT_KEY).then_some("1"))
                .unwrap();
        let error = admit_in(
            pool,
            FIELD,
            &Blob::from(png(300, 300)),
            bounds(128),
            &policy,
        )
        .unwrap_err();
        assert_eq!(error.kind(), erp_fields::ErrorKind::ResourceExhausted);
        assert_eq!(
            error.to_string(),
            "test.model.image_128: no picture decode became free within 1 s \
             (base.image_decode_timeout_secs; this node decodes 1 pictures at once, \
             attachments.image_decode_permits); try again"
        );
        release.send(()).unwrap();
        holder.join().unwrap().unwrap().unwrap();
        assert_eq!(
            leaked_pool(1).set_permits(65).unwrap_err().to_string(),
            "attachments.image_decode_permits is outside the allowed range 1..=64: 65"
        );
        assert!(leaked_pool(1).set_permits(0).is_err());
    }

    #[test]
    fn policy_values_outside_their_range_are_refused_by_name() {
        let only = |key: &'static str, text: &'static str| {
            ImagePolicy::from_parameters(move |k| (k == key).then_some(text))
        };
        assert_eq!(
            only(IMAGE_JPEG_QUALITY_KEY, "").unwrap(),
            ImagePolicy::default()
        );
        assert_eq!(
            only(IMAGE_MAX_PIXELS_KEY, "0").unwrap_err().to_string(),
            "base.image_max_pixels is outside the allowed range 1000000..=100000000: 0"
        );
        assert_eq!(
            only(IMAGE_JPEG_QUALITY_KEY, "101").unwrap_err().to_string(),
            "base.image_jpeg_quality is outside the allowed range 1..=100: 101"
        );
        assert_eq!(
            only(IMAGE_DECODE_TIMEOUT_KEY, "121")
                .unwrap_err()
                .to_string(),
            "base.image_decode_timeout_secs is outside the allowed range 1..=120: 121"
        );
        assert_eq!(
            only(IMAGE_MAX_BYTES_KEY, "65535").unwrap_err().to_string(),
            "base.image_max_bytes is outside the allowed range 65536..=134217728: 65535"
        );
        assert_eq!(
            only(IMAGE_MAX_PIXELS_KEY, "many").unwrap_err().to_string(),
            "base.image_max_pixels must be a whole number, got a malformed value"
        );
        let set = ImagePolicy::from_parameters(|key| match key {
            IMAGE_MAX_PIXELS_KEY => Some("50000000"),
            IMAGE_JPEG_QUALITY_KEY => Some("80"),
            IMAGE_DECODE_TIMEOUT_KEY => Some("30"),
            IMAGE_MAX_BYTES_KEY => Some("1048576"),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            (
                set.max_pixels(),
                set.jpeg_quality(),
                set.decode_timeout(),
                set.max_bytes()
            ),
            (50_000_000, 80, Duration::from_secs(30), 1_048_576)
        );
    }
}
