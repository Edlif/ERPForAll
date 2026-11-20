// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{ReportError, error::invalid};
/// Additional validated inline presentation for user content. No CSS or executable markup.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct RichText {
    pub(crate) strike: bool,
    pub(crate) baseline: i8,
    pub(crate) pre: bool,
    pub(crate) rtl: Option<bool>,
    pub(crate) tracking: f64,
    pub(crate) font: String,
    pub(crate) link: String,
}
impl RichText {
    /// Whether strike-through is enabled.
    pub fn strike(&self) -> bool {
        self.strike
    }
    /// Baseline offset: -1 subscript, 0 normal, 1 superscript.
    pub fn baseline(&self) -> i8 {
        self.baseline
    }
    /// Whether whitespace is preserved.
    pub fn preformatted(&self) -> bool {
        self.pre
    }
    /// Explicit direction, or inherited direction when absent.
    pub fn rtl(&self) -> Option<bool> {
        self.rtl
    }
    /// Letter spacing in points.
    pub fn tracking(&self) -> f64 {
        self.tracking
    }
    /// Validated family name; empty inherits the document font.
    pub fn font(&self) -> &str {
        &self.font
    }
    /// Validated URL; empty is not a link.
    pub fn link(&self) -> &str {
        &self.link
    }
    /// Set strike-through.
    pub fn with_strike(mut self, value: bool) -> Self {
        self.strike = value;
        self
    }
    /// Set subscript (-1), normal (0), or superscript (1); other values fail.
    pub fn with_baseline(mut self, value: i8) -> Result<Self, ReportError> {
        if !(-1..=1).contains(&value) {
            return Err(invalid(
                "unsupported text baseline offset; use subscript (-1), normal (0), or superscript (1)",
            ));
        }
        self.baseline = value;
        Ok(self)
    }
    /// Set whitespace preservation.
    pub fn with_preformatted(mut self, value: bool) -> Self {
        self.pre = value;
        self
    }
    /// Set or inherit text direction.
    pub fn with_rtl(mut self, value: Option<bool>) -> Self {
        self.rtl = value;
        self
    }
    /// Set finite letter spacing in [-100, 100] points.
    pub fn with_tracking(mut self, value: f64) -> Result<Self, ReportError> {
        if !value.is_finite() || !(-100.0..=100.0).contains(&value) {
            return Err(invalid(
                "letter spacing must be finite and in [-100, 100] points",
            ));
        }
        self.tracking = value;
        Ok(self)
    }
    /// Set a family of at most 256 safe identifier bytes, or empty to inherit.
    pub fn with_font(mut self, value: &str) -> Result<Self, ReportError> {
        if value.len() > 256
            || !value
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        {
            return Err(invalid(
                "inline font must be at most 256 bytes of letters, digits, spaces, '-' or '_'",
            ));
        }
        self.font = value.into();
        Ok(self)
    }
    /// Set a bounded HTTP, HTTPS or mailto URL, or empty to remove the link.
    pub fn with_link(mut self, value: &str) -> Result<Self, ReportError> {
        if !value.is_empty()
            && (value.len() > 4096
                || value.chars().any(char::is_control)
                || !url::Url::parse(value)
                    .is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "mailto")))
        {
            return Err(invalid(
                "inline link must be an http, https or mailto URL of at most 4096 bytes without controls",
            ));
        }
        self.link = value.into();
        Ok(self)
    }
    pub(crate) fn bytes(&self) -> usize {
        self.font.capacity() + self.link.capacity()
    }
}
