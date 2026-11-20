// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{DocTree, ReportError};
use std::borrow::Cow;
/// A synchronous engine adapter. Call outside the async executor.
pub trait LayoutEngine {
    fn render(&self, document: &DocTree) -> Result<RenderResult, ReportError>;
}
/// What produced an output: the caller's safe document id and the versioned renderer. Never document content.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderIdentity {
    document: String,
    renderer: Cow<'static, str>,
}
impl RenderIdentity {
    /// `renderer` is a constant of the engine, or the name a render worker reported for itself.
    pub fn new(document: &str, renderer: impl Into<Cow<'static, str>>) -> Self {
        Self {
            document: document.into(),
            renderer: renderer.into(),
        }
    }
    pub fn document(&self) -> &str {
        &self.document
    }
    pub fn renderer(&self) -> &str {
        &self.renderer
    }
}
/// A renderer failure tied to an arena image node, independently of human-readable messages.
/// Construction admits the index and reason under the host's document limits; mapping also
/// verifies the index against the actual document. This type deliberately has no deserializer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageFailure {
    node: usize,
    reason: String,
}
impl ImageFailure {
    /// Admit a nonempty reason and a node index under the document's node and byte bounds.
    pub fn new(node: usize, reason: String, limits: crate::Limits) -> Result<Self, ReportError> {
        limits.add(crate::LimitKind::Nodes, node, 1)?;
        limits.check(crate::LimitKind::Bytes, reason.len())?;
        if reason.is_empty() {
            return Err(crate::error::invalid(
                "image failure reason must not be empty",
            ));
        }
        Ok(Self { node, reason })
    }
    /// Arena index of the image which degraded.
    pub fn node(&self) -> usize {
        self.node
    }
    /// Decoder reason, with no field provenance encoded inside it.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}
/// Output warnings travel with bytes instead of being lost in logs.
#[derive(Debug)]
#[non_exhaustive]
pub struct RenderResult {
    identity: RenderIdentity,
    bytes: Vec<u8>,
    pages: usize,
    warnings: Vec<String>,
    retried: u8,
    content_warnings: Vec<crate::ReportWarning>,
    image_failures: Vec<ImageFailure>,
    engine_warning_count: usize,
    document_warning_count: usize,
}
impl RenderResult {
    /// Structured user-content diagnostics retained alongside engine messages.
    pub fn content_warnings(&self) -> &[crate::ReportWarning] {
        &self.content_warnings
    }
    /// Replace evaluation warnings while preserving any mapped decoder warnings.
    pub fn with_content_warnings(mut self, warnings: &[crate::ReportWarning]) -> Self {
        self.warnings
            .truncate(self.warnings.len() - self.content_warnings.len());
        self.content_warnings
            .splice(..self.document_warning_count, warnings.iter().cloned());
        self.document_warning_count = warnings.len();
        self.warnings
            .extend(self.content_warnings.iter().map(ToString::to_string));
        self
    }
    /// Engine messages before document and image diagnostics are formatted. Worker transport uses
    /// this channel so the parent can map image diagnostics against its own source-bearing tree.
    pub fn engine_warnings(&self) -> &[String] {
        &self.warnings[..self.engine_warning_count]
    }
    /// Structured decoder failures retained for parent-side provenance mapping.
    pub fn image_failures(&self) -> &[ImageFailure] {
        &self.image_failures
    }
    /// Attach unique, bounded renderer diagnostics. Actual image membership is checked when mapping.
    pub fn with_image_failures(
        mut self,
        failures: Vec<ImageFailure>,
        limits: crate::Limits,
    ) -> Result<Self, ReportError> {
        limits.check(crate::LimitKind::Images, failures.len())?;
        let mut bytes = 0;
        let mut seen = std::collections::BTreeSet::new();
        for failure in &failures {
            limits.add(crate::LimitKind::Nodes, failure.node, 1)?;
            bytes = limits.add(crate::LimitKind::Bytes, bytes, failure.reason.len())?;
            bytes = limits.add(
                crate::LimitKind::Bytes,
                bytes,
                std::mem::size_of::<ImageFailure>(),
            )?;
            if !seen.insert(failure.node) {
                return Err(crate::error::invalid(
                    "image failures must identify unique image nodes",
                ));
            }
        }
        self.warnings.truncate(self.engine_warning_count);
        self.content_warnings.truncate(self.document_warning_count);
        self.warnings
            .extend(self.content_warnings.iter().map(ToString::to_string));
        self.image_failures = failures;
        Ok(self)
    }
    /// Validate renderer targets and combine evaluation and late image warnings exactly once.
    /// HTML images retain their original field/template location; template images keep an engine
    /// message. Reapplying this method replaces the mapped diagnostics rather than duplicating them.
    pub fn with_document_warnings(mut self, document: &DocTree) -> Result<Self, ReportError> {
        let limits = document.limits;
        limits.check(crate::LimitKind::Images, self.image_failures.len())?;
        self.warnings.truncate(self.engine_warning_count);
        self.content_warnings = document.content_warnings().to_vec();
        self.document_warning_count = self.content_warnings.len();
        let mut bytes = 0;
        for warning in &self.content_warnings {
            bytes = limits.add(crate::LimitKind::Bytes, bytes, warning.bytes())?;
        }
        for failure in &self.image_failures {
            if !matches!(
                document.nodes().get(failure.node),
                Some(crate::Node::Image { .. })
            ) {
                return Err(crate::error::invalid(
                    "image failure must identify an image node",
                ));
            }
            bytes = limits.add(crate::LimitKind::Bytes, bytes, failure.reason.len())?;
            if let Some(origin) = document.image_origin(failure.node) {
                let mut warning = origin.clone();
                warning.kind = crate::ReportWarningKind::ImageDegraded {
                    reason: failure.reason.clone(),
                };
                bytes = limits.add(crate::LimitKind::Bytes, bytes, warning.bytes())?;
                self.content_warnings.push(warning);
            } else {
                let message = format!(
                    "image node {}: {}; an empty box is printed",
                    failure.node, failure.reason
                );
                bytes = limits.add(crate::LimitKind::Bytes, bytes, message.len())?;
                self.warnings.push(message);
            }
        }
        for warning in &self.content_warnings {
            let message = warning.to_string();
            bytes = limits.add(crate::LimitKind::Bytes, bytes, message.len())?;
            self.warnings.push(message);
        }
        Ok(self)
    }
    pub fn new(
        identity: RenderIdentity,
        bytes: Vec<u8>,
        pages: usize,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            identity,
            bytes,
            pages,
            engine_warning_count: warnings.len(),
            document_warning_count: 0,
            image_failures: Vec::new(),
            warnings,
            retried: 0,
            content_warnings: Vec::new(),
        }
    }
    /// Record how many times the document was sent again after a render worker stopped under it.
    pub fn with_retried(mut self, retried: u8) -> Self {
        self.retried = retried;
        self
    }
    /// Attempts beyond the first that it took to render this document; 0 for an in-process engine.
    pub fn retried(&self) -> u8 {
        self.retried
    }
    /// The PDF bytes, without a copy.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
    pub fn identity(&self) -> &RenderIdentity {
        &self.identity
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn page_count(&self) -> usize {
        self.pages
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod image_diagnostic_tests {
    use super::*;
    use crate::{DocumentBuilder, DocumentOptions, Limits, Node, PageSetup};

    #[test]
    fn image_diagnostic_rejects_non_image_target() {
        let limits = Limits::default();
        let mut builder = DocumentBuilder::new(limits);
        let text = builder.add(Node::Text("body".into())).unwrap();
        let doc = builder
            .finish(
                text,
                DocumentOptions::new("test", "Test", PageSetup::new(400.0, 500.0, 20.0).unwrap()),
            )
            .unwrap();
        let result = RenderResult::new(RenderIdentity::new("test", "test"), vec![], 1, vec![])
            .with_image_failures(
                vec![ImageFailure::new(0, "bad raster".into(), limits).unwrap()],
                limits,
            )
            .unwrap()
            .with_document_warnings(&doc);
        assert!(matches!(
            result,
            Err(ReportError::InvalidDocument {
                reason: "image failure must identify an image node"
            })
        ));
    }

    #[test]
    fn image_origin_becomes_typed_warning_exactly_once() {
        let limits = Limits::default();
        let mut builder = DocumentBuilder::new(limits);
        let image = builder
            .add(Node::Image {
                data: std::sync::Arc::from(&b"<svg/>"[..]),
                width: Some(20.0),
                height: Some(20.0),
                fit: crate::ImageFit::Contain,
                alt: "logo".into(),
            })
            .unwrap();
        let origin = crate::ReportWarning {
            kind: crate::ReportWarningKind::ImageDegraded {
                reason: String::new(),
            },
            source: crate::HtmlSource {
                record: Some(("note".into(), 7, "Invoice".into(), "body".into())),
            },
            template: "invoice".into(),
            line: 3,
            node: 5,
        };
        let doc = builder
            .finish(
                image,
                DocumentOptions::new("test", "Test", PageSetup::new(400.0, 500.0, 20.0).unwrap()),
            )
            .unwrap()
            .with_image_origins(vec![(0, origin.clone())], limits)
            .unwrap();
        let result = RenderResult::new(RenderIdentity::new("test", "test"), vec![], 1, vec![])
            .with_image_failures(
                vec![ImageFailure::new(0, "broken raster".into(), limits).unwrap()],
                limits,
            )
            .unwrap()
            .with_document_warnings(&doc)
            .unwrap()
            .with_document_warnings(&doc)
            .unwrap();
        let mut expected = origin;
        expected.kind = crate::ReportWarningKind::ImageDegraded {
            reason: "broken raster".into(),
        };
        assert_eq!(result.content_warnings(), std::slice::from_ref(&expected));
        assert_eq!(result.warnings(), &[expected.to_string()]);
        let result = result.with_content_warnings(&[]);
        assert_eq!(result.content_warnings(), std::slice::from_ref(&expected));
        assert_eq!(result.warnings(), &[expected.to_string()]);
    }

    #[test]
    fn image_diagnostic_obeys_reason_and_count_budgets() {
        let limits = Limits::default()
            .lowered(crate::LimitKind::Bytes, 4)
            .unwrap();
        assert!(matches!(
            ImageFailure::new(0, "large".into(), limits),
            Err(ReportError::DocumentLimit {
                kind: crate::LimitKind::Bytes,
                actual: 5,
                max: 4
            })
        ));
        let limits = Limits::default()
            .lowered(crate::LimitKind::Images, 1)
            .unwrap();
        let failures = vec![
            ImageFailure::new(0, "a".into(), limits).unwrap(),
            ImageFailure::new(1, "b".into(), limits).unwrap(),
        ];
        assert!(matches!(
            RenderResult::new(RenderIdentity::new("test", "test"), vec![], 1, vec![])
                .with_image_failures(failures, limits),
            Err(ReportError::DocumentLimit {
                kind: crate::LimitKind::Images,
                actual: 2,
                max: 1
            })
        ));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod image_failure_boundaries {
    use super::*;
    use crate::{LimitKind, Limits};

    #[test]
    fn duplicate_failure_targets_are_refused() {
        let limits = Limits::default();
        let failure = ImageFailure::new(0, "bad".into(), limits).unwrap();
        let result = RenderResult::new(RenderIdentity::new("test", "test"), vec![], 1, vec![])
            .with_image_failures(vec![failure.clone(), failure], limits);
        assert!(matches!(
            result,
            Err(ReportError::InvalidDocument {
                reason: "image failures must identify unique image nodes"
            })
        ));
    }

    #[test]
    fn exact_reason_bound_is_accepted_and_index_overflow_is_refused() {
        let limits = Limits::default().lowered(LimitKind::Bytes, 4).unwrap();
        assert_eq!(
            ImageFailure::new(0, "four".into(), limits)
                .unwrap()
                .reason(),
            "four"
        );
        assert!(matches!(
            ImageFailure::new(usize::MAX, "x".into(), limits),
            Err(ReportError::DocumentLimit {
                kind: LimitKind::Nodes,
                actual: usize::MAX,
                ..
            })
        ));
        assert!(matches!(
            ImageFailure::new(0, String::new(), limits),
            Err(ReportError::InvalidDocument {
                reason: "image failure reason must not be empty"
            })
        ));
    }
}
