// SPDX-License-Identifier: MIT OR Apache-2.0
//! Number and date formatting by a language's settings (the `res.lang` row of the document's
//! language, read by the data stage). No locale database is consulted here.
use crate::{ReportError, error::invalid, program::FORMATS_TASK};

/// How one language writes numbers, dates and times.
#[derive(Debug, Clone)]
pub struct LangFormat {
    decimal_point: String,
    thousands_sep: String,
    grouping: Vec<i8>,
    date_format: String,
    time_format: String,
}
impl Default for LangFormat {
    /// The settings of `en_US`.
    fn default() -> Self {
        Self {
            decimal_point: ".".into(),
            thousands_sep: ",".into(),
            grouping: vec![3, 0],
            date_format: "%m/%d/%Y".into(),
            time_format: "%H:%M:%S".into(),
        }
    }
}
impl LangFormat {
    /// `grouping` lists digit group sizes from the right: `0` repeats the size before it, `-1` ends
    /// grouping, and a list that ends without either groups no further. The two formats use the
    /// directives `%d %m %Y %y %H %M %S %I %p %j %%`.
    pub fn new(
        decimal_point: &str,
        thousands_sep: &str,
        grouping: &[i8],
        date_format: &str,
        time_format: &str,
    ) -> Result<Self, ReportError> {
        if decimal_point.is_empty() || decimal_point.len() > 8 || thousands_sep.len() > 8 {
            return Err(invalid(
                "decimal point must be 1..=8 bytes and the thousands separator at most 8",
            ));
        }
        if grouping.len() > 8 || grouping.iter().any(|n| !(-1..=9).contains(n)) {
            return Err(invalid("digit grouping takes at most 8 sizes, each -1..=9"));
        }
        for pattern in [date_format, time_format] {
            if pattern.is_empty() || pattern.len() > 64 {
                return Err(invalid("date and time formats must be 1..=64 bytes"));
            }
            let mut chars = pattern.chars();
            while let Some(c) = chars.next() {
                if c != '%' {
                    continue;
                }
                match chars.next() {
                    Some('d' | 'm' | 'Y' | 'y' | 'H' | 'M' | 'S' | 'I' | 'p' | 'j' | '%') => {}
                    Some('a' | 'A' | 'b' | 'B') => {
                        return Err(ReportError::Unsupported {
                            feature: "month and weekday names in date formats",
                            task: FORMATS_TASK,
                        });
                    }
                    _ => return Err(invalid("date format directive is not supported")),
                }
            }
        }
        Ok(Self {
            decimal_point: decimal_point.into(),
            thousands_sep: thousands_sep.into(),
            grouping: grouping.to_vec(),
            date_format: date_format.into(),
            time_format: time_format.into(),
        })
    }
    pub(crate) fn date_format(&self) -> &str {
        &self.date_format
    }
    pub(crate) fn time_format(&self) -> &str {
        &self.time_format
    }
    /// Group the digits of an integer part.
    fn group(&self, digits: &str) -> String {
        if self.thousands_sep.is_empty() {
            return digits.into();
        }
        let mut parts = Vec::new();
        let mut rest = digits;
        let mut sizes = self.grouping.iter().copied();
        let mut size = 0usize;
        let mut repeat = false;
        loop {
            if !repeat {
                match sizes.next() {
                    None | Some(-1) => break,
                    Some(0) => repeat = true,
                    Some(n) => size = usize::try_from(n).unwrap_or(0),
                }
            }
            if size == 0 || rest.len() <= size {
                break;
            }
            let (head, tail) = rest.split_at(rest.len() - size);
            parts.push(tail);
            rest = head;
        }
        parts.push(rest);
        parts.reverse();
        parts.join(&self.thousands_sep)
    }
    /// Join a split number: grouped integer digits, the decimal point, the decimals.
    pub(crate) fn number(&self, negative: bool, integer: &str, decimals: &str) -> String {
        let mut out = String::new();
        if negative {
            out.push('-');
        }
        out.push_str(&self.group(integer));
        if !decimals.is_empty() {
            out.push_str(&self.decimal_point);
            out.push_str(decimals);
        }
        out
    }
    /// An integer with digit grouping.
    pub(crate) fn integer(&self, value: i64) -> String {
        self.number(value < 0, &value.unsigned_abs().to_string(), "")
    }
}
/// Round `value` half away from zero at `digits` decimals and split it into sign, integer digits
/// and decimal digits. A value that is zero after rounding has no sign.
pub(crate) fn split(value: f64, digits: usize) -> Result<(bool, String, String), ReportError> {
    if !value.is_finite() || digits > 14 {
        return Err(invalid(
            "a number to format must be finite, with at most 14 decimals",
        ));
    }
    let scaled = value.abs() * 10f64.powi(i32::try_from(digits).unwrap_or(14));
    // The margin keeps 2.675 at two decimals from rounding down on its binary representation.
    let rounded = (scaled + scaled * f64::EPSILON + 0.5).floor();
    if rounded >= 1e30 {
        return Err(invalid("a number to format is too large"));
    }
    let all = format!("{rounded:.0}");
    let all = format!("{all:0>width$}", width = digits + 1);
    let (integer, decimals) = all.split_at(all.len() - digits);
    Ok((
        value < 0.0 && rounded != 0.0,
        integer.into(),
        decimals.into(),
    ))
}
/// Write a date and time by a `strftime`-style pattern whose directives [`LangFormat::new`] checked.
pub(crate) fn datetime(
    pattern: &str,
    (year, month, day): (i32, u8, u8),
    (hour, minute, second): (u8, u8, u8),
    ordinal: u16,
) -> String {
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let part = match chars.next() {
            Some('d') => format!("{day:02}"),
            Some('m') => format!("{month:02}"),
            Some('Y') => format!("{year:04}"),
            Some('y') => format!("{:02}", year.rem_euclid(100)),
            Some('H') => format!("{hour:02}"),
            Some('M') => format!("{minute:02}"),
            Some('S') => format!("{second:02}"),
            Some('I') => format!("{:02}", (hour + 11) % 12 + 1),
            Some('p') => String::from(if hour < 12 { "AM" } else { "PM" }),
            Some('j') => format!("{ordinal:03}"),
            Some(other) => String::from(other),
            None => String::from('%'),
        };
        out.push_str(&part);
    }
    out
}
