// SPDX-License-Identifier: MIT OR Apache-2.0
//! Prepared user-content assets; no network, filesystem or ORM capability.
use crate::{
    ImageFormat, Limits, ReportError, ReportLimits, ReportWarningKind, asset::embedded_bytes,
    probe_image,
};
use erp_html::{Fragment, FragmentKind, HtmlSanitizeError};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LocalImage {
    pub model: String,
    pub id: i64,
    pub field: String,
}
/// One auxiliary image answer: its requested record ID and bytes or a content rejection reason.
/// Access and database failures must propagate from the adapter, not become rejection reasons.
/// [`crate::DataStage::supply_image_results`] validates IDs and image admission before retaining it.
pub type HtmlImageResult = (i64, Result<Arc<[u8]>, String>);

/// One exact binary-field projection for a batch of HTML image references.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HtmlImageRequest {
    pub(crate) model: String,
    pub(crate) field: String,
    pub(crate) ids: Vec<i64>,
}
impl HtmlImageRequest {
    /// Exact registry model name of this projection.
    pub fn model(&self) -> &str {
        &self.model
    }
    /// One requested binary field; other fields are never joined into the projection.
    pub fn field(&self) -> &str {
        &self.field
    }
    /// Sorted unique positive persisted IDs.
    pub fn ids(&self) -> &[i64] {
        &self.ids
    }
}
#[derive(Debug, Clone)]
pub(crate) enum ImageOutcome {
    Ready(Arc<[u8]>),
    Warning(ReportWarningKind),
    Local(LocalImage),
}
pub(crate) fn degraded(reason: impl Into<String>) -> ImageOutcome {
    ImageOutcome::Warning(ReportWarningKind::ImageDegraded {
        reason: reason.into(),
    })
}
pub(crate) fn admit(bytes: Arc<[u8]>, limits: &Limits) -> ImageOutcome {
    match probe_image(&bytes, limits) {
        Ok(info) if info.format() == ImageFormat::UntrustedSvg => {
            degraded("user SVG is not supported by the report image renderer")
        }
        Ok(_) => ImageOutcome::Ready(bytes),
        Err(error) => degraded(error.to_string()),
    }
}
fn identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
pub(crate) fn classify(src: &str, limits: &Limits) -> ImageOutcome {
    if let Some(path) = src.strip_prefix("/web/image/") {
        let parts: Vec<_> = path.split('/').collect();
        if let [model, id, field] = parts.as_slice()
            && model.split('.').all(identifier)
            && identifier(field)
            && !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit())
            && let Ok(id) = id.parse::<i64>()
            && id > 0
        {
            return ImageOutcome::Local(LocalImage {
                model: (*model).into(),
                id,
                field: (*field).into(),
            });
        }
        return degraded("invalid local image reference");
    }
    if src.starts_with("data:") {
        return match embedded_bytes(src, limits) {
            Ok(bytes) => admit(bytes.into(), limits),
            Err(error) => degraded(error.to_string()),
        };
    }
    let url = if src.starts_with("//") {
        url::Url::parse(&format!("https:{src}"))
    } else {
        url::Url::parse(src)
    };
    if let Ok(url) = url
        && matches!(url.scheme(), "http" | "https")
    {
        return ImageOutcome::Warning(ReportWarningKind::ExternalImageNotFetched {
            // Host absence is meaningful for malformed HTTP URLs; never retain credentials/path/query.
            url_host: url.host_str().unwrap_or("").into(),
        });
    }
    degraded("image source is not resolved")
}

type Source = (String, i64, String);
#[derive(Debug, Default)]
pub(crate) struct HtmlImages {
    sources: BTreeMap<Source, BTreeMap<usize, ImageOutcome>>,
    targets: BTreeMap<LocalImage, ImageOutcome>,
    pending: VecDeque<HtmlImageRequest>,
    prepared: bool,
    bytes: usize,
    nodes: usize,
}
impl HtmlImages {
    fn charge(&mut self, bytes: usize, limits: ReportLimits) -> Result<(), ReportError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= limits.bytes)
            .ok_or(ReportError::Limit {
                name: "html_image_bytes",
                maximum: limits.bytes,
            })?;
        Ok(())
    }
    pub(crate) fn scan(
        &mut self,
        source: Source,
        input: &str,
        limits: ReportLimits,
        document: Limits,
    ) -> Result<(), ReportError> {
        self.charge(input.len() + source.0.len() + source.2.len(), limits)?;
        let parser = erp_html::Limits::ceiling()
            .with_input_bytes(limits.html_bytes as u64)
            .and_then(|l| l.with_nodes(limits.html_nodes as u64))
            .map_err(|_| crate::error::invalid("invalid HTML image parser configuration"))?;
        let parsed = match Fragment::parse(input, &parser) {
            Ok(parsed) => parsed,
            // Evaluation emits HtmlParseFailed and plain text for these specified non-resource failures.
            Err(HtmlSanitizeError::ParseFailed(_) | HtmlSanitizeError::NulByte { .. }) => {
                return Ok(());
            }
            Err(error) => {
                let (name, maximum) = match error {
                    HtmlSanitizeError::InputTooLarge { limit, .. } => ("html_bytes", limit),
                    HtmlSanitizeError::NodeLimit { limit } => ("html_nodes", limit),
                    HtmlSanitizeError::DepthExceeded { limit } => ("html_parser_depth", limit),
                    HtmlSanitizeError::WorkLimit { limit } => ("html_work", limit),
                    HtmlSanitizeError::MemoryLimit { limit } => ("html_memory", limit),
                    other => {
                        return Err(ReportError::Data {
                            reason: format!("HTML image parser: {other}"),
                        });
                    }
                };
                return Err(ReportError::Limit {
                    name,
                    maximum: usize::try_from(maximum)
                        .map_err(|_| crate::error::invalid("HTML parser limit overflows usize"))?,
                });
            }
        };
        let mut found = BTreeMap::new();
        let mut stack = vec![parsed.root()];
        while let Some(node) = stack.pop() {
            self.nodes = self
                .nodes
                .checked_add(1)
                .filter(|n| *n <= limits.steps)
                .ok_or(ReportError::Limit {
                    name: "html_image_work",
                    maximum: limits.steps,
                })?;
            stack.extend(node.children());
            if matches!(
                node.kind(),
                FragmentKind::Element {
                    name: "img",
                    namespace: "http://www.w3.org/1999/xhtml"
                }
            ) {
                // An absent src is the declared unresolved-image degradation.
                let src = node
                    .attributes()
                    .find(|a| a.namespace.is_empty() && a.name == "src")
                    .map_or("", |a| a.value);
                let outcome = classify(src, &document);
                let size = match &outcome {
                    ImageOutcome::Ready(bytes) => bytes.len(),
                    ImageOutcome::Local(target) => target.model.len() + target.field.len(),
                    ImageOutcome::Warning(w) => format!("{w:?}").len(),
                };
                self.charge(
                    size + std::mem::size_of::<ImageOutcome>() + std::mem::size_of::<usize>(),
                    limits,
                )?;
                found.insert(node.index(), outcome);
            }
        }
        self.sources.insert(source, found);
        Ok(())
    }
    pub(crate) fn prepare(
        &mut self,
        limits: ReportLimits,
        ordinary: usize,
    ) -> Result<(), ReportError> {
        if self.prepared {
            return Ok(());
        }
        let mut groups: BTreeMap<(String, String), BTreeSet<i64>> = BTreeMap::new();
        let mut records = BTreeSet::new();
        for nodes in self.sources.values() {
            for outcome in nodes.values() {
                if let ImageOutcome::Local(target) = outcome {
                    records.insert((target.model.clone(), target.id));
                    if records.len().saturating_add(ordinary) > limits.data_records {
                        return Err(ReportError::Limit {
                            name: "data_records",
                            maximum: limits.data_records,
                        });
                    }
                    groups
                        .entry((target.model.clone(), target.field.clone()))
                        .or_default()
                        .insert(target.id);
                    if groups.len() > limits.plan_paths {
                        return Err(ReportError::Limit {
                            name: "html_image_groups",
                            maximum: limits.plan_paths,
                        });
                    }
                }
            }
        }
        self.pending = groups
            .into_iter()
            .map(|((model, field), ids)| HtmlImageRequest {
                model,
                field,
                ids: ids.into_iter().collect(),
            })
            .collect();
        self.prepared = true;
        Ok(())
    }
    pub(crate) fn request(&self) -> Option<HtmlImageRequest> {
        self.pending.front().cloned()
    }
    pub(crate) fn supply(
        &mut self,
        values: Vec<HtmlImageResult>,
        rejection: Option<&str>,
        limits: ReportLimits,
        document: Limits,
    ) -> Result<(), ReportError> {
        let request = self
            .pending
            .front()
            .cloned()
            .ok_or_else(|| crate::error::invalid("no HTML image read is pending"))?;
        let mut answers = BTreeMap::new();
        for (id, bytes) in values {
            if request.ids.binary_search(&id).is_err() || answers.contains_key(&id) {
                return Err(crate::error::invalid(
                    "HTML image answer contains an unrequested or duplicate record",
                ));
            }
            let outcome = match bytes {
                Ok(bytes) => admit(bytes, &document),
                Err(reason) => degraded(reason),
            };
            let size = match &outcome {
                ImageOutcome::Ready(bytes) => bytes.len(),
                ImageOutcome::Warning(w) => format!("{w:?}").len(),
                ImageOutcome::Local(_) => 0,
            };
            self.charge(size, limits)?;
            answers.insert(id, outcome);
        }
        let request = request.clone();
        for id in request.ids {
            let charged = answers.contains_key(&id) && rejection.is_none();
            let outcome = match rejection {
                Some(reason) => degraded(reason),
                None => answers
                    .remove(&id)
                    .unwrap_or_else(|| degraded("image record or value is missing")),
            };
            let size = match &outcome {
                ImageOutcome::Ready(bytes) => bytes.len(),
                ImageOutcome::Warning(w) => format!("{w:?}").len(),
                ImageOutcome::Local(_) => {
                    return Err(crate::error::invalid(
                        "an image answer cannot request another image",
                    ));
                }
            };
            self.charge(if charged { 0 } else { size }, limits)?;
            self.charge(
                request.model.len()
                    + request.field.len()
                    + std::mem::size_of::<LocalImage>()
                    + std::mem::size_of::<ImageOutcome>(),
                limits,
            )?;
            self.targets.insert(
                LocalImage {
                    model: request.model.clone(),
                    id,
                    field: request.field.clone(),
                },
                outcome,
            );
        }
        self.pending.pop_front();
        Ok(())
    }
    pub(crate) fn remaining(&self, limits: ReportLimits) -> Result<usize, ReportError> {
        let request = self
            .pending
            .front()
            .ok_or_else(|| crate::error::invalid("no HTML image read is pending"))?;
        let per = std::mem::size_of::<LocalImage>()
            + std::mem::size_of::<ImageOutcome>()
            + request.model.len()
            + request.field.len();
        let remaining = request
            .ids
            .len()
            .checked_mul(per)
            .and_then(|n| self.bytes.checked_add(n))
            .and_then(|n| limits.bytes.checked_sub(n))
            .filter(|n| *n > 0)
            .ok_or(ReportError::Limit {
                name: "html_image_bytes",
                maximum: limits.bytes,
            })?;
        Ok(remaining)
    }
    pub(crate) fn get(&self, source: &crate::HtmlSource, node: usize) -> Option<ImageOutcome> {
        let (model, id, _, field) = source.record()?;
        let outcome = self
            .sources
            .get(&(model.into(), id, field.into()))?
            .get(&node)?;
        Some(match outcome {
            ImageOutcome::Local(target) => self.targets.get(target)?.clone(),
            outcome => outcome.clone(),
        })
    }
}
