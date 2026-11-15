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
/// Output warnings travel with bytes instead of being lost in logs.
#[derive(Debug)]
#[non_exhaustive]
pub struct RenderResult {
    identity: RenderIdentity,
    bytes: Vec<u8>,
    pages: usize,
    warnings: Vec<String>,
    retried: u8,
}
impl RenderResult {
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
            warnings,
            retried: 0,
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
