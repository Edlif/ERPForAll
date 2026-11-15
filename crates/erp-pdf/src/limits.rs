// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::error::{Result, invalid};
use crate::{PdfError, PdfErrorKind};
/// Caller-configurable admission dimensions, all with floor one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(usize)]
pub enum Limit {
    /// Encoded bytes of one input.
    InputBytes,
    /// Emitted bytes and cumulative generated/copied data (separate counters).
    OutputBytes,
    /// Input documents per merge.
    Documents,
    /// Pages per operation.
    Pages,
    /// Objects per operation.
    Objects,
    /// Nesting and reference-chain depth.
    Depth,
    /// Traversal work units.
    WorkUnits,
    /// Decoded bytes per content/attachment operation.
    DecodedBytes,
    /// Metadata/name bytes.
    MetadataBytes,
}
impl Limit {
    /// Stable option name.
    pub fn name(self) -> &'static str {
        [
            "input_bytes",
            "output_bytes",
            "documents",
            "pages",
            "objects",
            "depth",
            "work_units",
            "decoded_bytes",
            "metadata_bytes",
        ][self as usize]
    }
    /// Hard admission ceiling, not a measured capacity guarantee.
    pub fn ceiling(self) -> usize {
        [
            268435456, 1073741824, 10000, 100000, 4000000, 128, 16000000, 67108864, 4194304,
        ][self as usize]
    }
}
/// Validated limits supplied by the caller; the library reads no settings.
/// Worker node configuration owns capacity before any database is available.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PdfLimits {
    values: [usize; 9],
}
impl Default for PdfLimits {
    fn default() -> Self {
        Self {
            values: [
                67108864, 268435456, 1024, 10000, 1000000, 64, 4000000, 16777216, 1048576,
            ],
        }
    }
}
impl PdfLimits {
    /// Change one bound. Zero and values above the hard ceiling are errors.
    pub fn with(mut self, limit: Limit, value: usize) -> Result<Self> {
        if value == 0 || value > limit.ceiling() {
            return Err(invalid(format!(
                "PDF limit {} accepts 1 to {}, not {value}",
                limit.name(),
                limit.ceiling()
            )));
        }
        self.values[limit as usize] = value;
        Ok(self)
    }
    /// Configured bound.
    pub fn get(&self, limit: Limit) -> usize {
        self.values[limit as usize]
    }
    pub(crate) fn check(&self, limit: Limit, value: usize) -> Result<()> {
        if value > self.get(limit) {
            Err(PdfError::new(
                PdfErrorKind::Limit,
                format!(
                    "PDF limit {} exceeded: {value} > {}",
                    limit.name(),
                    self.get(limit)
                ),
            ))
        } else {
            Ok(())
        }
    }
}
/// Private counters are created once per public call, never retained in caller options.
pub(crate) struct Context<'a> {
    limits: &'a PdfLimits,
    work: std::cell::Cell<usize>,
    objects: std::cell::Cell<usize>,
    copied: std::cell::Cell<usize>,
}
impl<'a> Context<'a> {
    pub fn new(limits: &'a PdfLimits) -> Self {
        Self {
            limits,
            work: std::cell::Cell::new(0),
            objects: std::cell::Cell::new(0),
            copied: std::cell::Cell::new(0),
        }
    }
    fn charge(&self, counter: &std::cell::Cell<usize>, limit: Limit, n: usize) -> Result<()> {
        let next = counter
            .get()
            .checked_add(n)
            .ok_or_else(|| invalid("PDF admission counter overflow"))?;
        self.check(limit, next)?;
        counter.set(next);
        Ok(())
    }
    pub fn step(&self) -> Result<()> {
        self.charge(&self.work, Limit::WorkUnits, 1)
    }
    pub fn copied(&self, n: usize) -> Result<()> {
        let size = self
            .copied
            .get()
            .checked_add(n)
            .ok_or_else(|| invalid("PDF copied byte counter overflow"))?;
        if size > self.get(Limit::OutputBytes) {
            return Err(PdfError::new(
                PdfErrorKind::Limit,
                "PDF copied/generated data exceeds output_bytes limit",
            ));
        }
        self.copied.set(size);
        Ok(())
    }
    pub fn objects(&self, n: usize) -> Result<()> {
        self.charge(&self.objects, Limit::Objects, n)
    }
}
impl std::ops::Deref for Context<'_> {
    type Target = PdfLimits;
    fn deref(&self) -> &PdfLimits {
        self.limits
    }
}
pub(crate) struct Budget<'a> {
    pub limits: &'a Context<'a>,
}
impl<'a> Budget<'a> {
    pub fn new(limits: &'a Context<'a>) -> Self {
        Self { limits }
    }
    pub fn step(&mut self) -> Result<()> {
        self.limits.step()
    }
}
