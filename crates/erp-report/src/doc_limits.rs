// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::ReportError;
use std::fmt;

/// Host-lowerable safety bounds of a document and its rendering. These are node configuration: a
/// renderer has no database. (Template admission has its own type, `ReportLimits`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitKind {
    Nodes,
    Bytes,
    Depth,
    ImageBytes,
    ImagePixels,
    Pages,
    PdfBytes,
    EvictionInterval,
    FontBytes,
    WorkerThreads,
}
impl LimitKind {
    /// Every kind, in declaration order: what a render worker receives with its start message.
    pub const ALL: [Self; 10] = [
        Self::Nodes,
        Self::Bytes,
        Self::Depth,
        Self::ImageBytes,
        Self::ImagePixels,
        Self::Pages,
        Self::PdfBytes,
        Self::EvictionInterval,
        Self::FontBytes,
        Self::WorkerThreads,
    ];
    /// The kind whose `Display` text is `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.to_string() == name)
    }
    fn index(self) -> usize {
        self as usize
    }
    fn ceiling(self) -> usize {
        [
            500_000,
            64 * 1024 * 1024,
            32,
            10 * 1024 * 1024,
            40_000_000,
            5_000,
            256 * 1024 * 1024,
            64,
            20 * 1024 * 1024,
            4,
        ][self.index()]
    }
}
impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            [
                "nodes",
                "bytes",
                "depth",
                "image_bytes",
                "image_pixels",
                "pages",
                "pdf_bytes",
                "eviction_interval",
                "font_bytes",
                "worker_threads",
            ][self.index()],
        )
    }
}
/// Positive admission limits, never a switch for unlimited rendering.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Limits {
    values: [usize; 10],
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            values: [
                500_000,
                64 * 1024 * 1024,
                32,
                10 * 1024 * 1024,
                40_000_000,
                5_000,
                256 * 1024 * 1024,
                64,
                20 * 1024 * 1024,
                4,
            ],
        }
    }
}
impl Limits {
    pub fn lowered(mut self, kind: LimitKind, value: usize) -> Result<Self, ReportError> {
        let max = self.get(kind);
        if value == 0 || value > max {
            return Err(ReportError::InvalidLimit { kind, value, max });
        }
        self.values[kind.index()] = value;
        Ok(self)
    }
    pub fn get(&self, kind: LimitKind) -> usize {
        self.values[kind.index()]
    }
    pub fn check(&self, kind: LimitKind, actual: usize) -> Result<(), ReportError> {
        let max = self.get(kind).min(kind.ceiling());
        if actual > max {
            Err(ReportError::DocumentLimit { kind, actual, max })
        } else {
            Ok(())
        }
    }
    pub(crate) fn add(&self, kind: LimitKind, a: usize, b: usize) -> Result<usize, ReportError> {
        let actual = a.checked_add(b).ok_or(ReportError::DocumentLimit {
            kind,
            actual: usize::MAX,
            max: self.get(kind),
        })?;
        self.check(kind, actual)?;
        Ok(actual)
    }
}
