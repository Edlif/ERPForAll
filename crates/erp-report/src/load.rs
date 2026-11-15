// SPDX-License-Identifier: MIT OR Apache-2.0
//! The data stage (design §4, step 3) without I/O. A [`DataStage`] turns a [`PrefetchPlan`] and
//! the ids of its context names into read requests, one per model and relation depth for the whole
//! batch, and the answers into the [`RecordStore`] evaluation reads. The caller runs each request
//! as one set-based read under the printing user's rights; nothing here reaches a database.
use crate::{
    FieldValue, Limits, PrefetchPlan, RecordStore, ReportError, ReportLimits, probe_image,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The task that runs registered helpers; until it lands a program that calls one is refused here.
const HELPERS_TASK: &str = "a later release";

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
/// The reads of one batch, in order. Ask for [`DataStage::request`], run it, hand the records to
/// [`DataStage::supply`], and repeat until there is no request; [`DataStage::finish`] gives the
/// store. The number of requests is at most the plan's levels, whatever the batch size. An error
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
}
impl<'p> DataStage<'p> {
    /// Start the reads of `plan` for the ids of its context names (`docs` and every declared name
    /// that holds records; see [`PrefetchPlan::roots`]).
    ///
    /// A name of the plan without ids, a name the plan does not have and a name given twice are
    /// [`ReportError::Data`] errors; more records than the data limit of `limits` is a
    /// [`ReportError::Limit`]; a program that calls a report helper is refused as
    /// [`ReportError::Unsupported`] until helper calls are run by the data stage. `document`
    /// carries the image limits.
    pub fn new(
        plan: &'p PrefetchPlan,
        roots: &[(&str, &[i64])],
        limits: ReportLimits,
        document: Limits,
    ) -> Result<Self, ReportError> {
        if plan.helpers().next().is_some() {
            return Err(ReportError::Unsupported {
                feature: "helper calls",
                task: HELPERS_TASK,
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
            let total: usize = self.asked.values().map(BTreeSet::len).sum();
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
    /// [`ReportError::Image`] naming the record and the field.
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
                // a binary value the plan reads is an image; empty bytes are the unset value
                if let Some(bytes) = value.bytes().filter(|bytes| !bytes.is_empty()) {
                    probe_image(bytes, &self.document).map_err(|error| ReportError::Image {
                        reason: match error {
                            ReportError::Image { reason } => {
                                format!("{model}({id}).{field}: {reason}")
                            }
                            other => format!("{model}({id}).{field}: {other}"),
                        },
                    })?;
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
    /// The reads answered so far.
    pub fn reads(&self) -> usize {
        self.reads
    }
    /// The store of everything that was read. A request that is still pending is a
    /// [`ReportError::Data`] error.
    pub fn finish(self) -> Result<RecordStore, ReportError> {
        if let Some(pending) = self.pending.front() {
            return Err(data(format!(
                "the read of {} is still pending",
                pending.model
            )));
        }
        let mut store = RecordStore::new();
        for (model, records) in self.loaded {
            for (id, record) in records {
                store.add(&model, id, record.display_name, record.fields)?;
            }
        }
        Ok(store)
    }
}
