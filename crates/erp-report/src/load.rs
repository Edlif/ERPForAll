// SPDX-License-Identifier: MIT OR Apache-2.0
//! The data stage (design §4, step 3) without I/O. A [`DataStage`] turns a [`PrefetchPlan`] and
//! the ids of its context names into read requests, one per model and relation depth for the whole
//! batch, and the answers into the [`RecordStore`] evaluation reads. The caller runs each request
//! as one set-based read under the printing user's rights; nothing here reaches a database.
use crate::{
    FieldValue, Limits, PrefetchPlan, RecordStore, ReportError, ReportLimits, probe_image,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;

fn data(reason: impl Into<String>) -> ReportError {
    ReportError::Data {
        reason: reason.into(),
    }
}
/// One set-based read: these fields of these records of one model, read with the printing user's
/// rights. Records that no longer exist are left out of the answer.
#[derive(Debug)]
pub struct ReadRequest<'a> {
    model: &'a str,
    ids: &'a [i64],
    fields: Vec<&'a str>,
    display_name: bool,
}
impl ReadRequest<'_> {
    pub fn model(&self) -> &str {
        self.model
    }
    /// The records to read, each once, in the order they were first reached.
    pub fn ids(&self) -> &[i64] {
        self.ids
    }
    /// The fields every returned record must carry, in name order.
    pub fn fields(&self) -> &[&str] {
        &self.fields
    }
    /// Whether every returned record must carry its display name too.
    pub fn display_name(&self) -> bool {
        self.display_name
    }
}
/// One record of an answer.
#[derive(Debug)]
pub struct LoadedRecord {
    id: i64,
    display_name: Option<String>,
    fields: BTreeMap<String, FieldValue>,
}
impl LoadedRecord {
    pub fn new(id: i64) -> Self {
        Self {
            id,
            display_name: None,
            fields: BTreeMap::new(),
        }
    }
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }
    /// Add one field value; a second value of the same field replaces the first.
    pub fn with_field(mut self, name: impl Into<String>, value: FieldValue) -> Self {
        self.fields.insert(name.into(), value);
        self
    }
}
#[derive(Debug)]
struct Pending {
    model: String,
    ids: Vec<i64>,
}
/// Progress belongs to the stage, including work spent finding an empty invocation.
#[derive(Debug)]
struct HelperProgress {
    index: usize,
    pending: Option<crate::HelperRequest>,
    work: crate::limits::Work,
    steps: usize,
    maximum: usize,
    failure: Option<(&'static str, usize)>,
}
impl HelperProgress {
    fn check_steps(&self, amount: usize) -> Result<usize, ReportError> {
        if let Some((name, maximum)) = self.failure {
            return Err(ReportError::Limit { name, maximum });
        }
        self.steps
            .checked_add(amount)
            .filter(|steps| *steps <= self.maximum)
            .ok_or(ReportError::Limit {
                name: "evaluation_steps",
                maximum: self.maximum,
            })
    }
    fn charge(&mut self, amount: usize) -> Result<(), ReportError> {
        let result = self.check_steps(amount).and_then(|steps| {
            self.work.charge(amount)?;
            self.steps = steps;
            Ok(())
        });
        if let Err(ReportError::Limit { name, maximum }) = &result {
            self.failure = Some((*name, *maximum));
        }
        result
    }
}
/// The reads of one batch, in order. Ask for [`DataStage::request`], run it, hand the records to
/// [`DataStage::supply`], and repeat until there is no request; [`DataStage::finish`] gives the
/// store. Also answer [`DataStage::precision_request`] once when present. Record requests are
/// bounded by plan levels, plus one precision lookup, whatever the batch size. An error
/// ends the stage.
///
/// ```
/// use erp_report::{
///     DataStage, FieldInfo, FieldValue, Language, Limits, LoadedRecord, PlanContext, ReportLimits,
///     ReportProgram, TemplateSource,
/// };
/// let sources = [TemplateSource::new("demo.report", "<div><p t-foreach='docs' t-as='o' t-out='o.name'/></div>")];
/// let limits = ReportLimits::default();
/// let program = ReportProgram::compile("demo.report", &sources, &Language::new("en_US")?, limits)?;
/// let fields = |_: &str, field: &str| (field == "name").then(|| FieldInfo::scalar(false));
/// let plan = program.prefetch_plan(&PlanContext::new("sale.order"), &fields)?;
/// let ids = [7_i64, 8];
/// let mut stage = DataStage::new(&plan, &[("docs", ids.as_slice())], limits, Limits::default())?;
/// while let Some(request) = stage.request() {
///     // One read of `request.fields()` for `request.ids()` of `request.model()`.
///     let records = request
///         .ids()
///         .iter()
///         .map(|id| LoadedRecord::new(*id).with_field("name", FieldValue::char("S0000")))
///         .collect();
///     stage.supply(records)?;
/// }
/// assert_eq!(stage.reads(), 1);
/// assert_eq!(stage.finish()?.len(), 2);
/// # Ok::<(), erp_report::ReportError>(())
/// ```
#[derive(Debug)]
pub struct DataStage<'p> {
    plan: &'p PrefetchPlan,
    document: Limits,
    maximum: usize,
    /// The relation depth whose reads are pending or next.
    depth: usize,
    /// The ids each path of the plan holds, in first-reached order.
    held: Vec<Vec<i64>>,
    pending: VecDeque<Pending>,
    asked: BTreeMap<String, BTreeSet<i64>>,
    loaded: BTreeMap<String, BTreeMap<i64, LoadedRecord>>,
    reads: usize,
    helpers: Mutex<HelperProgress>,
    helper_results: Vec<crate::records::HelperResults>,
    limits: ReportLimits,
    helper_bytes: usize,
    precisions: Option<BTreeMap<String, u8>>,
    html_images: crate::html_images::HtmlImages,
}
impl<'p> DataStage<'p> {
    /// Start the reads of `plan` for the ids of its context names (`docs` and every declared name
    /// that holds records; see [`PrefetchPlan::roots`]).
    ///
    /// A name of the plan without ids, a name the plan does not have and a name given twice are
    /// [`ReportError::Data`] errors; more records than the data limit of `limits` is a
    /// [`ReportError::Limit`]. After the reads, supply every helper answer before finishing.
    /// `document` carries the image limits.
    pub fn new(
        plan: &'p PrefetchPlan,
        roots: &[(&str, &[i64])],
        limits: ReportLimits,
        document: Limits,
    ) -> Result<Self, ReportError> {
        if plan.precision_names().len() > limits.data_records {
            return Err(ReportError::Limit {
                name: "data_records",
                maximum: limits.data_records,
            });
        }
        let mut held = vec![Vec::new(); plan.paths.len()];
        let mut given = BTreeSet::new();
        for (name, ids) in roots {
            let path = plan.root(name).ok_or_else(|| {
                data(format!(
                    "`{name}` is not a name of the report context that holds records"
                ))
            })?;
            if !given.insert(*name) {
                return Err(data(format!("the ids of `{name}` were given twice")));
            }
            let mut seen = BTreeSet::new();
            held[path] = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
        }
        if let Some((name, _)) = plan.roots().find(|(name, _)| !given.contains(name)) {
            return Err(data(format!("the ids of `{name}` were not given")));
        }
        let mut stage = Self {
            plan,
            document,
            maximum: limits.data_records,
            depth: 0,
            held,
            pending: VecDeque::new(),
            asked: BTreeMap::new(),
            loaded: BTreeMap::new(),
            reads: 0,
            helpers: Mutex::new(HelperProgress {
                index: 0,
                pending: None,
                work: limits.work(),
                steps: 0,
                maximum: limits.steps,
                failure: None,
            }),
            helper_results: Vec::new(),
            limits,
            helper_bytes: 0,
            precisions: (plan.precision_names().len() == 0).then(BTreeMap::new),
            html_images: crate::html_images::HtmlImages::default(),
        };
        stage.schedule()?;
        Ok(stage)
    }
    /// Queue the reads of the current depth; a depth with nothing to read is passed over.
    fn schedule(&mut self) -> Result<(), ReportError> {
        let plan = self.plan;
        while self.pending.is_empty() && self.depth < plan.depths() {
            let mut reads: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
            for (index, path) in plan.paths.iter().enumerate() {
                if path.depth != self.depth || !path.reads() {
                    continue;
                }
                // A record another path already asked for is read once, whole.
                let asked = self.asked.entry(path.model.clone()).or_default();
                let ids = reads.entry(path.model.as_str()).or_default();
                ids.extend(
                    self.held[index]
                        .iter()
                        .copied()
                        .filter(|id| asked.insert(*id)),
                );
            }
            let total = self
                .asked
                .values()
                .try_fold(plan.precision_names().len(), |total, ids| {
                    total.checked_add(ids.len())
                })
                .ok_or(ReportError::Limit {
                    name: "data_records",
                    maximum: self.maximum,
                })?;
            if total > self.maximum {
                return Err(ReportError::Limit {
                    name: "data_records",
                    maximum: self.maximum,
                });
            }
            self.pending = reads
                .into_iter()
                .filter(|(_, ids)| !ids.is_empty())
                .map(|(model, ids)| Pending {
                    model: model.into(),
                    ids,
                })
                .collect();
            if self.pending.is_empty() {
                self.descend()?;
            }
        }
        Ok(())
    }
    /// Follow the relations of the current depth: the ids the next depth's paths hold.
    fn descend(&mut self) -> Result<(), ReportError> {
        let plan = self.plan;
        for (index, path) in plan.paths.iter().enumerate() {
            if path.depth != self.depth {
                continue;
            }
            let records = self.loaded.get(&path.model);
            for (field, child) in &path.children {
                let target = &plan.paths[*child].model;
                let mut seen = BTreeSet::new();
                let mut ids = Vec::new();
                for id in &self.held[index] {
                    // A record that no longer exists was left out of its read.
                    let Some(record) = records.and_then(|records| records.get(id)) else {
                        continue;
                    };
                    let related = record.fields.get(field).and_then(FieldValue::relation);
                    let Some((model, more)) = related else {
                        return Err(data(format!(
                            "{}.{field} is a relation in the plan and was not read as one",
                            path.model
                        )));
                    };
                    if model != target {
                        return Err(data(format!(
                            "{}.{field} was read as a relation to {model}; the plan says {target}",
                            path.model
                        )));
                    }
                    ids.extend(more.iter().copied().filter(|id| seen.insert(*id)));
                }
                self.held[*child] = ids;
            }
        }
        self.depth += 1;
        Ok(())
    }
    /// The read to run now, or `None` when everything is loaded.
    pub fn request(&self) -> Option<ReadRequest<'_>> {
        let pending = self.pending.front()?;
        let read = self.plan.models.get(&pending.model)?;
        Some(ReadRequest {
            model: &pending.model,
            ids: &pending.ids,
            fields: read.fields.iter().map(String::as_str).collect(),
            display_name: read.display_name,
        })
    }
    /// The answer to the current request: every existing record among its ids, each with exactly
    /// the requested fields, and the display name when it was asked for.
    ///
    /// A record that was not asked for or comes twice, a missing or extra field and a missing
    /// display name are [`ReportError::Data`] errors. A binary value over an image limit is a
    /// [`ReportError::Image`] naming the record and the field; bytes that are no picture are kept
    /// (the evaluator prints a placeholder for them).
    pub fn supply(&mut self, records: Vec<LoadedRecord>) -> Result<(), ReportError> {
        let Some(pending) = self.pending.pop_front() else {
            return Err(data("no read is pending"));
        };
        let model = pending.model.as_str();
        let read = self
            .plan
            .models
            .get(model)
            .ok_or_else(|| data(format!("the plan reads nothing of {model}")))?;
        let asked: BTreeSet<i64> = pending.ids.iter().copied().collect();
        let loaded = self.loaded.entry(pending.model.clone()).or_default();
        for record in records {
            let id = record.id;
            if !asked.contains(&id) {
                return Err(data(format!("{model}({id}) was read but not asked for")));
            }
            if read.display_name && record.display_name.is_none() {
                return Err(data(format!(
                    "the display name of {model}({id}) is missing from its read"
                )));
            }
            for field in &read.fields {
                let Some(value) = record.fields.get(field) else {
                    return Err(data(format!(
                        "{model}({id}).{field} is missing from its read"
                    )));
                };
                if matches!(value.kind, crate::records::Kind::Html)
                    && let erp_expr::ExprValue::Str(text) = &value.value
                {
                    self.html_images.scan(
                        (model.into(), id, field.clone()),
                        text.as_str(),
                        self.limits,
                        self.document,
                    )?;
                }
                // A binary value the plan reads is a picture; empty bytes are the unset value. One
                // over a picture limit is refused here, by record and field. Bytes that are no
                // picture are kept: where a template prints them, an empty box and a warning
                // stand for them, so one bad picture does not stop the print.
                if let Some(bytes) = value.bytes().filter(|bytes| !bytes.is_empty())
                    && let Err(error @ ReportError::DocumentLimit { .. }) =
                        probe_image(bytes, &self.document)
                {
                    return Err(ReportError::Image {
                        reason: format!("{model}({id}).{field}: {error}"),
                    });
                }
            }
            if let Some(extra) = record.fields.keys().find(|f| !read.fields.contains(*f)) {
                return Err(data(format!(
                    "{model}({id}).{extra} was read but is not in the plan"
                )));
            }
            if loaded.insert(id, record).is_some() {
                return Err(data(format!("{model}({id}) was read twice")));
            }
        }
        self.reads += 1;
        if self.pending.is_empty() {
            self.descend()?;
            self.schedule()?;
        }
        Ok(())
    }
    /// The next helper batch. Field reads must finish first. Empty receiver batches are skipped.
    /// Repeated polls return the retained batch without repeating dependency analysis. Work and
    /// accepted result counts share the stage's cumulative evaluation-step budget.
    pub fn helper_request(&self) -> Result<Option<crate::HelperRequest>, ReportError> {
        if !self.pending.is_empty() {
            return Err(data("field reads must complete before report helpers"));
        }
        let mut progress = self
            .helpers
            .lock()
            .map_err(|_| data("report helper state was poisoned"))?;
        progress.check_steps(0)?;
        if let Some(request) = &progress.pending {
            return Ok(Some(request.clone()));
        }
        while let Some(invocation) = self.plan.invocations.get(progress.index) {
            progress.charge(1)?;
            let mut ids = BTreeSet::new();
            for path in &invocation.paths {
                progress.charge(self.held[*path].len())?;
                ids.extend(self.held[*path].iter().copied().filter(|id| {
                    self.loaded
                        .get(&invocation.model)
                        .is_some_and(|rows| rows.contains_key(id))
                }));
            }
            if ids.is_empty() {
                progress.index += 1;
                continue;
            }
            let mut permissions: BTreeMap<(String, i64), BTreeSet<String>> = BTreeMap::new();
            for id in &ids {
                permissions
                    .entry((invocation.model.clone(), *id))
                    .or_default()
                    .insert("id".into());
            }
            for dependency in &invocation.dependencies {
                let mut current: Vec<_> = ids
                    .iter()
                    .map(|id| (invocation.model.clone(), *id))
                    .collect();
                let segments: Vec<_> = dependency.split('.').collect();
                for (index, field) in segments.iter().enumerate() {
                    let mut next = BTreeSet::new();
                    for (model, id) in current {
                        progress.charge(1)?;
                        let Some(record) = self.loaded.get(&model).and_then(|rows| rows.get(&id))
                        else {
                            continue;
                        };
                        let allowed = permissions.entry((model.clone(), id)).or_default();
                        allowed.insert("id".into());
                        allowed.insert((*field).into());
                        if index + 1 < segments.len() {
                            let (target, related) = record
                                .fields
                                .get(*field)
                                .and_then(FieldValue::relation)
                                .ok_or_else(|| {
                                    data(format!(
                                        "helper {}.{} dependency {dependency} was not loaded",
                                        invocation.model, invocation.method
                                    ))
                                })?;
                            progress.charge(related.len())?;
                            next.extend(related.iter().map(|id| (target.to_owned(), *id)));
                        }
                    }
                    current = next.into_iter().collect();
                }
            }
            let mut grouped: BTreeMap<(String, Vec<String>), Vec<i64>> = BTreeMap::new();
            for ((model, id), fields) in permissions {
                grouped
                    .entry((model, fields.into_iter().collect()))
                    .or_default()
                    .push(id);
            }
            let reads = grouped
                .into_iter()
                .map(|((model, fields), ids)| crate::HelperRead { model, fields, ids })
                .collect();
            let request = crate::HelperRequest {
                invocation: invocation.clone(),
                ids: ids.into_iter().collect(),
                reads,
            };
            progress.pending = Some(request.clone());
            return Ok(Some(request));
        }
        Ok(None)
    }
    /// Supply exactly one bounded scalar per receiver, in request order. No result is kept on failure.
    pub fn supply_helper(&mut self, values: Vec<serde_json::Value>) -> Result<(), ReportError> {
        let request = self
            .helper_request()?
            .ok_or_else(|| data("no report helper is pending"))?;
        if values.len() != request.ids().len() {
            return Err(data(format!(
                "helper {}.{} returned {} results for {} records",
                request.model(),
                request.method(),
                values.len(),
                request.ids().len()
            )));
        }
        // Refuse the total before constructing any retained result, including empty strings.
        // A malformed answer leaves both the pending batch and successful-result charges intact.
        self.helpers
            .lock()
            .map_err(|_| data("report helper state was poisoned"))?
            .check_steps(values.len())?;
        let mut bytes = self.helper_bytes;
        for value in &values {
            bytes = bytes
                .checked_add(crate::helper::scalar_bytes(value))
                .and_then(|bytes| bytes.checked_add(std::mem::size_of::<i64>()))
                .filter(|bytes| *bytes <= self.limits.bytes)
                .ok_or(ReportError::Limit {
                    name: "evaluation_bytes",
                    maximum: self.limits.bytes,
                })?;
        }
        let scalars = values
            .into_iter()
            .map(|value| {
                crate::helper::scalar(value).map_err(|error| {
                    data(format!(
                        "helper {}.{}: {error}",
                        request.model(),
                        request.method()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let progress = self
            .helpers
            .get_mut()
            .map_err(|_| data("report helper state was poisoned"))?;
        progress.charge(scalars.len())?;
        let results = request.ids().iter().copied().zip(scalars).collect();
        progress.index += 1;
        progress.pending = None;
        self.helper_bytes = bytes;
        self.helper_results.push(crate::records::HelperResults {
            invocation: request.invocation,
            values: results,
        });
        Ok(())
    }
    /// The names for one set-based precision read, in name order. None means no read is pending.
    /// The caller reads `decimal.precision` name/digits under the printing user's rights.
    pub fn precision_request(&self) -> Option<impl ExactSizeIterator<Item = &str>> {
        self.precisions
            .is_none()
            .then(|| self.plan.precision_names())
    }
    /// Supply the complete precision answer atomically. Missing, repeated, duplicate or extra
    /// names are [`ReportError::Data`] errors. An invalid answer leaves the request pending.
    pub fn supply_precisions(
        &mut self,
        values: Vec<crate::LoadedPrecision>,
    ) -> Result<(), ReportError> {
        if self.precisions.is_some() {
            return Err(data("no decimal precision read is pending"));
        }
        let requested: BTreeSet<_> = self.plan.precision_names().collect();
        let mut precisions = BTreeMap::new();
        for value in values {
            if !requested.contains(value.name.as_str()) {
                return Err(data(format!(
                    "decimal precision `{}` was not requested",
                    value.name
                )));
            }
            if precisions.contains_key(&value.name) {
                return Err(data(format!(
                    "decimal precision `{}` was supplied twice",
                    value.name
                )));
            }
            precisions.insert(value.name, value.digits);
        }
        for name in requested {
            if !precisions.contains_key(name) {
                return Err(crate::precision::missing(name));
            }
        }
        self.precisions = Some(precisions);
        self.reads += 1;
        Ok(())
    }
    /// The next auxiliary HTML image projection, after the declared field reads. No I/O occurs here.
    /// Distinct model/field groups and the total records/bytes are bounded by ReportLimits.
    pub fn image_request(&mut self) -> Result<Option<crate::HtmlImageRequest>, ReportError> {
        if !self.pending.is_empty() {
            return Err(data("field reads must complete before HTML images"));
        }
        let ordinary = self
            .asked
            .values()
            .try_fold(self.plan.precision_names().len(), |total, ids| {
                total.checked_add(ids.len())
            })
            .ok_or(ReportError::Limit {
                name: "data_records",
                maximum: self.maximum,
            })?;
        self.html_images.prepare(self.limits, ordinary)?;
        Ok(self.html_images.request())
    }
    /// Answer the current image request with raw bytes of existing records. Missing records degrade.
    /// Duplicate or unrequested IDs are errors; image byte/pixel/header failures are warnings.
    pub fn supply_images(
        &mut self,
        records: Vec<(i64, std::sync::Arc<[u8]>)>,
    ) -> Result<(), ReportError> {
        self.html_images.supply(
            records
                .into_iter()
                .map(|(id, bytes)| (id, Ok(bytes)))
                .collect(),
            None,
            self.limits,
            self.document,
        )
    }
    /// Per-image byte cap and remaining aggregate payload budget of the pending projection.
    /// Metadata for its targets is reserved before the adapter requests payloads.
    pub fn image_byte_budget(&self) -> Result<(usize, usize), ReportError> {
        Ok((
            self.document.get(crate::LimitKind::ImageBytes),
            self.html_images.remaining(self.limits)?,
        ))
    }
    /// Answer with admitted bytes or explicit content rejections. Access failures must be propagated
    /// by the adapter, never supplied as content rejections.
    pub fn supply_image_results(
        &mut self,
        records: Vec<crate::HtmlImageResult>,
    ) -> Result<(), ReportError> {
        self.html_images
            .supply(records, None, self.limits, self.document)
    }
    /// Degrade an unavailable or nonbinary target projection without reading it. Never use this to
    /// swallow an ORM access refusal or a database error; those must end the data stage.
    pub fn reject_images(&mut self, reason: &str) -> Result<(), ReportError> {
        self.html_images
            .supply(Vec::new(), Some(reason), self.limits, self.document)
    }
    /// The record reads and precision lookup answered so far; excludes helper calls.
    pub fn reads(&self) -> usize {
        self.reads
    }
    /// The store of everything that was read. A request that is still pending is a
    /// [`ReportError::Data`] error.
    pub fn finish(mut self) -> Result<RecordStore, ReportError> {
        if let Some(pending) = self.pending.front() {
            return Err(data(format!(
                "the read of {} is still pending",
                pending.model
            )));
        }
        if self.helper_request()?.is_some() {
            return Err(data("a report helper answer is still pending"));
        }
        if let Some(mut names) = self.precision_request()
            && let Some(name) = names.next()
        {
            return Err(crate::precision::missing(name));
        }
        if self.image_request()?.is_some() {
            return Err(data("an HTML image read is still pending"));
        }
        let mut store = RecordStore::new();
        store.precisions = self
            .precisions
            .ok_or_else(|| data("decimal precision answer is still pending"))?;
        store.html_images = self.html_images;
        store.helpers = self.helper_results;
        for (model, records) in self.loaded {
            for (id, record) in records {
                store.add(&model, id, record.display_name, record.fields)?;
            }
        }
        Ok(store)
    }
}
