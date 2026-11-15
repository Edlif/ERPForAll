// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::error::{Result, invalid};
/// Caller-supplied calendar date and time with a UTC offset; the library never reads a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PdfDate {
    y: u16,
    mo: u8,
    d: u8,
    h: u8,
    mi: u8,
    s: u8,
    off: i16,
}
impl PdfDate {
    /// Validate a Gregorian date-time (year 1000..=9999) and an offset of at most 14 hours.
    pub fn new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        offset_minutes: i16,
    ) -> Result<Self> {
        let leap =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
        if !(1000..=9999).contains(&year)
            || day == 0
            || day > days
            || hour > 23
            || minute > 59
            || second > 59
            || offset_minutes.unsigned_abs() > 14 * 60
        {
            return Err(invalid(format!(
                "PDF date {year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02} \
                 offset {offset_minutes} minutes is invalid"
            )));
        }
        Ok(Self {
            y: year,
            mo: month,
            d: day,
            h: hour,
            mi: minute,
            s: second,
            off: offset_minutes,
        })
    }
    fn offset(&self, sep: &str, tail: &str) -> String {
        if self.off == 0 {
            return "Z".into();
        }
        let sign = if self.off < 0 { '-' } else { '+' };
        let a = self.off.unsigned_abs();
        format!("{sign}{:02}{sep}{:02}{tail}", a / 60, a % 60)
    }
    /// PDF date string, e.g. `D:20261115093000+07'00'`.
    pub fn to_pdf_string(&self) -> String {
        format!(
            "D:{:04}{:02}{:02}{:02}{:02}{:02}{}",
            self.y,
            self.mo,
            self.d,
            self.h,
            self.mi,
            self.s,
            self.offset("'", "'")
        )
    }
    /// XMP (ISO 8601) form, e.g. `2026-11-15T09:30:00+07:00`.
    pub fn to_xmp_string(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}",
            self.y,
            self.mo,
            self.d,
            self.h,
            self.mi,
            self.s,
            self.offset(":", "")
        )
    }
    /// Parse the full PDF date form (seconds and offset present); partial forms return `None`.
    pub(crate) fn parse_pdf(s: &str) -> Option<Self> {
        let b = s.strip_prefix("D:")?.as_bytes();
        let num = |r: std::ops::Range<usize>| -> Option<u16> {
            std::str::from_utf8(b.get(r)?).ok()?.parse().ok()
        };
        let small = |r| num(r).and_then(|v| u8::try_from(v).ok());
        if !b.get(..14)?.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let off = match b.get(14..)? {
            b"Z" | b"Z00'00'" | b"Z00'00" => 0,
            rest if rest.len() >= 6 && matches!(rest[0], b'+' | b'-') && rest[3] == b'\'' => {
                if !matches!(&rest[6..], b"" | b"'") {
                    return None;
                }
                let v = i16::try_from(num(15..17)? * 60 + num(18..20)?).ok()?;
                if rest[0] == b'-' { -v } else { v }
            }
            _ => return None,
        };
        Self::new(
            num(0..4)?,
            small(4..6)?,
            small(6..8)?,
            small(8..10)?,
            small(10..12)?,
            small(12..14)?,
            off,
        )
        .ok()
    }
}
/// Report PDF dates in their XMP form when they use the full syntax; other values stay verbatim.
pub(crate) fn normalize(s: String) -> String {
    PdfDate::parse_pdf(&s).map_or(s, |d| d.to_xmp_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn date_round_trip_and_refusals() {
        let d = PdfDate::new(2024, 2, 29, 23, 59, 58, -330).unwrap();
        assert_eq!(d.to_pdf_string(), "D:20240229235958-05'30'");
        assert_eq!(d.to_xmp_string(), "2024-02-29T23:59:58-05:30");
        assert_eq!(PdfDate::parse_pdf(&d.to_pdf_string()), Some(d));
        let z = PdfDate::new(2026, 1, 1, 0, 0, 0, 0).unwrap();
        assert_eq!(PdfDate::parse_pdf(&z.to_pdf_string()), Some(z));
        assert!(PdfDate::new(2023, 2, 29, 0, 0, 0, 0).is_err());
        assert!(PdfDate::new(2026, 13, 1, 0, 0, 0, 0).is_err());
        assert!(PdfDate::new(2026, 1, 1, 0, 0, 0, 900).is_err());
        assert_eq!(PdfDate::parse_pdf("D:2026"), None);
    }
}
