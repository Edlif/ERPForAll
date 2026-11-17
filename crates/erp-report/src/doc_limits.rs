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
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
    fn index(self) -> usize {
        self as usize
    }
    /// The hard bound of the kind, which is also its default: a host can only lower it.
    const fn ceiling(self) -> usize {
        match self {
            Self::Nodes => 500_000,
            Self::Bytes => 64 * 1024 * 1024,
            Self::Depth => 32,
            Self::ImageBytes => 10 * 1024 * 1024,
            Self::ImagePixels => 40_000_000,
            Self::Pages => 5_000,
            Self::PdfBytes => 256 * 1024 * 1024,
            Self::EvictionInterval => 64,
            Self::FontBytes => 20 * 1024 * 1024,
            Self::WorkerThreads => 4,
        }
    }
    const fn name(self) -> &'static str {
        match self {
            Self::Nodes => "nodes",
            Self::Bytes => "bytes",
            Self::Depth => "depth",
            Self::ImageBytes => "image_bytes",
            Self::ImagePixels => "image_pixels",
            Self::Pages => "pages",
            Self::PdfBytes => "pdf_bytes",
            Self::EvictionInterval => "eviction_interval",
            Self::FontBytes => "font_bytes",
            Self::WorkerThreads => "worker_threads",
        }
    }
}
// `ALL` is in discriminant order, so `values[kind as usize]` is the value of `kind`.
const _: () = {
    let mut i = 0;
    while i < LimitKind::ALL.len() {
        assert!(LimitKind::ALL[i] as usize == i);
        i += 1;
    }
};
impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
/// Positive admission limits, never a switch for unlimited rendering.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Limits {
    /// One value per kind, in the order of [`LimitKind::ALL`].
    values: [usize; LimitKind::ALL.len()],
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            values: LimitKind::ALL.map(LimitKind::ceiling),
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
