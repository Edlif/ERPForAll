// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Typst adapter. Only shipped renderer source and hash-admitted fonts are executable resources.
//! In-process compilation cannot be interrupted and the page limit is checked after layout: a served
//! node builds this engine only inside a render worker process (`erp report-worker`, deadline and
//! memory cap of `erp-report-worker`), and record text must not reach it any other way.
mod encode;
mod fonts;
mod picture;
mod world;
use erp_report::{
    DocTree, LayoutEngine, LayoutFailure, LimitKind, Limits, PdfStandard, RenderIdentity,
    RenderResult, ReportError,
};
use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{Mutex, Once, PoisonError},
};
use typst::{
    foundations::{Datetime, Smart},
    layout::{Frame, FrameItem},
    syntax::Source,
};
use typst_layout::{Page, PagedDocument};
use typst_pdf::{PdfOptions, PdfStandards, Timestamp};
use world::ReportWorld;

pub use fonts::OptionalFonts;
/// Renderer identity reported with every result: the fixed renderer source version and the pinned engine.
pub const RENDERER: &str = "erp-print-v1+typst-0.15.1";
/// Evictions performed in this process; observed by tests only.
#[cfg(test)]
static EVICTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static EVICTION: Mutex<EvictionState> = Mutex::new(EvictionState {
    completed: 0,
    interval: usize::MAX,
});
struct EvictionState {
    completed: usize,
    interval: usize,
}
impl Default for EvictionState {
    fn default() -> Self {
        Self {
            completed: 0,
            interval: usize::MAX,
        }
    }
}
impl EvictionState {
    fn complete(&mut self, interval: usize, failed: bool) -> Option<usize> {
        self.interval = self.interval.min(interval);
        self.completed += 1;
        if failed || self.completed >= self.interval {
            self.completed = 0;
            Some(if failed { 0 } else { 2 })
        } else {
            None
        }
    }
}
fn maintain(interval: usize, failed: bool) {
    // The state is two counters and valid after any interrupted update: a poisoned lock is taken over.
    let age = EVICTION
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .complete(interval, failed);
    if let Some(age) = age {
        comemo::evict(age);
        #[cfg(test)]
        EVICTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
thread_local! {static REPORT_THREAD:Cell<bool>=const {Cell::new(false)};}
static PANIC_HOOK: Once = Once::new();
/// Source locations (`file:line:column`) of the panics raised on layout threads and not yet reported:
/// the hook adds one, [`contain`] takes one for the error it builds. A location is a line of renderer
/// source, never document text. The list is shared by the engines of the process (a panic can be
/// raised on another pool thread than the one that catches it), so two renders that panic at the same
/// moment may get each other's location.
static PANIC_SITES: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Locations kept when nothing takes them; a build invariant, the oldest goes first.
const PANIC_SITES_KEPT: usize = 16;
fn install_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !REPORT_THREAD.get() {
                previous(info);
                return;
            }
            // The payload can quote document text and is dropped; the location cannot.
            if let Some(at) = info.location() {
                // A list of strings is whole after any interrupted update: a poisoned lock is taken over.
                let mut sites = PANIC_SITES.lock().unwrap_or_else(PoisonError::into_inner);
                if sites.len() >= PANIC_SITES_KEPT {
                    sites.remove(0);
                }
                sites.push(format!("{}:{}:{}", at.file(), at.line(), at.column()));
            }
        }));
    });
}
/// Stack of a layout thread. A build invariant that goes with the depth ceiling of 32, not a setting:
/// the nested fixtures of every container kind lay out on these threads in the tests.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;
/// Native engine instance. Font bytes and compiled renderer source are reused between documents.
///
/// The first engine of a process installs a panic hook that keeps the panic text of its own layout
/// threads off stderr, keeps the panic's source location for the `reason` field of the error the
/// render returns, and passes every other panic to the hook that was installed before it. Install
/// the host's hook before the first engine and do not replace the hook afterwards: a later
/// `std::panic::set_hook` that does not chain would print renderer panic text again.
#[non_exhaustive]
pub struct TypstEngine {
    fonts: fonts::FontStore,
    source: Source,
    limits: Limits,
    pool: rayon::ThreadPool,
    /// Makes the next compile panic; set by tests only.
    #[cfg(test)]
    panic_next: std::sync::atomic::AtomicBool,
}
impl TypstEngine {
    pub fn new(font_directory: impl AsRef<Path>, limits: Limits) -> Result<Self, ReportError> {
        install_hook();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(limits.get(LimitKind::WorkerThreads))
            .stack_size(WORKER_STACK_BYTES)
            .start_handler(|_| REPORT_THREAD.set(true))
            .build()
            .map_err(|_| ReportError::Engine {
                reason: "cannot create the bounded report thread pool",
            })?;
        Ok(Self {
            pool,
            fonts: fonts::FontStore::new(font_directory.as_ref(), limits)?,
            source: ReportWorld::source(include_str!("erp-print-v1.typ"))?,
            limits,
            #[cfg(test)]
            panic_next: std::sync::atomic::AtomicBool::new(false),
        })
    }
    /// Gives an optional font package its directory on this node, so that documents can name its
    /// families. The package's manifest is compiled in; a file of the directory is parsed only when
    /// its bytes have the compiled SHA-256. Without this call a document that names one of the
    /// package's families fails with a `Font` error that names the package.
    ///
    /// # Errors
    /// `Font` when the directory cannot be opened, is not a directory, or the package already has one.
    pub fn with_optional_fonts(
        mut self,
        package: OptionalFonts,
        directory: impl AsRef<Path>,
    ) -> Result<Self, ReportError> {
        self.fonts.add_optional(package, directory.as_ref())?;
        Ok(self)
    }
    /// Release memoized document state at a batch boundary, also called after errors.
    pub fn finish_batch(&self) {
        maintain(self.limits.get(LimitKind::EvictionInterval), true);
    }
    fn compile(&self, doc: &DocTree) -> Result<RenderResult, ReportError> {
        #[cfg(test)]
        tests::injected_panic(&self.panic_next);
        doc.check_limits(self.limits)?;
        let (year, month, day) = doc.options().date().parts();
        let date = Datetime::from_ymd_hms(year, month, day, 0, 0, 0).ok_or(
            ReportError::InvalidDocument {
                reason: "invalid render date",
            },
        )?;
        let (inputs, mut warnings) = encode::encode(doc, &self.limits)?;
        let world = ReportWorld::new(
            self.source.clone(),
            inputs,
            self.fonts.select(doc.options().fonts())?,
            date,
        );
        let result = typst::compile::<PagedDocument>(&world);
        let output = result.output.map_err(|errors| ReportError::Layout {
            document: doc.options().id().into(),
            kind: LayoutFailure::Diagnostic,
            reason: errors
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        })?;
        self.limits.check(LimitKind::Pages, output.pages().len())?;
        let standard = match doc.options().standard() {
            PdfStandard::Pdf17 => typst_pdf::PdfStandard::V_1_7,
            PdfStandard::PdfA3b => typst_pdf::PdfStandard::A_3b,
            _ => {
                return Err(ReportError::Unsupported {
                    feature: "PDF standard",
                    task: "a later release",
                });
            }
        };
        let standards = PdfStandards::new(&[standard]).map_err(|e| ReportError::Pdf {
            document: doc.options().id().into(),
            reason: format!("{e:?}"),
        })?;
        let options = PdfOptions {
            ident: Smart::Custom(doc.options().id().into()),
            timestamp: Some(Timestamp::new_utc(date)),
            standards,
            ..PdfOptions::default()
        };
        let bytes = typst_pdf::pdf(&output, &options).map_err(|errors| ReportError::Pdf {
            document: doc.options().id().into(),
            reason: errors
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        })?;
        self.limits.check(LimitKind::PdfBytes, bytes.len())?;
        warnings.extend(result.warnings.iter().map(|d| d.message.to_string()));
        let missing = missing_glyphs(output.pages());
        if missing > 0 {
            warnings.push(format!(
                "{missing} glyphs are in none of the selected font families and print as empty boxes"
            ));
        }
        Ok(RenderResult::new(
            RenderIdentity::new(doc.options().id(), RENDERER),
            bytes,
            output.pages().len(),
            warnings,
        ))
    }
}
/// Glyph 0 is a font's missing-glyph box: the renderer draws it without a diagnostic of its own.
fn missing_glyphs(pages: &[Page]) -> usize {
    let mut frames: Vec<&Frame> = pages.iter().map(|page| &page.frame).collect();
    let mut missing = 0;
    while let Some(frame) = frames.pop() {
        for (_, item) in frame.items() {
            match item {
                FrameItem::Group(group) => frames.push(&group.frame),
                FrameItem::Text(text) => {
                    missing += text.glyphs.iter().filter(|glyph| glyph.id == 0).count();
                }
                FrameItem::Shape(..)
                | FrameItem::Image(..)
                | FrameItem::Link(..)
                | FrameItem::Tag(..) => {}
            }
        }
    }
    missing
}
fn contain<T>(id: &str, render: impl FnOnce() -> Result<T, ReportError>) -> Result<T, ReportError> {
    catch_unwind(AssertUnwindSafe(render)).map_err(|_| {
        let site = PANIC_SITES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        ReportError::Layout {
            document: id.into(),
            kind: LayoutFailure::Panic,
            // For the operator: where the renderer panicked. `Display` never prints the reason.
            reason: match site {
                Some(site) => format!("renderer panicked at {site}"),
                None => "renderer panicked".into(),
            },
        }
    })?
}
impl LayoutEngine for TypstEngine {
    fn render(&self, doc: &DocTree) -> Result<RenderResult, ReportError> {
        self.pool.install(|| {
            let result = contain(doc.options().id(), || self.compile(doc));
            // Only a failure inside the renderer can leave memoized state worth dropping at once; a
            // document refused before layout counts as an ordinary completion.
            let failed = matches!(
                result,
                Err(ReportError::Layout { .. } | ReportError::Pdf { .. })
            );
            maintain(self.limits.get(LimitKind::EvictionInterval), failed);
            result
        })
    }
}
#[cfg(test)]
mod tests;
