// SPDX-License-Identifier: MIT OR Apache-2.0
//! Barcodes and QR codes. A value is encoded into modules here and drawn as a [`Vector`] of
//! rectangles: never a raster, never caller markup.
//!
//! Sizing is print-accurate. A module is a whole number of dots of a [`ModuleGrid`] (the printer's
//! resolution), every bar is a whole number of modules, and the symbol is the largest one of that
//! kind that fits the box it is given: it is never stretched, and its quiet zone is part of it.
use crate::{LimitKind, Limits, ReportError, Vector};

const BLACK: [u8; 3] = [0, 0, 0];
/// Modules of a wide element of Code 39 and Interleaved 2 of 5 (a narrow one is 1).
const WIDE: usize = 3;
/// Quiet zone of a QR code on each side, in modules.
const QR_QUIET: usize = 4;

/// The kinds of code this library prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Symbology {
    Code128,
    Ean13,
    Ean8,
    UpcA,
    Code39,
    /// Full 7-bit ASCII encoded using Code 39 character pairs.
    Extended39,
    /// Interleaved 2 of 5 of 14 digits with the GS1 check digit.
    Itf14,
    /// Interleaved 2 of 5 of any even number of digits, without a check digit.
    Interleaved2of5,
    Qr,
    /// Square ECC200 Data Matrix. GS1 is selected by the dedicated encoding method.
    DataMatrix,
}
impl Symbology {
    pub const ALL: [Self; 10] = [
        Self::Code128,
        Self::Ean13,
        Self::Ean8,
        Self::UpcA,
        Self::Code39,
        Self::Extended39,
        Self::Itf14,
        Self::Interleaved2of5,
        Self::Qr,
        Self::DataMatrix,
    ];
    /// The name a template writes.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Code128 => "Code128",
            Self::Ean13 => "EAN13",
            Self::Ean8 => "EAN8",
            Self::UpcA => "UPCA",
            Self::Code39 => "Code39",
            Self::Extended39 => "Extended39",
            Self::Itf14 => "ITF14",
            Self::Interleaved2of5 => "I2of5",
            Self::Qr => "QR",
            Self::DataMatrix => "DataMatrix",
        }
    }
    /// The symbology of a template name: ours, or the reference's name for the same code
    /// (`Standard39`). Letter case, `-`, `_` and spaces do not matter.
    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Some(match key.as_str() {
            "code128" => Self::Code128,
            "ean13" => Self::Ean13,
            "ean8" => Self::Ean8,
            "upca" => Self::UpcA,
            "code39" | "standard39" => Self::Code39,
            "extended39" => Self::Extended39,
            "itf14" => Self::Itf14,
            "i2of5" | "interleaved2of5" => Self::Interleaved2of5,
            "qr" | "qrcode" => Self::Qr,
            "datamatrix" | "ecc200datamatrix" => Self::DataMatrix,
            _ => return None,
        })
    }
    /// The symbology of a value whose kind the template leaves open (`auto`): EAN-13 or EAN-8 for
    /// 13 or 8 digits that end in the right check digit, Code 128 for everything else. As in the
    /// reference, 13 digits that start with 0 print as Code 128 under `auto`; asked for by name
    /// they are an EAN-13.
    pub fn auto(value: &str) -> Self {
        let digits: Option<Vec<u8>> = value
            .bytes()
            .map(|b| b.is_ascii_digit().then(|| b - b'0'))
            .collect();
        match digits.as_deref() {
            Some(d) if d.len() == 8 && mod10(&d[..7]) == d[7] => Self::Ean8,
            Some(d) if d.len() == 13 && d[0] != 0 && mod10(&d[..12]) == d[12] => Self::Ean13,
            _ => Self::Code128,
        }
    }
    pub const fn is_matrix(self) -> bool {
        matches!(self, Self::Qr | Self::DataMatrix)
    }
}
impl std::fmt::Display for Symbology {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Error correction of a QR code: the share of the symbol that may be unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum QrLevel {
    /// About 7 %.
    #[default]
    Low,
    /// About 15 %.
    Medium,
    /// About 25 %.
    Quartile,
    /// About 30 %.
    High,
}
impl QrLevel {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Low => "L",
            Self::Medium => "M",
            Self::Quartile => "Q",
            Self::High => "H",
        }
    }
    /// `L`, `M`, `Q` or `H`.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "L" | "l" => Self::Low,
            "M" | "m" => Self::Medium,
            "Q" | "q" => Self::Quartile,
            "H" | "h" => Self::High,
            _ => return None,
        })
    }
}

/// The resolution of the printer a code is sized for, in dots per inch. A module is a whole number
/// of its dots, at least 1/150 inch wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleGrid {
    dpi: u32,
}
impl ModuleGrid {
    /// 300 dots per inch: whole dots on 300, 600 and 1200 dpi printers.
    pub const DEFAULT_DPI: u32 = 300;
    pub const MIN_DPI: u32 = 72;
    pub const MAX_DPI: u32 = 2400;
    /// A grid of `dpi` dots per inch, in 72..=2400.
    pub fn new(dpi: u32) -> Result<Self, ReportError> {
        Self::from_given(i64::from(dpi))
    }
    /// As `new`, for a value as it was written: a negative one is reported as written.
    pub(crate) fn from_given(dpi: i64) -> Result<Self, ReportError> {
        match u32::try_from(dpi) {
            Ok(value) if (Self::MIN_DPI..=Self::MAX_DPI).contains(&value) => {
                Ok(Self { dpi: value })
            }
            _ => Err(ReportError::Code {
                symbology: "barcode",
                reason: format!(
                    "the printer resolution must be in {}..={} dots per inch; got {dpi}",
                    Self::MIN_DPI,
                    Self::MAX_DPI
                ),
            }),
        }
    }
    pub fn dpi(self) -> u32 {
        self.dpi
    }
    /// One dot, in points.
    fn dot(self) -> f64 {
        72.0 / f64::from(self.dpi)
    }
    /// Dots of the smallest module printed: 1/150 inch rounded up to whole dots.
    fn smallest(self) -> u32 {
        self.dpi.div_ceil(150)
    }
}
impl Default for ModuleGrid {
    fn default() -> Self {
        Self {
            dpi: Self::DEFAULT_DPI,
        }
    }
}

fn refuse(symbology: Symbology, reason: impl Into<String>) -> ReportError {
    ReportError::Code {
        symbology: symbology.name(),
        reason: reason.into(),
    }
}

/// One encoded symbol: its dark modules, before it is sized.
#[derive(Debug, Clone)]
pub struct Code {
    symbology: Symbology,
    /// Row by row, without the quiet zone; a linear symbol has one row.
    modules: Vec<bool>,
    columns: usize,
    /// Quiet zone in modules: before and after a linear symbol, on every side of a matrix one.
    quiet: [usize; 2],
    text: String,
    /// Present only for ITF14; validated thickness in narrow modules.
    bearer: Option<u8>,
}
impl Code {
    /// Encode `value`. A value the symbology cannot carry is refused by name: nothing is dropped,
    /// replaced or printed in another symbology. `level` applies to a QR code only.
    pub fn encode(
        symbology: Symbology,
        value: &str,
        level: QrLevel,
        limits: &Limits,
    ) -> Result<Self, ReportError> {
        if value.is_empty() {
            return Err(refuse(symbology, "there is no value to encode"));
        }
        limits.check(LimitKind::CodeBytes, value.len())?;
        let (modules, quiet, text) = match symbology {
            Symbology::Code128 => (code128(value)?, [10, 10], value.to_owned()),
            Symbology::Ean13 => {
                let d = checked_digits(symbology, value, 13)?;
                let parity = EAN_PARITY[usize::from(d[0])];
                (ean(&d[1..7], parity, &d[7..]), [11, 7], digit_text(&d))
            }
            Symbology::Ean8 => {
                let d = checked_digits(symbology, value, 8)?;
                (ean(&d[..4], 0, &d[4..]), [7, 7], digit_text(&d))
            }
            Symbology::UpcA => {
                let d = checked_digits(symbology, value, 12)?;
                (ean(&d[..6], 0, &d[6..]), [9, 9], digit_text(&d))
            }
            Symbology::Code39 => (code39(value)?, [10, 10], value.to_owned()),
            Symbology::Extended39 => (extended39(value)?, [10, 10], value.to_owned()),
            Symbology::Itf14 => {
                let d = checked_digits(symbology, value, 14)?;
                (interleaved(&d), [10, 10], digit_text(&d))
            }
            Symbology::Interleaved2of5 => {
                let d = digits(symbology, value)?;
                if d.len() % 2 != 0 {
                    return Err(refuse(
                        symbology,
                        format!(
                            "it prints digits in pairs; `{value}` has {} digits",
                            d.len()
                        ),
                    ));
                }
                (interleaved(&d), [10, 10], value.to_owned())
            }
            Symbology::DataMatrix => return Self::datamatrix(value, false),
            Symbology::Qr => {
                let (modules, side) = qr(value, level)?;
                return Ok(Self {
                    symbology,
                    modules,
                    columns: side,
                    quiet: [QR_QUIET; 2],
                    text: value.to_owned(),
                    bearer: None,
                });
            }
        };
        Ok(Self {
            symbology,
            columns: modules.len(),
            modules,
            quiet,
            text,
            bearer: (symbology == Symbology::Itf14).then_some(Self::DEFAULT_BEARER_MODULES),
        })
    }
    /// Default ITF14 bearer thickness for the non-plate printing profile, in modules.
    pub const DEFAULT_BEARER_MODULES: u8 = 2;
    /// Minimum bearer thickness in the non-plate printing profile.
    pub const MIN_BEARER_MODULES: u8 = 2;
    /// Hard admission bound for the per-code bearer thickness.
    pub const MAX_BEARER_MODULES: u8 = 10;

    /// Set ITF14 top/bottom bearer thickness in narrow modules; the complete graphic stays
    /// inside the fitted box. Other symbologies and values outside 2..=10 are refused.
    ///
    /// # Errors
    /// Returns a named code error for an incompatible symbology or invalid thickness.
    ///
    /// ```
    /// use erp_report::{Code, Limits, QrLevel, Symbology};
    /// let code = Code::encode(Symbology::Itf14, "1540014128876", QrLevel::Low, &Limits::default())?
    ///     .with_bearer_modules(3)?;
    /// # Ok::<(), erp_report::ReportError>(())
    /// ```
    pub fn with_bearer_modules(mut self, modules: u8) -> Result<Self, ReportError> {
        if self.symbology != Symbology::Itf14 {
            return Err(refuse(
                self.symbology,
                "bearer_modules is available only for ITF14",
            ));
        }
        if !(Self::MIN_BEARER_MODULES..=Self::MAX_BEARER_MODULES).contains(&modules) {
            return Err(refuse(
                self.symbology,
                format!("bearer_modules must be in 2..=10; got {modules}"),
            ));
        }
        self.bearer = Some(modules);
        Ok(self)
    }

    /// Encode a caller-validated GS1 element string as Code128. FNC1 follows the start;
    /// supplied ASCII GS separators become FNC1. Printable ASCII is accepted, other controls
    /// and non-ASCII are refused. No AI, check digit or separator placement is inferred.
    /// [`Self::text`] retains the original input; ordinary Code128 encoding is unchanged.
    ///
    /// # Errors
    /// Returns a document limit error for excessive input or a named code error for empty
    /// or unrepresentable input.
    ///
    /// ```
    /// use erp_report::{Code, Limits};
    /// let code = Code::encode_gs1_code128("010950110102091710LOT-1", &Limits::default())?;
    /// assert_eq!(code.text(), "010950110102091710LOT-1");
    /// # Ok::<(), erp_report::ReportError>(())
    /// ```
    pub fn encode_gs1_code128(value: &str, limits: &Limits) -> Result<Self, ReportError> {
        if value.is_empty() {
            return Err(refuse(Symbology::Code128, "there is no value to encode"));
        }
        limits.check(LimitKind::CodeBytes, value.len())?;
        if !value
            .bytes()
            .all(|b| (0x20..=0x7e).contains(&b) || b == 0x1d)
        {
            return Err(refuse(
                Symbology::Code128,
                "GS1 data must contain printable ASCII or the ASCII GS separator (0x1D)",
            ));
        }
        let tokens: Vec<_> = std::iter::once(Code128Token::Fnc1)
            .chain(value.bytes().map(|b| {
                if b == 0x1d {
                    Code128Token::Fnc1
                } else {
                    Code128Token::Data(b)
                }
            }))
            .collect();
        let modules = code128_modules(code128_token_values(&tokens)?);
        Ok(Self {
            symbology: Symbology::Code128,
            columns: modules.len(),
            modules,
            quiet: [10, 10],
            text: value.to_owned(),
            bearer: None,
        })
    }

    /// Encode a GS1 element string as a square ECC200 Data Matrix, with leading FNC1.
    ///
    /// The caller constructs and validates application identifiers. Printable ASCII and ASCII
    /// GS (`0x1D`) separators are preserved; other characters are refused. No AI or check digit
    /// is inferred. An ordinary Data Matrix uses [`Self::encode`] instead.
    ///
    /// # Errors
    /// Returns a document limit error for excessive input, or a code error for empty/invalid
    /// input or a value that cannot fit a square ECC200 symbol.
    ///
    /// ```
    /// use erp_report::{Code, Limits};
    /// let code = Code::encode_gs1_datamatrix("010950110102091710LOT-1", &Limits::default())?;
    /// assert_eq!(code.text(), "010950110102091710LOT-1");
    /// # Ok::<(), erp_report::ReportError>(())
    /// ```
    pub fn encode_gs1_datamatrix(value: &str, limits: &Limits) -> Result<Self, ReportError> {
        if value.is_empty() {
            return Err(refuse(Symbology::DataMatrix, "there is no value to encode"));
        }
        limits.check(LimitKind::CodeBytes, value.len())?;
        if !value
            .bytes()
            .all(|b| (0x20..=0x7e).contains(&b) || b == 0x1d)
        {
            return Err(refuse(
                Symbology::DataMatrix,
                "GS1 data must contain printable ASCII or the ASCII GS separator (0x1D)",
            ));
        }
        Self::datamatrix(value, true)
    }

    // Both callers check CodeBytes before reaching the external encoder. Restricting it to
    // standard square ECC200 keeps the existing square fit contract and bounds the module map.
    fn datamatrix(value: &str, gs1: bool) -> Result<Self, ReportError> {
        let encoder = datamatrix::DataMatrixBuilder::new()
            .with_symbol_list(datamatrix::SymbolList::default().enforce_square())
            .with_macros(false)
            .with_fnc1_start(gs1);
        let encoded = if gs1 {
            encoder.encode(value.as_bytes())
        } else {
            encoder.encode_str(value)
        }
        .map_err(|error| match error {
            // The encoder also returns SymbolListEmpty after excluding sizes too small
            // for the payload; our initial square symbol list is always nonempty.
            datamatrix::data::DataEncodingError::TooMuchOrIllegalData
            | datamatrix::data::DataEncodingError::SymbolListEmpty => refuse(
                Symbology::DataMatrix,
                format!(
                    "{} bytes do not fit a square ECC200 Data Matrix",
                    value.len()
                ),
            ),
        })?;
        let bitmap = encoded.bitmap();
        let columns = bitmap.width();
        // 144 is the largest square ECC200 side, a format invariant, not an operator limit.
        if !(10..=144).contains(&columns) || bitmap.height() != columns {
            return Err(refuse(
                Symbology::DataMatrix,
                "encoder returned invalid square ECC200 dimensions",
            ));
        }
        let size = columns
            .checked_mul(columns)
            .ok_or_else(|| refuse(Symbology::DataMatrix, "ECC200 module count overflow"))?;
        let mut modules = vec![false; size];
        for (x, y) in bitmap.pixels() {
            if x >= columns || y >= columns {
                return Err(refuse(
                    Symbology::DataMatrix,
                    "encoder returned a module outside the ECC200 symbol",
                ));
            }
            modules[y * columns + x] = true;
        }
        Ok(Self {
            symbology: Symbology::DataMatrix,
            modules,
            columns,
            // ECC200 requires a one-module quiet zone on every side.
            quiet: [1; 2],
            text: value.to_owned(),
            bearer: None,
        })
    }
    pub fn symbology(&self) -> Symbology {
        self.symbology
    }
    /// What a person reads under the code: the value, with the check digit where one is computed.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Modules across the symbol, quiet zone included.
    pub fn modules_across(&self) -> usize {
        self.quiet[0] + self.columns + self.quiet[1]
    }
    /// Rows of modules: 1 for a linear symbol.
    pub fn rows(&self) -> usize {
        self.modules.len() / self.columns
    }
    /// Whether the module `column` of `row` is dark, counted without the quiet zone.
    pub fn is_dark(&self, column: usize, row: usize) -> bool {
        column < self.columns
            && self
                .modules
                .get(row * self.columns + column)
                .copied()
                .unwrap_or(false)
    }
    /// Drop the quiet zone, for a code printed where the layout already keeps the margin clear.
    pub fn without_quiet_zone(mut self) -> Self {
        self.quiet = [0, 0];
        self
    }
    /// The symbol at the largest module of `grid` that fits `width` × `height` points. A linear
    /// symbol takes the whole height; a matrix one is square. A box too small for the smallest
    /// module is refused with the width the symbol needs.
    pub fn fit(
        &self,
        width: f64,
        height: f64,
        grid: ModuleGrid,
    ) -> Result<CodeGraphic, ReportError> {
        let side_ok = |v: f64| v.is_finite() && v > 0.0 && v <= 14400.0;
        if !side_ok(width) || !side_ok(height) {
            return Err(refuse(
                self.symbology,
                "the box of a code must be in (0, 14400] points each way",
            ));
        }
        let matrix = self.symbology.is_matrix();
        let across = self.modules_across();
        let room = if matrix { width.min(height) } else { width };
        let dot = grid.dot();
        // The small term forgives the rounding of a box that is an exact number of dots.
        let mut dots = (room / (across as f64 * dot) + 1e-9).floor();
        // Reserve both bearer strips and at least one positive module of data-bar height.
        let bearer = self.bearer.map_or(0.0, f64::from);
        if self.bearer.is_some() {
            let minimum_height = (2.0 * bearer + 1.0) * f64::from(grid.smallest()) * dot;
            if height + 1e-9 < minimum_height {
                return Err(refuse(
                    self.symbology,
                    format!(
                        "bearer_modules {} needs at least {minimum_height:.2} points of height at {} dpi; got {height:.2}",
                        bearer,
                        grid.dpi()
                    ),
                ));
            }
            dots = dots.min((height / ((2.0 * bearer + 1.0) * dot) + 1e-9).floor());
        }
        let least = f64::from(grid.smallest());
        if dots < least {
            return Err(refuse(
                self.symbology,
                format!(
                    "`{}` is {across} modules and needs {:.1} points at the smallest module ({least} dots at {} dpi); its box gives {room:.1}",
                    self.text,
                    across as f64 * least * dot,
                    grid.dpi()
                ),
            ));
        }
        let module = dots * dot;
        // Vector units are modules, so every edge is a whole number and one scale sizes both ways.
        let tall = if matrix { 1.0 } else { height / module };
        // A box so tall for its module that the vector would pass its 14400-unit bound.
        if tall.is_nan() || tall > 14400.0 {
            return Err(refuse(
                self.symbology,
                format!(
                    "the box is {height:.1} points high for a module of {module:.2} points; the height must be at most 14400 modules"
                ),
            ));
        }
        let mut rectangles = Vec::new();
        for (y, row) in self.modules.chunks(self.columns).enumerate() {
            let top = if matrix {
                (self.quiet[0] + y) as f64
            } else {
                bearer
            };
            let mut x = 0;
            while x < row.len() {
                if !row[x] {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < row.len() && row[x] {
                    x += 1;
                }
                rectangles.push([
                    (self.quiet[0] + start) as f64,
                    top,
                    (x - start) as f64,
                    tall - 2.0 * bearer,
                ]);
            }
        }
        let high = if matrix { across as f64 } else { tall };
        if self.bearer.is_some() {
            let left = self.quiet[0] as f64;
            let span = self.columns as f64;
            rectangles.push([left, 0.0, span, bearer]);
            rectangles.push([left, high - bearer, span, bearer]);
        }
        let vector = Vector::rectangles(across as f64, high, &rectangles, BLACK)?;
        Ok(CodeGraphic {
            vector,
            width: across as f64 * module,
            height: high * module,
            module,
        })
    }
}

/// A code sized for print: the vector and the size it prints at, in points.
#[derive(Debug, Clone)]
pub struct CodeGraphic {
    vector: Vector,
    width: f64,
    height: f64,
    module: f64,
}
impl CodeGraphic {
    pub fn vector(&self) -> &Vector {
        &self.vector
    }
    pub fn into_vector(self) -> Vector {
        self.vector
    }
    pub fn width(&self) -> f64 {
        self.width
    }
    pub fn height(&self) -> f64 {
        self.height
    }
    /// The width of one module (the X-dimension), in points.
    pub fn module(&self) -> f64 {
        self.module
    }
}

/// Names the reference accepts for codes this library does not print.
const NOT_PRINTED: [&str; 8] = [
    "codabar",
    "code11",
    "extended93",
    "fim",
    "msi",
    "postnet",
    "standard93",
    "usps4state",
];
/// What a template asks of one code, in points.
pub(crate) struct CodeAsk<'a> {
    /// A symbology name or `auto`.
    pub(crate) symbology: &'a str,
    pub(crate) value: &'a str,
    pub(crate) width: f64,
    pub(crate) height: f64,
    /// The value as text under a linear code.
    pub(crate) human_readable: bool,
    pub(crate) quiet: bool,
    pub(crate) level: QrLevel,
    pub(crate) gs1: bool,
    pub(crate) bearer_modules: Option<u8>,
    pub(crate) grid: ModuleGrid,
}
/// A code as a document prints it.
#[derive(Debug, PartialEq)]
pub(crate) struct PrintedCode {
    pub(crate) vector: Vector,
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) alt: String,
    pub(crate) caption: Option<String>,
}
/// The code a template asked for. The text under a linear code is taken out of its height, so the
/// whole stays inside the box; a matrix code has no text.
pub(crate) fn print(ask: &CodeAsk<'_>, limits: &Limits) -> Result<PrintedCode, ReportError> {
    let key: String = ask
        .symbology
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let symbology = match Symbology::from_name(ask.symbology) {
        Some(symbology) => symbology,
        None if key == "auto" => Symbology::auto(ask.value),
        None => {
            let names: Vec<&str> = Symbology::ALL.iter().map(|s| s.name()).collect();
            let known = NOT_PRINTED.contains(&key.as_str());
            return Err(ReportError::Code {
                symbology: "barcode",
                reason: format!(
                    "symbology `{}` is {}; the symbologies are auto, {}",
                    ask.symbology,
                    if known { "not printed" } else { "unknown" },
                    names.join(", ")
                ),
            });
        }
    };
    if ask.gs1
        && (key == "auto" || !matches!(symbology, Symbology::Code128 | Symbology::DataMatrix))
    {
        return Err(ReportError::Code {
            symbology: "barcode",
            reason: "option `gs1` requires explicit Code128 or DataMatrix".into(),
        });
    }
    let mut code = if ask.gs1 {
        if symbology == Symbology::Code128 {
            Code::encode_gs1_code128(ask.value, limits)?
        } else {
            Code::encode_gs1_datamatrix(ask.value, limits)?
        }
    } else {
        Code::encode(symbology, ask.value, ask.level, limits)?
    };
    if let Some(modules) = ask.bearer_modules {
        code = code.with_bearer_modules(modules)?;
    }
    if !ask.quiet {
        code = code.without_quiet_zone();
    }
    let captioned = ask.human_readable && !symbology.is_matrix();
    let room = if captioned {
        crate::document::CODE_CAPTION_POINTS * 1.5
    } else {
        0.0
    };
    // Bars shorter than this are not worth printing under a text.
    const BARS_MIN: f64 = 4.0;
    if captioned && ask.height - room < BARS_MIN {
        return Err(refuse(
            symbology,
            format!(
                "its box is {:.1} points high and the text under the code takes {room:.1} of them",
                ask.height
            ),
        ));
    }
    let graphic = code.fit(ask.width, ask.height - room, ask.grid)?;
    // The alternative text of the reference: the word and the value.
    let escaped =
        symbology == Symbology::Extended39 || (ask.gs1 && symbology == Symbology::Code128);
    let display = |value: &str| {
        if escaped {
            control_text(value)
        } else {
            value.to_owned()
        }
    };
    let alt = format!("Barcode {}", display(ask.value));
    Ok(PrintedCode {
        width: graphic.width(),
        height: graphic.height(),
        vector: graphic.into_vector(),
        alt,
        caption: captioned.then(|| display(code.text())),
    })
}

fn control_text(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = String::new();
    for c in value.chars() {
        if c.is_ascii_control() {
            let b = c as u8;
            text.push_str("\\x");
            text.push(char::from(HEX[usize::from(b >> 4)]));
            text.push(char::from(HEX[usize::from(b & 15)]));
        } else {
            text.push(c);
        }
    }
    text
}

fn run(out: &mut Vec<bool>, dark: bool, width: usize) {
    out.extend(std::iter::repeat_n(dark, width));
}
fn bits(out: &mut Vec<bool>, pattern: u8, count: u32) {
    for i in (0..count).rev() {
        out.push(pattern >> i & 1 == 1);
    }
}
fn digits(symbology: Symbology, value: &str) -> Result<Vec<u8>, ReportError> {
    value
        .chars()
        .map(|c| match c {
            '0'..='9' => Ok(c as u8 - b'0'),
            _ => Err(refuse(
                symbology,
                format!("it prints digits only; `{value}` has `{c}`"),
            )),
        })
        .collect()
}
fn digit_text(digits: &[u8]) -> String {
    digits.iter().map(|d| char::from(b'0' + d)).collect()
}
/// The GS1 check digit of `body`: weights 3 and 1 from the right.
fn mod10(body: &[u8]) -> u8 {
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| u32::from(*d) * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    ((10 - sum % 10) % 10) as u8
}
/// The `length` digits of a symbol with a GS1 check digit: one digit fewer gets it computed, the
/// full length must end in the right one.
fn checked_digits(
    symbology: Symbology,
    value: &str,
    length: usize,
) -> Result<Vec<u8>, ReportError> {
    let mut d = digits(symbology, value)?;
    if d.len() + 1 == length {
        d.push(mod10(&d));
        return Ok(d);
    }
    match d.split_last() {
        Some((check, body)) if d.len() == length => {
            let computed = mod10(body);
            if computed != *check {
                return Err(refuse(
                    symbology,
                    format!(
                        "the check digit of `{value}` is {check}; its other digits give {computed}"
                    ),
                ));
            }
            Ok(d)
        }
        _ => Err(refuse(
            symbology,
            format!(
                "it takes {} digits (the check digit is computed) or {length} digits with it; `{value}` has {}",
                length - 1,
                d.len()
            ),
        )),
    }
}

/// The seven modules of each digit in set L of EAN and UPC. Set R is its complement, set G is R
/// read backwards.
const EAN_L: [u8; 10] = [
    0b000_1101, 0b001_1001, 0b001_0011, 0b011_1101, 0b010_0011, 0b011_0001, 0b010_1111, 0b011_1011,
    0b011_0111, 0b000_1011,
];
/// Which of the six left digits of an EAN-13 use set G, by its first digit (the high bit is the
/// first of the six).
const EAN_PARITY: [u8; 10] = [
    0b00_0000, 0b00_1011, 0b00_1101, 0b00_1110, 0b01_0011, 0b01_1001, 0b01_1100, 0b01_0101,
    0b01_0110, 0b01_1010,
];
/// Guard, left half (set L, or G where `parity` says so), centre guard, right half (set R), guard.
fn ean(left: &[u8], parity: u8, right: &[u8]) -> Vec<bool> {
    let mut out = Vec::with_capacity(11 + 7 * (left.len() + right.len()));
    bits(&mut out, 0b101, 3);
    for (i, digit) in left.iter().enumerate() {
        let l = EAN_L[usize::from(*digit)];
        let pattern = if parity >> (left.len() - 1 - i) & 1 == 1 {
            // seven bits read backwards
            (!l & 0x7f).reverse_bits() >> 1
        } else {
            l
        };
        bits(&mut out, pattern, 7);
    }
    bits(&mut out, 0b01010, 5);
    for digit in right {
        bits(&mut out, !EAN_L[usize::from(*digit)] & 0x7f, 7);
    }
    bits(&mut out, 0b101, 3);
    out
}

/// The wide elements of each digit of Interleaved 2 of 5 (two of five; the high bit is the first
/// element).
const TWO_OF_FIVE: [u8; 10] = [
    0b00110, 0b10001, 0b01001, 0b11000, 0b00101, 0b10100, 0b01100, 0b00011, 0b10010, 0b01010,
];
fn element(wide: u8) -> usize {
    if wide & 1 == 1 { WIDE } else { 1 }
}
/// Start, digit pairs (the first digit in the bars, the second in the spaces between them), stop.
fn interleaved(digits: &[u8]) -> Vec<bool> {
    let mut out = Vec::with_capacity(9 + digits.len() * 9);
    for dark in [true, false, true, false] {
        run(&mut out, dark, 1);
    }
    for [first, second] in digits.as_chunks::<2>().0 {
        let (bars, spaces) = (
            TWO_OF_FIVE[usize::from(*first)],
            TWO_OF_FIVE[usize::from(*second)],
        );
        for i in (0..5).rev() {
            run(&mut out, true, element(bars >> i));
            run(&mut out, false, element(spaces >> i));
        }
    }
    run(&mut out, true, WIDE);
    run(&mut out, false, 1);
    run(&mut out, true, 1);
    out
}

/// Code 39 characters with two wide bars and one wide space, in the order of the symbology's
/// table: ten per wide space, and within a ten the two wide bars count 1 to 10 (two of five).
const CODE39: &[u8; 40] = b"1234567890ABCDEFGHIJKLMNOPQRSTUVWXYZ-. *";
/// The wide space of each ten of `CODE39` (the high bit is the first of the four spaces).
const CODE39_SPACE: [u8; 4] = [0b0100, 0b0010, 0b0001, 0b1000];
/// One character: five bars and the four spaces between them. `false` for a character Code 39
/// does not have.
fn code39_character(out: &mut Vec<bool>, character: u8) -> bool {
    let (bars, spaces) = match CODE39.iter().position(|c| *c == character) {
        Some(i) => (TWO_OF_FIVE[(i % 10 + 1) % 10], CODE39_SPACE[i / 10]),
        // Four characters have three wide spaces and no wide bar.
        None => (
            0,
            match character {
                b'$' => 0b1110,
                b'/' => 0b1101,
                b'+' => 0b1011,
                b'%' => 0b0111,
                _ => return false,
            },
        ),
    };
    for i in (0..5).rev() {
        run(out, true, element(bars >> i));
        if i > 0 {
            run(out, false, element(spaces >> (i - 1)));
        }
    }
    true
}
/// `*`, the value, `*`, a narrow gap between characters. No check character.
fn code39(value: &str) -> Result<Vec<bool>, ReportError> {
    let mut out = Vec::with_capacity((value.len() + 2) * 16);
    code39_character(&mut out, b'*');
    for c in value.chars() {
        run(&mut out, false, 1);
        let known = c != '*' && c.is_ascii() && code39_character(&mut out, c as u8);
        if !known {
            return Err(refuse(
                Symbology::Code39,
                format!(
                    "it prints digits, capital letters, space and `-.$/+%`; `{value}` has `{c}`"
                ),
            ));
        }
    }
    run(&mut out, false, 1);
    code39_character(&mut out, b'*');
    Ok(out)
}

/// Full ASCII pairs; the encoded alphabet is passed through our ordinary Code39 bars.
fn extended39(value: &str) -> Result<Vec<bool>, ReportError> {
    if let Some(c) = value.chars().find(|c| !c.is_ascii()) {
        return Err(refuse(
            Symbology::Extended39,
            format!("it prints ASCII characters; `{value}` has `{c}`"),
        ));
    }
    let capacity = value
        .len()
        .checked_mul(2)
        .ok_or_else(|| refuse(Symbology::Extended39, "ASCII expansion length overflow"))?;
    let mut expanded = String::with_capacity(capacity);
    for b in value.bytes() {
        let pair = match b {
            0 => Some((b'%', b'U')),
            1..=26 => Some((b'$', b'A' + b - 1)),
            27..=31 => Some((b'%', b'A' + b - 27)),
            b'!'..=b',' => Some((b'/', b'A' + b - b'!')),
            b'/' => Some((b'/', b'O')),
            b':' => Some((b'/', b'Z')),
            b';'..=b'?' => Some((b'%', b'F' + b - b';')),
            b'@' => Some((b'%', b'V')),
            b'['..=b'_' => Some((b'%', b'K' + b - b'[')),
            b'`' => Some((b'%', b'W')),
            b'a'..=b'z' => Some((b'+', b'A' + b - b'a')),
            b'{'..=127 => Some((b'%', b'P' + b - b'{')),
            // Remaining ASCII is space, hyphen, period, digits and uppercase letters.
            _ => None,
        };
        if let Some((prefix, suffix)) = pair {
            expanded.push(char::from(prefix));
            expanded.push(char::from(suffix));
        } else {
            expanded.push(char::from(b));
        }
    }
    code39(&expanded)
}

/// Widths of the bars and spaces of each Code 128 symbol value, bar first; 103 to 105 start in
/// code set A, B and C, 106 is the stop.
const CODE128: [&str; 107] = [
    "212222", "222122", "222221", "121223", "121322", "131222", "122213", "122312", "132212",
    "221213", "221312", "231212", "112232", "122132", "122231", "113222", "123122", "123221",
    "223211", "221132", "221231", "213212", "223112", "312131", "311222", "321122", "321221",
    "312212", "322112", "322211", "212123", "212321", "232121", "111323", "131123", "131321",
    "112313", "132113", "132311", "211313", "231113", "231311", "112133", "112331", "132131",
    "113123", "113321", "133121", "313121", "211331", "231131", "213113", "213311", "213131",
    "311123", "311321", "331121", "312113", "312311", "332111", "314111", "221411", "431111",
    "111224", "111422", "121124", "121421", "141122", "141221", "112214", "112412", "122114",
    "122411", "142112", "142211", "241211", "221114", "413111", "241112", "134111", "111242",
    "121142", "121241", "114212", "124112", "124211", "411212", "421112", "421211", "212141",
    "214121", "412121", "111143", "111341", "131141", "114113", "114311", "411113", "411311",
    "113141", "114131", "311141", "411131", "211412", "211214", "211232", "2331112",
];
const CODE128_STOP: u8 = 106;
/// The symbol that switches to code set A, B and C.
const CODE128_SWITCH: [u8; 3] = [101, 100, 99];
/// A function symbol is distinct from every ASCII data byte, including GS.
#[derive(Clone, Copy)]
enum Code128Token {
    Data(u8),
    Fnc1,
}

/// Symbol and consumed tokens for a code set (A, B, C). FNC1 is one symbol in every set.
fn code128_step(data: &[Code128Token], i: usize, set: usize) -> Option<(u8, usize)> {
    match data.get(i)? {
        Code128Token::Fnc1 => Some((102, 1)),
        Code128Token::Data(byte) => match set {
            0 if *byte < 96 => Some((if *byte < 32 { *byte + 64 } else { *byte - 32 }, 1)),
            1 if (32..128).contains(byte) => Some((*byte - 32, 1)),
            2 if byte.is_ascii_digit() => match data.get(i + 1) {
                Some(Code128Token::Data(second)) if second.is_ascii_digit() => {
                    Some(((*byte - b'0') * 10 + (*second - b'0'), 2))
                }
                // A function character or non-digit cannot complete a numeric pair.
                _ => None,
            },
            // This token is not representable in the requested set.
            _ => None,
        },
    }
}

/// Fewest symbols across sets A/B/C, including real function characters and weighted checksum.
fn code128_token_values(data: &[Code128Token]) -> Result<Vec<u8>, ReportError> {
    const NEVER: u32 = u32::MAX / 4;
    let n = data.len();
    // CodeBytes admission bounds n before this private function is called.
    let mut cost = vec![[NEVER; 3]; n + 1];
    cost[n] = [0; 3];
    let direct = |cost: &[[u32; 3]], i: usize, set: usize| {
        code128_step(data, i, set).map_or(NEVER, |(_, taken)| 1 + cost[i + taken][set])
    };
    for i in (0..n).rev() {
        let stay: [u32; 3] = std::array::from_fn(|set| direct(&cost, i, set));
        cost[i] = std::array::from_fn(|set| {
            let mut best = stay[set];
            for (other, value) in stay.iter().enumerate() {
                if other != set {
                    best = best.min(1 + value);
                }
            }
            best
        });
    }
    // Prefer B then C then A on ties, preserving ordinary Code128 output.
    let mut set = 1;
    for candidate in [2, 0] {
        if cost[0][candidate] < cost[0][set] {
            set = candidate;
        }
    }
    let mut values = vec![103 + set as u8];
    let mut i = 0;
    while i < n {
        if direct(&cost, i, set) != cost[i][set] {
            let mut next = None;
            let mut best = NEVER;
            for candidate in [1, 2, 0] {
                let price = direct(&cost, i, candidate);
                if candidate != set && price < best {
                    next = Some(candidate);
                    best = price;
                }
            }
            let selected = next.ok_or_else(|| {
                refuse(Symbology::Code128, "no code set can encode the next token")
            })?;
            values.push(CODE128_SWITCH[selected]);
            set = selected;
        }
        let (symbol, taken) = code128_step(data, i, set)
            .ok_or_else(|| refuse(Symbology::Code128, "no code set can encode the next token"))?;
        values.push(symbol);
        i += taken;
    }
    // Reduce each contribution modulo 103; input admission also bounds all positions.
    let sum = values
        .iter()
        .enumerate()
        .fold(0usize, |sum, (position, value)| {
            (sum + (position.max(1) % 103) * usize::from(*value)) % 103
        });
    values.push(sum as u8);
    Ok(values)
}

fn code128_values(data: &[u8]) -> Result<Vec<u8>, ReportError> {
    let tokens: Vec<_> = data.iter().copied().map(Code128Token::Data).collect();
    code128_token_values(&tokens)
}

fn code128_modules(values: Vec<u8>) -> Vec<bool> {
    let mut out = Vec::new();
    for value in values.into_iter().chain([CODE128_STOP]) {
        let mut dark = true;
        for width in CODE128[usize::from(value)].bytes() {
            run(&mut out, dark, usize::from(width - b'0'));
            dark = !dark;
        }
    }
    out
}

fn code128(value: &str) -> Result<Vec<bool>, ReportError> {
    if let Some(c) = value.chars().find(|c| !c.is_ascii()) {
        return Err(refuse(
            Symbology::Code128,
            format!("it prints ASCII characters; `{value}` has `{c}`"),
        ));
    }
    Ok(code128_modules(code128_values(value.as_bytes())?))
}

/// The modules of a QR code of `value` in byte mode at exactly `level`, and its side.
fn qr(value: &str, level: QrLevel) -> Result<(Vec<bool>, usize), ReportError> {
    use qrcodegen::{QrCode, QrCodeEcc, QrSegment, Version};
    let ecc = match level {
        QrLevel::Low => QrCodeEcc::Low,
        QrLevel::Medium => QrCodeEcc::Medium,
        QrLevel::Quartile => QrCodeEcc::Quartile,
        QrLevel::High => QrCodeEcc::High,
    };
    let segment = QrSegment::make_bytes(value.as_bytes());
    // The level is not raised when a higher one would fit: the template chose it.
    let code =
        QrCode::encode_segments_advanced(&[segment], ecc, Version::MIN, Version::MAX, None, false)
            .map_err(|_| {
                refuse(
                    Symbology::Qr,
                    format!(
                        "{} bytes do not fit the largest QR code at error correction level {}",
                        value.len(),
                        level.name()
                    ),
                )
            })?;
    let side = code.size();
    let mut modules = Vec::with_capacity((side * side) as usize);
    for y in 0..side {
        for x in 0..side {
            modules.push(code.get_module(x, y));
        }
    }
    Ok((modules, side as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every symbol of the Code 128 table is 11 modules (the stop 13) with an even number of dark
    /// ones, and no two are alike: a mistyped row breaks one of the three.
    #[test]
    fn code128_table_is_well_formed() {
        let mut seen = std::collections::BTreeSet::new();
        for (value, widths) in CODE128.iter().enumerate() {
            let w: Vec<u32> = widths.bytes().map(|b| u32::from(b - b'0')).collect();
            let total: u32 = w.iter().sum();
            let dark: u32 = w.iter().step_by(2).sum();
            assert_eq!(total, if value == 106 { 13 } else { 11 }, "value {value}");
            assert_eq!(dark % 2, 0, "value {value}");
            assert!(w.iter().all(|x| (1..=4).contains(x)), "value {value}");
            assert!(seen.insert(*widths), "value {value} repeats");
        }
    }
    /// Symbol values worked out by hand from the code set tables and the modulo 103 rule: plain
    /// text in set B, digit pairs in set C (from the start, or after a switch when that is
    /// shorter), a control character in set A.
    #[test]
    fn code128_values_use_the_shortest_code_sets() {
        assert_eq!(
            code128_values(b"PJJ123C").unwrap(),
            [104, 48, 42, 42, 17, 18, 19, 35, 55]
        );
        assert_eq!(
            code128_values(b"0123456789").unwrap(),
            [105, 1, 23, 45, 67, 89, 73]
        );
        assert_eq!(
            code128_values(b"AB1234").unwrap(),
            [104, 33, 34, 99, 12, 34, 102]
        );
        assert_eq!(code128_values(b"A\tB").unwrap(), [103, 33, 73, 34, 75]);
    }
    /// Every Code 39 character is 15 modules with three wide elements of nine, and no two are
    /// alike.
    #[test]
    fn code39_table_is_well_formed() {
        let mut seen = std::collections::BTreeSet::new();
        for c in CODE39.iter().chain(b"$/+%") {
            let mut out = Vec::new();
            assert!(code39_character(&mut out, *c));
            assert_eq!(out.len(), 6 + 3 * WIDE, "{}", *c as char);
            assert!(seen.insert(out), "{} repeats", *c as char);
        }
        assert_eq!(seen.len(), 44);
    }
}
