// SPDX-License-Identifier: MIT OR Apache-2.0
//! `report.paperformat`: the page a report prints on (
//! ).
//!
//! A paper format names a standard size or gives its own width and height, an orientation, the
//! four margins, the space kept for the running header and the resolution images are placed at.
//! A report action points at one (`ir.actions.report.paperformat_id`), a company at its default
//! (`res.company.paperformat_id`); `ir.actions.report::paperformat_of` resolves between them.

use erp_sdk::prelude::*;

/// A standard paper size: selection key, width and height in millimetres, portrait.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaperSize {
    /// The `format` selection key.
    pub key: &'static str,
    /// Width in millimetres.
    pub width_mm: f64,
    /// Height in millimetres.
    pub height_mm: f64,
}

/// The `format` key of a size the record gives itself (`page_width`, `page_height`).
pub const CUSTOM_FORMAT: &str = "custom";

/// Every standard size the `format` selection offers (ISO 216 A and B series, the envelope and
/// North American sizes), portrait; `custom` is not in the table. The keys are the ones report
/// data files of the reference use, so their records load unchanged.
pub const PAPER_SIZES: [PaperSize; 30] = [
    size("A0", 841.0, 1189.0),
    size("A1", 594.0, 841.0),
    size("A2", 420.0, 594.0),
    size("A3", 297.0, 420.0),
    size("A4", 210.0, 297.0),
    size("A5", 148.0, 210.0),
    size("A6", 105.0, 148.0),
    size("A7", 74.0, 105.0),
    size("A8", 52.0, 74.0),
    size("A9", 37.0, 52.0),
    size("B0", 1000.0, 1414.0),
    size("B1", 707.0, 1000.0),
    size("B2", 500.0, 707.0),
    size("B3", 353.0, 500.0),
    size("B4", 250.0, 353.0),
    size("B5", 176.0, 250.0),
    size("B6", 125.0, 176.0),
    size("B7", 88.0, 125.0),
    size("B8", 62.0, 88.0),
    size("B9", 33.0, 62.0),
    size("B10", 31.0, 44.0),
    size("C5E", 163.0, 229.0),
    size("Comm10E", 105.0, 241.0),
    size("DLE", 110.0, 220.0),
    size("Executive", 190.5, 254.0),
    size("Folio", 210.0, 330.0),
    size("Ledger", 431.8, 279.4),
    size("Legal", 215.9, 355.6),
    size("Letter", 215.9, 279.4),
    size("Tabloid", 279.4, 431.8),
];

const fn size(key: &'static str, width_mm: f64, height_mm: f64) -> PaperSize {
    PaperSize {
        key,
        width_mm,
        height_mm,
    }
}

/// The standard size of the `format` key `key`; `None` for `custom` and for an unknown key.
pub fn paper_size(key: &str) -> Option<PaperSize> {
    PAPER_SIZES.iter().copied().find(|size| size.key == key)
}

/// The printed page of a format in millimetres, `(width, height)`: the standard size or the
/// record's own, swapped when the orientation is `Landscape`. A record
/// without a format has no page: `(0, 0)`, as in the reference.
///
/// # Errors
///
/// `Programming` for a `format` that is neither `custom` nor in [`PAPER_SIZES`]: the selection
/// and the table are out of step.
pub fn print_page_size(
    format: Option<&str>,
    page_width: i32,
    page_height: i32,
    orientation: Option<&str>,
) -> Result<(f64, f64)> {
    let (width, height) = match format {
        None => (0.0, 0.0),
        Some(CUSTOM_FORMAT) => (f64::from(page_width), f64::from(page_height)),
        Some(key) => {
            let size = paper_size(key).ok_or_else(|| {
                Error::programming(format!(
                    "report.paperformat.format '{key}' has no entry in PAPER_SIZES"
                ))
            })?;
            (size.width_mm, size.height_mm)
        }
    };
    Ok(if orientation == Some("Landscape") {
        (height, width)
    } else {
        (width, height)
    })
}

/// `report.paperformat`.
#[model(
    name = "report.paperformat",
    description = "Paper Format Config",
    order = "name, id",
    constraints = [
        ("_dpi_positive", "CHECK (dpi > 0)", "The output DPI must be above zero."),
        (
            "_margins_not_negative",
            "CHECK (margin_top >= 0 AND margin_bottom >= 0 AND margin_left >= 0 AND margin_right >= 0)",
            "A paper format margin cannot be negative."
        ),
        (
            "_page_size_not_negative",
            "CHECK (page_width >= 0 AND page_height >= 0)",
            "A paper format page size cannot be negative."
        ),
        (
            "_header_spacing_not_negative",
            "CHECK (header_spacing >= 0)",
            "The header spacing cannot be negative."
        ),
    ],
)]
pub struct ReportPaperformat {
    /// Display name.
    #[field(char, required = true, string = "Name")]
    pub name: Option<Arc<str>>,
    /// Marks the formats offered as a default (the reference's flag; the resolution reads the
    /// company's format, not this flag).
    #[field(boolean, string = "Default paper format?")]
    pub default: bool,
    /// A standard size, or `custom` for the record's own width and height.
    #[field(
        selection,
        selection = [
            ("A0", "A0 (841 x 1189 mm)"),
            ("A1", "A1 (594 x 841 mm)"),
            ("A2", "A2 (420 x 594 mm)"),
            ("A3", "A3 (297 x 420 mm)"),
            ("A4", "A4 (210 x 297 mm)"),
            ("A5", "A5 (148 x 210 mm)"),
            ("A6", "A6 (105 x 148 mm)"),
            ("A7", "A7 (74 x 105 mm)"),
            ("A8", "A8 (52 x 74 mm)"),
            ("A9", "A9 (37 x 52 mm)"),
            ("B0", "B0 (1000 x 1414 mm)"),
            ("B1", "B1 (707 x 1000 mm)"),
            ("B2", "B2 (500 x 707 mm)"),
            ("B3", "B3 (353 x 500 mm)"),
            ("B4", "B4 (250 x 353 mm)"),
            ("B5", "B5 (176 x 250 mm)"),
            ("B6", "B6 (125 x 176 mm)"),
            ("B7", "B7 (88 x 125 mm)"),
            ("B8", "B8 (62 x 88 mm)"),
            ("B9", "B9 (33 x 62 mm)"),
            ("B10", "B10 (31 x 44 mm)"),
            ("C5E", "C5 envelope (163 x 229 mm)"),
            ("Comm10E", "US #10 envelope (105 x 241 mm)"),
            ("DLE", "DL envelope (110 x 220 mm)"),
            ("Executive", "Executive (190.5 x 254 mm)"),
            ("Folio", "Folio (210 x 330 mm)"),
            ("Ledger", "Ledger (431.8 x 279.4 mm)"),
            ("Legal", "Legal (215.9 x 355.6 mm)"),
            ("Letter", "Letter (215.9 x 279.4 mm)"),
            ("Tabloid", "Tabloid (279.4 x 431.8 mm)"),
            ("custom", "Custom")
        ],
        default = "A4",
        string = "Paper size"
    )]
    pub format: Option<Arc<str>>,
    /// Top margin in millimetres.
    #[field(float, default = 40.0, string = "Top Margin (mm)")]
    pub margin_top: f64,
    /// Bottom margin in millimetres.
    #[field(float, default = 20.0, string = "Bottom Margin (mm)")]
    pub margin_bottom: f64,
    /// Left margin in millimetres.
    #[field(float, default = 7.0, string = "Left Margin (mm)")]
    pub margin_left: f64,
    /// Right margin in millimetres.
    #[field(float, default = 7.0, string = "Right Margin (mm)")]
    pub margin_right: f64,
    /// Page height in millimetres of a `custom` format; zero otherwise.
    #[field(integer, string = "Page height (mm)")]
    pub page_height: i32,
    /// Page width in millimetres of a `custom` format; zero otherwise.
    #[field(integer, string = "Page width (mm)")]
    pub page_width: i32,
    /// `Portrait` or `Landscape`.
    #[field(
        selection,
        selection = [("Landscape", "Landscape"), ("Portrait", "Portrait")],
        default = "Landscape",
        string = "Orientation"
    )]
    pub orientation: Option<Arc<str>>,
    /// A rule under the running header.
    #[field(boolean, string = "Display a header line")]
    pub header_line: bool,
    /// Space kept for the running header, in millimetres.
    #[field(integer, default = 35, string = "Header spacing")]
    pub header_spacing: i32,
    /// Kept for data files of the reference; the native engine never shrinks content.
    #[field(boolean, string = "Disable smart shrinking")]
    pub disable_shrinking: bool,
    /// Resolution images are placed at.
    #[field(integer, required = true, default = 90, string = "Output DPI")]
    pub dpi: i32,
    /// The reports that name this format.
    #[field(
        one2many,
        comodel = "ir.actions.report",
        inverse_name = "paperformat_id",
        string = "Associated reports"
    )]
    pub report_ids: Vec<RecId>,
    /// Printed page width in millimetres, orientation applied.
    #[field(
        float,
        compute = "_compute_print_page_size",
        string = "Print page width (mm)"
    )]
    pub print_page_width: f64,
    /// Printed page height in millimetres, orientation applied.
    #[field(
        float,
        compute = "_compute_print_page_size",
        string = "Print page height (mm)"
    )]
    pub print_page_height: f64,
    /// Kept for data files of the reference: the margins of the record are always the page
    /// margins in the native engine.
    #[field(boolean, string = "Use css margins")]
    pub css_margins: bool,
}

/// The message of the format/size exclusivity (the reference's wording).
pub const FORMAT_OR_SIZE: &str = "Choose a paper size or a custom width and height, not both.";

/// The message of a `custom` format without a usable size.
pub const CUSTOM_NEEDS_SIZE: &str =
    "A custom paper format needs a page width and a page height above zero.";

#[methods(ReportPaperformat, addon = "base")]
impl ReportPaperformat {
    /// The printed page size: [`print_page_size`].
    #[compute(depends = ["format", "page_width", "page_height", "orientation"])]
    fn _compute_print_page_size(batch: &mut Batch<'_, ReportPaperformat>) -> Result<()> {
        for i in batch.indices() {
            let format = batch.get(i, ReportPaperformat::FORMAT)?;
            let orientation = batch.get(i, ReportPaperformat::ORIENTATION)?;
            let page_width = batch.get(i, ReportPaperformat::PAGE_WIDTH)?;
            let page_height = batch.get(i, ReportPaperformat::PAGE_HEIGHT)?;
            let (width, height) = print_page_size(
                format.as_deref(),
                page_width,
                page_height,
                orientation.as_deref(),
            )?;
            batch.set(i, ReportPaperformat::PRINT_PAGE_WIDTH, width)?;
            batch.set(i, ReportPaperformat::PRINT_PAGE_HEIGHT, height)?;
        }
        Ok(())
    }

    /// A standard format has no size of its own, and a `custom` one needs both.
    #[constrains("format", "page_width", "page_height")]
    async fn _check_format_or_page(
        this: &RecordSet<ReportPaperformat>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        let formats = records.values(tx, "format").await?;
        let widths = records.values(tx, "page_width").await?;
        let heights = records.values(tx, "page_height").await?;
        for ((format, width), height) in formats.iter().zip(&widths).zip(&heights) {
            let sized = |value: &Value| match value {
                Value::Int(n) => Ok(*n > 0),
                Value::False => Ok(false),
                other => Err(Error::programming(format!(
                    "report.paperformat page size read as {other:?}, expected an integer"
                ))),
            };
            let (has_width, has_height) = (sized(width)?, sized(height)?);
            match format {
                Value::Str(key) if &**key == CUSTOM_FORMAT => {
                    if !(has_width && has_height) {
                        return Err(Error::validation(CUSTOM_NEEDS_SIZE));
                    }
                }
                Value::Str(_) => {
                    if has_width || has_height {
                        return Err(Error::validation(FORMAT_OR_SIZE));
                    }
                }
                // no format: no page; the reference accepts it and so do we (a report that
                // resolves to it is refused when it prints)
                Value::False => {}
                other => {
                    return Err(Error::programming(format!(
                        "report.paperformat.format read as {other:?}, expected text"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_sizes_resolve_in_portrait() {
        assert_eq!(
            print_page_size(Some("A4"), 0, 0, Some("Portrait")).unwrap(),
            (210.0, 297.0)
        );
        assert_eq!(
            print_page_size(Some("Letter"), 0, 0, Some("Portrait")).unwrap(),
            (215.9, 279.4)
        );
    }

    #[test]
    fn custom_size_is_the_records_own() {
        assert_eq!(
            print_page_size(Some("custom"), 100, 150, Some("Portrait")).unwrap(),
            (100.0, 150.0)
        );
    }

    #[test]
    fn landscape_swaps_width_and_height() {
        assert_eq!(
            print_page_size(Some("A4"), 0, 0, Some("Landscape")).unwrap(),
            (297.0, 210.0)
        );
        assert_eq!(
            print_page_size(Some("custom"), 100, 150, Some("Landscape")).unwrap(),
            (150.0, 100.0)
        );
    }

    #[test]
    fn no_format_has_no_page_and_an_unknown_key_is_refused() {
        assert_eq!(print_page_size(None, 10, 20, None).unwrap(), (0.0, 0.0));
        let error = print_page_size(Some("A11"), 0, 0, None).unwrap_err();
        assert_eq!(error.kind(), erp_sdk::ErrorKind::Programming);
        assert_eq!(
            error.message(),
            "report.paperformat.format 'A11' has no entry in PAPER_SIZES"
        );
    }

    #[test]
    fn size_keys_are_unique_and_never_custom() {
        for (i, size) in PAPER_SIZES.iter().enumerate() {
            assert_ne!(size.key, CUSTOM_FORMAT);
            assert!(size.width_mm > 0.0 && size.height_mm > 0.0, "{}", size.key);
            assert!(
                PAPER_SIZES[i + 1..]
                    .iter()
                    .all(|other| other.key != size.key),
                "{} twice",
                size.key
            );
        }
    }
}
