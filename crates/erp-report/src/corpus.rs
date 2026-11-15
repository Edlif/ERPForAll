// SPDX-License-Identifier: MIT OR Apache-2.0
//! Corpus scan: runs the library validator over an extractor bundle (`scripts/report-corpus.py`)
//! and counts the results per addon. No I/O and no execution of reference code. The report is static
//! evidence: it proves neither rendering nor attachment security, and says so in its own fields.
//!
//! Every counter here counts items of the bounded input (at most one step per template, diagnostic,
//! report or finding), so a count never exceeds the input's byte length.
use crate::{ReportError, ReportLimits, inspect_arch};
use erp_views::{LimitKind, ViewCatalogue, ViewDescriptor};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A refusal of the corpus scan, or the reason one template's call closure is unresolved.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CorpusError {
    /// The input is not a schema 1 extractor bundle.
    #[error("not a schema 1 extractor bundle: {0}")]
    Bundle(#[source] serde_json::Error),
    /// The bundle declares another schema version.
    #[error("bundle schema {0} is unsupported; supported schema is 1")]
    Schema(u32),
    /// Two templates share a key.
    #[error("duplicate template key `{0}`")]
    DuplicateTemplate(String),
    /// A primary copy names a parent the bundle does not hold.
    #[error("template `{template}` names the parent `{parent}`, which is not in the bundle")]
    MissingParent { template: String, parent: String },
    /// A static `t-call` names a template the bundle does not hold.
    #[error("template `{caller}` calls `{target}`, which is not in the bundle")]
    MissingTemplate { caller: String, target: String },
    /// A static `t-call` names an extension, which has no arch of its own to call.
    #[error("template `{caller}` calls `{target}`, which is an extension and not a root template")]
    CallsExtension { caller: String, target: String },
    /// Static calls form a cycle; the chain is written from its first repeated template.
    #[error("cyclic t-call {0}")]
    CyclicCall(String),
    /// A called template has no result of its own.
    #[error("called template `{0}` is unresolved")]
    Callee(String),
    /// A gate addon has no root template in the corpus.
    #[error("gate addon `{0}` has no root template in the corpus")]
    GateAddon(String),
    /// The gate was asked for no addon.
    #[error("the gate addon list is empty")]
    EmptyGate,
}

#[derive(Deserialize)]
struct Version {
    schema: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    #[serde(rename = "schema")]
    _schema: u32,
    reference_commit: String,
    provenance: serde_json::Value,
    field_metadata: String,
    templates: Vec<Template>,
    reports: Vec<Report>,
    field_declarations: Vec<FieldDeclaration>,
    findings: Vec<Finding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    key: String,
    addon: String,
    path: String,
    line: u32,
    arch: String,
    parent: Option<String>,
    primary: bool,
    priority: i32,
    /// Position in the reference's load order; equal-priority extensions apply in this order.
    sequence: u32,
    selected_by: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    key: String,
    template: String,
    model: String,
    attachment: bool,
    #[serde(rename = "path")]
    _path: String,
    #[serde(rename = "line")]
    _line: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldDeclaration {
    model: String,
    field: String,
    groups: Vec<String>,
    path: String,
    line: u32,
}
/// What an extraction finding leaves open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Scope {
    Templates,
    Fields,
    Security,
}
impl Scope {
    fn name(self) -> &'static str {
        match self {
            Self::Templates => "templates",
            Self::Fields => "fields",
            Self::Security => "security",
        }
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    path: String,
    line: u32,
    kind: String,
    scope: Scope,
    detail: String,
}

/// Result of one template. The order is the order of severity used for a call closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Validated,
    Dynamic,
    Invalid,
    Unresolved,
}
/// Root templates by result. The four counts always add up to the roots they describe.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct StatusCounts {
    validated: usize,
    dynamic: usize,
    invalid: usize,
    unresolved: usize,
}
impl StatusCounts {
    /// Roots with no diagnostic and no dynamic obligation.
    pub fn validated(&self) -> usize {
        self.validated
    }
    /// Roots whose static tokens are accepted and whose dynamic values wait for the evaluator.
    pub fn dynamic(&self) -> usize {
        self.dynamic
    }
    /// Roots with at least one token outside the profile.
    pub fn invalid(&self) -> usize {
        self.invalid
    }
    /// Roots the analysis could not finish; never a pass.
    pub fn unresolved(&self) -> usize {
        self.unresolved
    }
    fn add(&mut self, status: Status) {
        *match status {
            Status::Validated => &mut self.validated,
            Status::Dynamic => &mut self.dynamic,
            Status::Invalid => &mut self.invalid,
            Status::Unresolved => &mut self.unresolved,
        } += 1;
    }
    fn merge(&mut self, other: Self) {
        self.validated += other.validated;
        self.dynamic += other.dynamic;
        self.invalid += other.invalid;
        self.unresolved += other.unresolved;
    }
}
/// Counts of one addon (or a sum of addons). `own` is each root's combined arch; `closure` adds every
/// template it reaches through static `t-call`; `action_*` covers the roots a report action names.
/// Extensions are counted for their addon and are never a root.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct AddonCounts {
    roots: usize,
    extensions: usize,
    own: StatusCounts,
    closure: StatusCounts,
    action_roots: usize,
    action_closure: StatusCounts,
}
impl AddonCounts {
    /// Root templates: no parent, or a primary copy.
    pub fn roots(&self) -> usize {
        self.roots
    }
    /// Extension templates of this addon.
    pub fn extensions(&self) -> usize {
        self.extensions
    }
    /// Results of each root's own combined arch.
    pub fn own(&self) -> StatusCounts {
        self.own
    }
    /// Results of each root with the templates it calls.
    pub fn closure(&self) -> StatusCounts {
        self.closure
    }
    /// Roots named by a report action.
    pub fn action_roots(&self) -> usize {
        self.action_roots
    }
    /// Closure results of the roots named by a report action.
    pub fn action_closure(&self) -> StatusCounts {
        self.action_closure
    }
    fn merge(&mut self, other: &Self) {
        self.roots += other.roots;
        self.extensions += other.extensions;
        self.own.merge(other.own);
        self.closure.merge(other.closure);
        self.action_roots += other.action_roots;
        self.action_closure.merge(other.action_closure);
    }
}
#[derive(Debug, Serialize)]
struct Token {
    kind: String,
    token: String,
    line: u32,
}
#[derive(Debug, Serialize)]
struct Own {
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    diagnostics: Vec<Token>,
    /// Dynamic values by attribute; the expressions themselves stay in the reference.
    obligations: BTreeMap<String, usize>,
}
#[derive(Debug, Clone, Serialize)]
struct Closure {
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    calls: Vec<String>,
}
#[derive(Debug, Serialize)]
struct Entry {
    key: String,
    addon: String,
    path: String,
    line: u32,
    selected_by: Vec<String>,
    action_reports: Vec<String>,
    own: Own,
    closure: Closure,
}
#[derive(Debug, Default, Serialize)]
struct TokenCount {
    occurrences: usize,
    templates: usize,
}
#[derive(Debug, Default, Serialize)]
struct Reasons {
    own: BTreeMap<String, usize>,
    closure: BTreeMap<String, usize>,
}
#[derive(Debug, Serialize)]
struct Candidate {
    field: String,
    groups: Vec<String>,
    path: String,
    line: u32,
}
#[derive(Debug, Serialize)]
struct AttachmentReport {
    key: String,
    template: String,
    model: String,
    /// Literal declarations with `groups` on the report model: candidates, not a resolved verdict.
    restricted_field_candidates: Vec<Candidate>,
}
#[derive(Debug, Serialize)]
struct Sizes {
    templates: usize,
    reports: usize,
    field_declarations: usize,
    findings: BTreeMap<&'static str, usize>,
}
#[derive(Debug, Serialize)]
struct Permille {
    own_validated: usize,
    own_validated_or_dynamic: usize,
    closure_validated: usize,
    closure_validated_or_dynamic: usize,
    action_closure_validated: Option<usize>,
    action_closure_validated_or_dynamic: Option<usize>,
}
#[derive(Debug, Serialize)]
struct Gate {
    addons: Vec<String>,
    counts: AddonCounts,
    /// Shares of the roots (or action roots), in thousandths, rounded down.
    permille: Permille,
}
/// Static corpus evidence. A validated template has only tokens of the print profile; that proves
/// neither that it renders nor that an archived report is safe. Extraction findings, provenance and
/// the incomplete-analysis flags travel with the counts and must be published with them.
#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct CorpusReport {
    schema: u32,
    profile: u32,
    reference_commit: String,
    provenance: serde_json::Value,
    field_metadata: String,
    combination: &'static str,
    counts: Sizes,
    addons: BTreeMap<String, AddonCounts>,
    totals: AddonCounts,
    gate: Option<Gate>,
    missing_tokens: BTreeMap<String, TokenCount>,
    unresolved_reasons: Reasons,
    entries: Vec<Entry>,
    orphan_extensions: Vec<String>,
    reports_without_template: Vec<String>,
    attachment_reports: Vec<AttachmentReport>,
    extraction_findings: Vec<Finding>,
    discovery_complete: bool,
    field_metadata_complete: bool,
    missing_tokens_complete: bool,
    rendering_verified: bool,
    attachment_security_verified: bool,
}
impl CorpusReport {
    /// Counts per addon, by addon name.
    pub fn addons(&self) -> &BTreeMap<String, AddonCounts> {
        &self.addons
    }
    /// The sum over every addon.
    pub fn totals(&self) -> AddonCounts {
        self.totals
    }
    /// Add the sum over `addons` and its shares as the report's `gate` section. An addon without a
    /// root template is refused by name: a gate over fewer addons than asked would look better than
    /// it is.
    pub fn with_gate(mut self, addons: &[&str]) -> Result<Self, ReportError> {
        let names: BTreeSet<&str> = addons.iter().copied().collect();
        if names.is_empty() {
            return Err(CorpusError::EmptyGate.into());
        }
        let mut counts = AddonCounts::default();
        for name in &names {
            match self.addons.get(*name).filter(|c| c.roots > 0) {
                Some(found) => counts.merge(found),
                None => return Err(CorpusError::GateAddon((*name).into()).into()),
            }
        }
        let share = |part: usize, whole: usize| part.saturating_mul(1000).checked_div(whole);
        let of_roots = |part: usize| share(part, counts.roots).unwrap_or(0);
        let (own, closure, action) = (counts.own, counts.closure, counts.action_closure);
        self.gate = Some(Gate {
            addons: names.into_iter().map(Into::into).collect(),
            counts,
            permille: Permille {
                own_validated: of_roots(own.validated),
                own_validated_or_dynamic: of_roots(own.validated + own.dynamic),
                closure_validated: of_roots(closure.validated),
                closure_validated_or_dynamic: of_roots(closure.validated + closure.dynamic),
                action_closure_validated: share(action.validated, counts.action_roots),
                action_closure_validated_or_dynamic: share(
                    action.validated + action.dynamic,
                    counts.action_roots,
                ),
            },
        });
        Ok(self)
    }
}

/// Work of the whole scan. Running out refuses the run: it must never turn into per-template results.
struct Work {
    left: usize,
    maximum: usize,
}
impl Work {
    fn charge(&mut self, amount: usize) -> Result<(), ReportError> {
        self.left = self
            .left
            .checked_sub(amount)
            .ok_or_else(|| limit("corpus_work", self.maximum))?;
        Ok(())
    }
}
fn limit(name: &'static str, maximum: usize) -> ReportError {
    ReportError::Limit { name, maximum }
}
/// The error with every cause below the one its own message already shows.
fn text(error: &ReportError) -> String {
    let mut text = error.to_string();
    let mut cause = std::error::Error::source(error).and_then(|shown| shown.source());
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}
fn reason(error: &ReportError) -> String {
    match error {
        ReportError::View { .. } => "view".into(),
        ReportError::Expression { .. } => "expression".into(),
        ReportError::Limit { name, .. } => format!("limit:{name}"),
        ReportError::Corpus(CorpusError::MissingParent { .. }) => "missing_parent".into(),
        ReportError::Corpus(CorpusError::MissingTemplate { .. }) => "missing_template".into(),
        ReportError::Corpus(CorpusError::CallsExtension { .. }) => "call_to_extension".into(),
        ReportError::Corpus(CorpusError::CyclicCall(_)) => "cyclic_call".into(),
        ReportError::Corpus(CorpusError::Callee(_)) => "callee".into(),
        _ => "other".into(),
    }
}
/// The closure result of one template as seen from itself; `Failed` keeps the reason and message.
#[derive(Clone)]
enum Reach {
    Passed { status: Status, height: usize },
    Failed { reason: String, error: String },
}
impl Reach {
    fn failed(error: &ReportError) -> Self {
        Self::Failed {
            reason: reason(error),
            error: text(error),
        }
    }
}
/// What the closure needs of a root: its own result and the static call targets of its arch.
struct Root<'a> {
    status: Status,
    error: Option<String>,
    calls: &'a [String],
}
struct Closures<'a> {
    roots: &'a BTreeMap<&'a str, Root<'a>>,
    extensions: &'a BTreeSet<&'a str>,
    /// Results that do not depend on the caller chain. A cycle or a depth refusal is never kept.
    memo: BTreeMap<&'a str, Reach>,
    depth: usize,
}
impl<'a> Closures<'a> {
    /// `path` is the chain of callers above `key`. The flag is true when the result holds for any
    /// caller chain. The recursion is at most `depth` deep: a longer chain is refused first.
    fn visit(
        &mut self,
        key: &'a str,
        path: &mut Vec<&'a str>,
        work: &mut Work,
    ) -> Result<(Reach, bool), ReportError> {
        work.charge(1)?;
        let (roots, extensions, depth) = (self.roots, self.extensions, self.depth);
        let too_deep = move || Reach::failed(&limit("call_depth", depth));
        if let Some(at) = path.iter().position(|seen| *seen == key) {
            let chain = path[at..].iter().chain([&key]).copied();
            let chain = chain.collect::<Vec<_>>().join(" -> ");
            return Ok((Reach::failed(&CorpusError::CyclicCall(chain).into()), false));
        }
        if path.len() >= depth {
            return Ok((too_deep(), false));
        }
        if let Some(known) = self.memo.get(key) {
            return Ok(match known {
                Reach::Passed { height, .. } if path.len().saturating_add(*height) > depth => {
                    (too_deep(), false)
                }
                known => (known.clone(), true),
            });
        }
        let Some(root) = roots.get(key) else {
            // The caller checks the target before it descends; a root key is always present.
            return Ok((Reach::failed(&CorpusError::Callee(key.into()).into()), true));
        };
        let mut portable = true;
        let mut result = match &root.error {
            Some(error) => Reach::Failed {
                reason: "own".into(),
                error: error.clone(),
            },
            None => Reach::Passed {
                status: root.status,
                height: 1,
            },
        };
        if root.error.is_none() {
            path.push(key);
            for target in root.calls {
                let missing = |error: CorpusError| (Reach::failed(&error.into()), true);
                let (caller, callee) = (key.to_string(), target.clone());
                let (below, kept) = if extensions.contains(target.as_str()) {
                    missing(CorpusError::CallsExtension {
                        caller,
                        target: callee,
                    })
                } else if let Some((name, _)) = roots.get_key_value(target.as_str()) {
                    match self.visit(name, path, work)? {
                        (Reach::Failed { reason, .. }, kept) if reason == "own" => {
                            (Reach::failed(&CorpusError::Callee(callee).into()), kept)
                        }
                        other => other,
                    }
                } else {
                    missing(CorpusError::MissingTemplate {
                        caller,
                        target: callee,
                    })
                };
                match below {
                    Reach::Passed {
                        status: s,
                        height: h,
                    } => {
                        if let Reach::Passed { status, height } = &mut result {
                            *status = (*status).max(s);
                            *height = (*height).max(h.saturating_add(1));
                        }
                    }
                    failed => {
                        result = failed;
                        portable = kept;
                        break;
                    }
                }
            }
            path.pop();
        }
        if portable {
            self.memo.insert(key, result.clone());
        }
        Ok((result, portable))
    }
}

/// Scan an extractor bundle with the validator library callers use (`inspect_arch`).
///
/// Every root template (no parent, or a primary copy) is combined with every extension in the bundle
/// and gets two results: its own combined arch, and that arch with every template it reaches through
/// static `t-call`. Validated, dynamic, invalid and unresolved stay separate; unresolved is never a pass.
///
/// # Errors
///
/// [`ReportError::Limit`] `corpus_bytes`, `corpus_templates` or `corpus_work` when the input is over
/// the view limits `CatalogueBytes`, `CatalogueViews` or `BatchSteps` (lower them with
/// [`ReportLimits::with_views`]); [`ReportError::Corpus`] when the bundle is malformed, of another
/// schema, or holds a key twice. A limit refuses the run; no partial report is returned.
///
/// ```
/// use erp_report::{ReportLimits, scan_corpus};
/// let bundle = r#"{"schema": 1, "reference_commit": "example", "provenance": {},
///   "field_metadata": "literal candidates only", "reports": [], "field_declarations": [],
///   "findings": [], "templates": [{"key": "demo.page", "addon": "demo", "path": "demo.xml",
///   "line": 1, "arch": "<p class='fw-bold'>Hello</p>", "parent": null, "primary": false,
///   "priority": 16, "sequence": 1, "selected_by": ["path"]}]}"#;
/// let report = scan_corpus(bundle, ReportLimits::default())?;
/// assert_eq!(report.totals().own().validated(), 1);
/// # Ok::<(), erp_report::ReportError>(())
/// ```
pub fn scan_corpus(input: &str, limits: ReportLimits) -> Result<CorpusReport, ReportError> {
    let cap = limits.views.get(LimitKind::CatalogueBytes);
    if input.len() > cap {
        return Err(limit("corpus_bytes", cap));
    }
    let version: Version = serde_json::from_str(input).map_err(CorpusError::Bundle)?;
    if version.schema != 1 {
        return Err(CorpusError::Schema(version.schema).into());
    }
    let mut bundle: Bundle = serde_json::from_str(input).map_err(CorpusError::Bundle)?;
    let cap = limits.views.get(LimitKind::CatalogueViews);
    if bundle.templates.len() > cap {
        return Err(limit("corpus_templates", cap));
    }
    let maximum = limits.views.get(LimitKind::BatchSteps);
    let mut work = Work {
        left: maximum,
        maximum,
    };
    // View ids follow the load order, as the reference's record ids do: among extensions of equal
    // priority the one loaded first applies first. Results are listed by key.
    bundle
        .templates
        .sort_by(|a, b| (a.sequence, &a.key).cmp(&(b.sequence, &b.key)));
    let mut ids = BTreeMap::new();
    for (index, template) in bundle.templates.iter().enumerate() {
        work.charge(1)?;
        let id = i64::try_from(index)
            .ok()
            .and_then(|at| at.checked_add(1))
            .ok_or_else(|| limit("corpus_templates", cap))?;
        if ids.insert(template.key.clone(), id).is_some() {
            return Err(CorpusError::DuplicateTemplate(template.key.clone()).into());
        }
    }
    bundle.templates.sort_by(|a, b| a.key.cmp(&b.key));
    let templates = &bundle.templates;
    let is_root = |t: &Template| t.parent.is_none() || t.primary;
    let extensions: BTreeSet<&str> = templates
        .iter()
        .filter(|t| !is_root(t))
        .map(|t| t.key.as_str())
        .collect();
    let mut orphan_extensions = Vec::new();
    let descriptors = templates.iter().map(|t| {
        let view = ViewDescriptor::new(ids[t.key.as_str()], t.key.as_str(), "", t.arch.as_str())
            .with_priority(t.priority);
        match t.parent.as_deref().map(|parent| ids.get(parent)) {
            Some(Some(parent)) if t.primary => view.primary_of(*parent),
            Some(Some(parent)) => view.extension_of(*parent),
            Some(None) if !t.primary => {
                orphan_extensions.push(t.key.clone());
                view
            }
            _ => view,
        }
    });
    let catalogue =
        ViewCatalogue::with_limits(descriptors.collect(), limits.views).map_err(|source| {
            ReportError::View {
                template: "corpus".into(),
                source: Box::new(source),
            }
        })?;

    // Own result of every root, in key order.
    let mut owns = Vec::new();
    let mut calls = Vec::new();
    for template in templates.iter().filter(|t| is_root(t)) {
        work.charge(1)?;
        let key = template.key.as_str();
        let mut found = BTreeSet::new();
        let checked = match template.parent.as_deref() {
            Some(parent) if !ids.contains_key(parent) => Err(CorpusError::MissingParent {
                template: key.into(),
                parent: parent.into(),
            }
            .into()),
            _ => match catalogue.combine(ids[key]) {
                Err(source) => Err(ReportError::View {
                    template: key.into(),
                    source: Box::new(source),
                }),
                Ok(combined) => {
                    let arch = combined.arch();
                    work.charge(arch.node_count())?;
                    let mut nodes = vec![arch.root()];
                    while let Some(node) = nodes.pop() {
                        // The validator's rule: a value without `{` or `#` is a static template key.
                        if let Some(target) =
                            node.attr("t-call").filter(|v| !v.contains(['{', '#']))
                        {
                            found.insert(target.to_string());
                        }
                        nodes.extend(node.children());
                    }
                    inspect_arch(key, arch, limits)
                }
            },
        };
        let own = match checked {
            Ok(validation) => {
                let mut obligations = BTreeMap::new();
                for value in validation.obligations() {
                    *obligations
                        .entry(value.attribute().to_string())
                        .or_default() += 1;
                }
                let diagnostics: Vec<_> = validation
                    .diagnostics()
                    .iter()
                    .map(|d| Token {
                        kind: d.kind().to_string(),
                        token: d.token().to_string(),
                        line: d.line(),
                    })
                    .collect();
                let status = if !diagnostics.is_empty() {
                    Status::Invalid
                } else if !obligations.is_empty() {
                    Status::Dynamic
                } else {
                    Status::Validated
                };
                Own {
                    status,
                    reason: None,
                    error: None,
                    diagnostics,
                    obligations,
                }
            }
            Err(error) => Own {
                status: Status::Unresolved,
                reason: Some(reason(&error)),
                error: Some(text(&error)),
                diagnostics: Vec::new(),
                obligations: BTreeMap::new(),
            },
        };
        owns.push(own);
        calls.push(found.into_iter().collect::<Vec<_>>());
    }
    let roots: BTreeMap<&str, Root<'_>> = templates
        .iter()
        .filter(|t| is_root(t))
        .zip(owns.iter().zip(&calls))
        .map(|(t, (own, calls))| {
            let root = Root {
                status: own.status,
                error: own.error.clone(),
                calls: calls.as_slice(),
            };
            (t.key.as_str(), root)
        })
        .collect();
    let mut closures = Closures {
        roots: &roots,
        extensions: &extensions,
        memo: BTreeMap::new(),
        depth: limits.calls,
    };
    let mut reached = Vec::new();
    for key in roots.keys() {
        let (result, _) = closures.visit(key, &mut Vec::new(), &mut work)?;
        reached.push(result);
    }

    let mut action_reports: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut reports_without_template = Vec::new();
    bundle.reports.sort_by(|a, b| a.key.cmp(&b.key));
    for report in &bundle.reports {
        work.charge(1)?;
        match roots.get_key_value(report.template.as_str()) {
            Some((key, _)) => action_reports
                .entry(*key)
                .or_default()
                .push(report.key.clone()),
            None => reports_without_template.push(report.key.clone()),
        }
    }
    let mut addons: BTreeMap<String, AddonCounts> = BTreeMap::new();
    let mut missing_tokens: BTreeMap<String, TokenCount> = BTreeMap::new();
    let mut unresolved_reasons = Reasons::default();
    let mut entries = Vec::new();
    for template in templates.iter().filter(|t| !is_root(t)) {
        addons.entry(template.addon.clone()).or_default().extensions += 1;
    }
    let results = owns.into_iter().zip(calls.iter()).zip(reached);
    for (template, ((own, calls), reach)) in templates.iter().filter(|t| is_root(t)).zip(results) {
        let closure = match reach {
            Reach::Passed { status, .. } => Closure {
                status,
                reason: None,
                error: None,
                calls: calls.clone(),
            },
            Reach::Failed { reason, error } => Closure {
                status: Status::Unresolved,
                reason: Some(reason),
                error: Some(error),
                calls: calls.clone(),
            },
        };
        let actions = action_reports
            .remove(template.key.as_str())
            .unwrap_or_default();
        let count = addons.entry(template.addon.clone()).or_default();
        count.roots += 1;
        count.own.add(own.status);
        count.closure.add(closure.status);
        if !actions.is_empty() {
            count.action_roots += 1;
            count.action_closure.add(closure.status);
        }
        let mut seen = BTreeSet::new();
        for token in &own.diagnostics {
            let name = format!("{}:{}", token.kind, token.token);
            let counted = missing_tokens.entry(name.clone()).or_default();
            counted.occurrences += 1;
            if seen.insert(name) {
                counted.templates += 1;
            }
        }
        for (reasons, reason) in [
            (&mut unresolved_reasons.own, &own.reason),
            (&mut unresolved_reasons.closure, &closure.reason),
        ] {
            if let Some(reason) = reason {
                *reasons.entry(reason.clone()).or_default() += 1;
            }
        }
        entries.push(Entry {
            key: template.key.clone(),
            addon: template.addon.clone(),
            path: template.path.clone(),
            line: template.line,
            selected_by: template.selected_by.clone(),
            action_reports: actions,
            own,
            closure,
        });
    }
    let mut totals = AddonCounts::default();
    for count in addons.values() {
        totals.merge(count);
    }
    let attachment_reports = bundle
        .reports
        .iter()
        .filter(|r| r.attachment)
        .map(|report| {
            let mut candidates: Vec<_> = bundle
                .field_declarations
                .iter()
                .filter(|d| d.model == report.model && !d.groups.is_empty())
                .map(|d| Candidate {
                    field: d.field.clone(),
                    groups: d.groups.clone(),
                    path: d.path.clone(),
                    line: d.line,
                })
                .collect();
            candidates
                .sort_by(|a, b| (&a.field, &a.path, a.line).cmp(&(&b.field, &b.path, b.line)));
            AttachmentReport {
                key: report.key.clone(),
                template: report.template.clone(),
                model: report.model.clone(),
                restricted_field_candidates: candidates,
            }
        });
    let attachment_reports = attachment_reports.collect();
    let mut findings = BTreeMap::new();
    for finding in &bundle.findings {
        *findings.entry(finding.scope.name()).or_default() += 1;
    }
    Ok(CorpusReport {
        schema: 1,
        profile: 1,
        reference_commit: bundle.reference_commit,
        provenance: bundle.provenance,
        field_metadata: bundle.field_metadata,
        combination: "every extension in the bundle is applied",
        counts: Sizes {
            templates: bundle.templates.len(),
            reports: bundle.reports.len(),
            field_declarations: bundle.field_declarations.len(),
            findings: findings.clone(),
        },
        discovery_complete: !findings.contains_key(Scope::Templates.name())
            && orphan_extensions.is_empty()
            && reports_without_template.is_empty(),
        // Literal declarations never resolve inherited, computed or runtime field security.
        field_metadata_complete: false,
        missing_tokens_complete: totals.own.unresolved == 0,
        rendering_verified: false,
        attachment_security_verified: false,
        addons,
        totals,
        gate: None,
        missing_tokens,
        unresolved_reasons,
        entries,
        orphan_extensions,
        reports_without_template,
        attachment_reports,
        extraction_findings: bundle.findings,
    })
}
