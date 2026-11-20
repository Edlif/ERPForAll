// SPDX-License-Identifier: MIT OR Apache-2.0
//! User-content diagnostics retained by evaluation, independently of renderer diagnostics.
use serde::Serialize;
/// A declared degradation of user HTML, not a developer-template validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub enum ReportWarningKind {
    HtmlElementUnwrapped { element: String },
    HtmlAttributeDropped { element: String, attribute: String },
    HtmlStyleDropped { property: String, value: String },
    HtmlClassIgnored { class: String },
    ExternalImageNotFetched { url_host: String },
    ImageDegraded { reason: String },
    LinkDropped { scheme: String },
    HtmlFlattened { depth: usize },
    HtmlParseFailed,
}
/// Original field identity, or an explicitly selected HTML expression without a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct HtmlSource {
    pub(crate) record: Option<(String, i64, String, String)>,
}
impl HtmlSource {
    pub(crate) fn expression() -> Self {
        Self { record: None }
    }
    /// Original model, record ID, display name and field; absent for expression content.
    pub fn record(&self) -> Option<(&str, i64, &str, &str)> {
        self.record
            .as_ref()
            .map(|(m, id, n, f)| (m.as_str(), *id, n.as_str(), f.as_str()))
    }
    pub(crate) fn bytes(&self) -> usize {
        self.record
            .as_ref()
            .map_or(0, |(m, _, n, f)| m.len() + n.len() + f.len())
    }
}
/// Location in the template and parsed HTML arena. The node index is structural, not a guessed source offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ReportWarning {
    pub(crate) kind: ReportWarningKind,
    pub(crate) source: HtmlSource,
    pub(crate) template: String,
    pub(crate) line: u32,
    pub(crate) node: usize,
}
impl ReportWarning {
    /// The specific degradation and its affected token.
    pub fn kind(&self) -> &ReportWarningKind {
        &self.kind
    }
    /// Field provenance retained by the issuing record store.
    pub fn source(&self) -> &HtmlSource {
        &self.source
    }
    /// Template key where this value was printed.
    pub fn template(&self) -> &str {
        &self.template
    }
    /// Template line, not an inferred line in the HTML field.
    pub fn line(&self) -> u32 {
        self.line
    }
    /// Structural parser arena index within the HTML value.
    pub fn node(&self) -> usize {
        self.node
    }
    pub(crate) fn bytes(&self) -> usize {
        use ReportWarningKind::*;
        let payload = match &self.kind {
            HtmlElementUnwrapped { element } => element.len(),
            HtmlAttributeDropped { element, attribute } => element.len() + attribute.len(),
            HtmlStyleDropped { property, value } => property.len() + value.len(),
            HtmlClassIgnored { class } => class.len(),
            ExternalImageNotFetched { url_host } => url_host.len(),
            ImageDegraded { reason } => reason.len(),
            LinkDropped { scheme } => scheme.len(),
            HtmlFlattened { .. } | HtmlParseFailed => 0,
        };
        std::mem::size_of::<Self>() + payload + self.source.bytes() + self.template.len()
    }
}
impl std::fmt::Display for ReportWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some((model, id, display, field)) = self.source.record() {
            write!(f, "{model}({id}, {display}).{field}: ")?;
        }
        write!(
            f,
            "{}:{} HTML node {}: {:?}",
            self.template, self.line, self.node, self.kind
        )
    }
}
