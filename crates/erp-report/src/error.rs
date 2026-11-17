// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::LimitKind;
use serde::Serialize;
use std::fmt;

/// Vocabulary category rejected by print profile 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub enum ProfileKind {
    Element,
    Directive,
    Attribute,
    AttributeValue,
    Class,
    Style,
    StyleValue,
    Icon,
}
impl fmt::Display for ProfileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Element => "element",
            Self::Directive => "directive",
            Self::Attribute => "attribute",
            Self::AttributeValue => "attribute value",
            Self::Class => "class",
            Self::Style => "style",
            Self::StyleValue => "style value",
            Self::Icon => "icon",
        })
    }
}
/// One unsupported token. Locations refer to the supplied arch, not a source map guessed from XML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Diagnostic {
    pub(crate) template: String,
    pub(crate) line: u32,
    pub(crate) token: String,
    pub(crate) kind: ProfileKind,
}
impl Diagnostic {
    /// Template identity supplied by the caller.
    pub fn template(&self) -> &str {
        &self.template
    }
    /// One-based element location in the supplied arch.
    pub fn line(&self) -> u32 {
        self.line
    }
    /// Exact rejected token.
    pub fn token(&self) -> &str {
        &self.token
    }
    /// Typed vocabulary category.
    pub fn kind(&self) -> ProfileKind {
        self.kind
    }
}
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {} `{}` is not in print profile 1; see the addon guide page on reports",
            self.template, self.line, self.kind, self.token
        )
    }
}
/// Layout diagnostics and unwind recovery are distinct error kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayoutFailure {
    Diagnostic,
    Panic,
}
impl LayoutFailure {
    /// The fixed tail of the message. The renderer's own text stays in the error's `reason` field.
    fn note(self) -> &'static str {
        match self {
            Self::Diagnostic => "renderer diagnostics are in the error's reason field",
            Self::Panic => "renderer panicked",
        }
    }
}
/// Why a render worker process stopped under a document (design `reports-pdf.md` §6.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkerCrash {
    /// Resident memory or address space went over the worker's cap. The document is not retried.
    MemoryCap,
    /// A signal other than the pool's own deadline or memory stop ended the process.
    Signal,
    /// The process exited by itself.
    Exit,
    /// The worker or the pool refused a frame: unknown kind, over the size cap, or malformed.
    Protocol,
}
impl fmt::Display for WorkerCrash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MemoryCap => "memory cap exceeded",
            Self::Signal => "stopped by a signal",
            Self::Exit => "process exited",
            Self::Protocol => "frame protocol violation",
        })
    }
}
/// The one error type of the report library: template admission (profile, attachment fields) and the
/// native document boundary (document, fonts, images, layout, export). Failures are returned, never
/// downgraded to successful validation or to an empty document.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReportError {
    /// Closed-vocabulary violation.
    #[error("{0}")]
    Profile(Diagnostic),
    /// XML or inheritance failure retaining the view engine's error kind.
    #[error("{template}: {source}")]
    View {
        template: String,
        #[source]
        source: Box<erp_views::ViewError>,
    },
    /// Bounded expression parse failure.
    #[error("{template}:{line}: expression `{expression}`: {source}")]
    Expression {
        template: String,
        line: u32,
        expression: String,
        #[source]
        source: erp_expr::ExprError,
    },
    /// A configurable, bounded library admission limit.
    #[error("report limit {name} exceeded or invalid (maximum {maximum})")]
    Limit { name: &'static str, maximum: usize },
    /// The corpus scan refused its input or one template's analysis.
    #[error("report corpus: {0}")]
    Corpus(#[from] crate::CorpusError),
    /// Unsupported profile version.
    #[error("print profile {0} is unsupported; supported version is 1")]
    Version(u32),
    /// A template prints a group-restricted field into an archived report.
    #[error(
        "report {report}: {template}:{line} prints restricted field {field}; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
    )]
    Restricted {
        report: String,
        template: String,
        line: u32,
        field: String,
    },
    /// Incomplete analysis cannot establish attachment safety.
    #[error(
        "report {report}: {template}:{line} cannot resolve `{expression}` for attachment field validation"
    )]
    Unresolved {
        report: String,
        template: String,
        line: u32,
        expression: String,
    },
    /// A document or rendering limit was lowered to zero or above its current value.
    #[error("report limit {kind} must be in 1..={max}; got {value}")]
    InvalidLimit {
        kind: LimitKind,
        value: usize,
        max: usize,
    },
    /// The document builder refused a structurally invalid document.
    #[error("invalid report document: {reason}")]
    InvalidDocument { reason: &'static str },
    /// A document or rendering admission limit (`Limits`); template admission uses `Limit`.
    #[error("report limit {kind} exceeded: {actual} > {max}")]
    DocumentLimit {
        kind: LimitKind,
        actual: usize,
        max: usize,
    },
    #[error("report font: {reason}")]
    Font {
        reason: String,
        source: Option<std::io::Error>,
    },
    #[error("report image: {reason}")]
    Image { reason: String },
    /// `document` is the caller's safe document id. `reason` holds the renderer's diagnostics for the
    /// operator and may quote layout input; `Display` never prints it.
    #[error("report layout {kind:?} for {document}: {}", .kind.note())]
    Layout {
        document: String,
        kind: LayoutFailure,
        reason: String,
    },
    /// Export refused the laid-out document. As for `Layout`, `reason` is the exporter's own text for
    /// the operator (it can quote document text) and `Display` never prints it.
    #[error(
        "report PDF export failed for {document}: exporter diagnostics are in the error's reason field"
    )]
    Pdf { document: String, reason: String },
    #[error("report {feature} requires {task}")]
    Unsupported {
        feature: &'static str,
        task: &'static str,
    },
    /// The profile admits the template, but the compiler cannot give it a meaning (a `t-call` of a
    /// template that was not supplied, a call cycle, a directive on the wrong element).
    #[error("{template}:{line}: {reason}")]
    Compile {
        template: String,
        line: u32,
        reason: String,
    },
    /// Evaluation of a compiled program failed for a reason other than a limit or an expression
    /// error: a value of the wrong shape, a record the data stage did not load.
    #[error("{template}:{line}: {reason}")]
    Evaluate {
        template: String,
        line: u32,
        reason: String,
    },
    /// The data stage could not load what the prefetch plan asks for: a context name without
    /// records, or a read whose answer does not match its request.
    #[error("report data: {reason}")]
    Data { reason: String },
    /// A render worker pool setting outside its floor and bound; zero never means unlimited.
    #[error("report worker setting {name} must be in {min}..={max}; got {value}")]
    InvalidSetting {
        name: &'static str,
        value: u64,
        min: u64,
        max: u64,
    },
    /// The document did not finish in time. Its render worker was stopped; the document is not retried.
    #[error(
        "report {document} did not finish within {deadline_ms} ms; its render worker was stopped and the document is not retried"
    )]
    Deadline { document: String, deadline_ms: u64 },
    /// The render worker process stopped under this document. `retried` counts the further attempts
    /// already made on another worker. `log` is the worker's captured stderr for the operator;
    /// `Display` never prints it.
    #[error("report render worker stopped under {document}: {reason} (retried {retried})")]
    WorkerCrashed {
        document: String,
        reason: WorkerCrash,
        retried: u8,
        log: String,
    },
    /// No render worker can take the document: workers do not start, or all stayed busy.
    #[error("report render workers are unavailable: {cause}")]
    WorkerUnavailable { cause: String },
    /// A render worker refused the document with an error whose kind has no owned form on this side
    /// of the pipe; `message` is the worker's own error text.
    #[error("report render worker refused {document}: {message}")]
    WorkerRefused { document: String, message: String },
    /// The engine could not be set up on this node; no document is involved.
    #[error("report engine setup: {reason}")]
    Engine { reason: &'static str },
}
pub(crate) fn invalid(reason: &'static str) -> ReportError {
    ReportError::InvalidDocument { reason }
}
