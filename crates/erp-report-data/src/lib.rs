// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ORM side of the report data stage (design, step 3).
//!
//! `erp-report` plans what a report program reads ([`PrefetchPlan`]) and sequences the reads
//! ([`DataStage`]) without touching a database. This crate runs them: [`load`] answers every
//! request of the stage with one set-based ORM read in the caller's environment, so access rights,
//! record rules and field groups decide exactly as they do for any other read of that user, and
//! the number of reads follows the plan, never the number of records. The one exception is the
//! ORM's own: the display name of a many2one target, which the many2one read gives as a form shows
//! it. [`field_info`] describes the
//! registry's fields to the planner.
#![allow(missing_docs)]

mod helpers;
mod html_images;
pub use helpers::report_helpers;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use erp_fields::{Digits, FieldType, Value};
use erp_orm::{Env, Load, Records, Transaction};
use erp_registry::{FieldSpec, ModelSpec, Registry};
use erp_report::{
    DataStage, FieldInfo, FieldValue, Limits, LoadedRecord, PrefetchPlan, ReadRequest, RecordStore,
    ReportError, ReportLimits,
};

const DISPLAY_NAME: &str = "display_name";

/// A failed data stage.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DataError {
    /// The plan or the stage refused: a limit, an answer that does not match its request, an image
    /// over the image limits.
    #[error(transparent)]
    Report(#[from] ReportError),
    /// The ORM refused or failed the read: no access to a model, a record or a field, an unknown
    /// model, a database error.
    #[error(transparent)]
    Orm(#[from] erp_fields::Error),
    /// The plan reads a field whose type has no report value.
    #[error("report data: {model}.{field} is a {kind} field, which reports cannot print")]
    UnsupportedField {
        /// The model of the field.
        model: String,
        /// The field.
        field: String,
        /// The field's type name, with the reason when only a variant of the type is refused.
        kind: String,
    },
    /// A field's metadata or a value the ORM returned does not fit the field's type. Nothing is
    /// put in its place.
    #[error("report data: {model}.{field}: {reason}")]
    Value {
        /// The model of the field.
        model: String,
        /// The field.
        field: String,
        /// What was expected and what was found.
        reason: String,
    },
    /// The stage asked for records of a model without any field or display name. A read without
    /// field names would return every field, so it is refused.
    #[error("report data: the data stage asked to read no field of {model}")]
    EmptyRequest {
        /// The model of the request.
        model: String,
    },
}

/// The planner's description of `model.field` in `registry`: the target model of a many2one,
/// one2many or many2many, the currency field of a monetary field, and whether the field declares
/// groups. `None` for an unknown model or field. It is the field lookup
/// `ReportProgram::prefetch_plan` takes: `&|model, field| field_info(&registry, model, field)`.
pub fn field_info(registry: &Registry, model: &str, field: &str) -> Option<FieldInfo> {
    let spec = registry.model(model)?.field(field)?;
    let restricted = spec.groups.is_some();
    Some(match &spec.ty {
        FieldType::Many2one { comodel, .. }
        | FieldType::One2many { comodel, .. }
        | FieldType::Many2many { comodel, .. } => FieldInfo::relation(comodel.as_str(), restricted),
        FieldType::Monetary {
            currency_field: Some(currency),
        } => FieldInfo::scalar(restricted).with_currency_field(currency.as_str()),
        FieldType::Html => FieldInfo::scalar(restricted).with_html(),
        _ => FieldInfo::scalar(restricted),
    })
}

/// Policy for dynamic HTML image dependencies. This never grants ORM read access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlImagePolicy {
    /// Conservative default when the caller has not supplied archive context.
    UnrestrictedFields,
    /// Non-archived output, or an archive explicitly acknowledging group-restricted fields.
    CallerRights,
}
/// Data-stage budgets and archive policy, supplied by the trusted report integration.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LoadOptions {
    limits: ReportLimits,
    document: Limits,
    image_policy: HtmlImagePolicy,
}
impl LoadOptions {
    /// Use explicit budgets and conservatively refuse dynamic group-restricted image fields.
    pub fn new(limits: ReportLimits, document: Limits) -> Self {
        Self {
            limits,
            document,
            image_policy: HtmlImagePolicy::UnrestrictedFields,
        }
    }
    /// Select caller-rights resolution only after deciding the report's archive policy.
    pub fn with_html_image_policy(mut self, policy: HtmlImagePolicy) -> Self {
        self.image_policy = policy;
        self
    }
}

/// Read everything `plan` needs for the records of `roots` (each context name of the plan with its ids)
/// and return the store evaluation reads from.
///
/// Every request of the data stage is one ORM read of its fields for all its records, in `env`:
/// the caller's environment decides what may be read, and nothing here elevates it. Many2one
/// values are read as ids, and their targets by the next request. Selection labels are the
/// declared ones.
///
/// A target
/// reached any other way (a root, a one2many or many2many) is read with the caller's rights, and
/// so is every other field of a target.
///
/// # Errors
///
/// - [`DataError::Orm`]: the environment may not read a model, a record or a field of the plan
///   (the ORM's `Access` error, unchanged), a model of the plan is not in the registry, or the
///   database failed.
/// - [`DataError::Report`]: `roots` do not match the plan, a limit of `limits` or an image limit
///   of `document` is exceeded, or the program calls a report helper.
/// - [`DataError::UnsupportedField`]: the plan reads a field of a type reports cannot print, or
///   a selection whose labels come from a method. It is raised before the model is read.
/// - [`DataError::Value`]: a field of the plan is not on its model, a monetary field has no
///   currency field, a stored selection key has no declared label, or a value does not have the
///   shape of its field type.
/// - [`DataError::EmptyRequest`]: a request without fields.
pub async fn load(
    tx: &mut Transaction,
    env: &Env,
    plan: &PrefetchPlan,
    roots: &[(&str, &[i64])],
    limits: ReportLimits,
    document: Limits,
) -> Result<RecordStore, DataError> {
    load_with_options(tx, env, plan, roots, LoadOptions::new(limits, document)).await
}

/// Load declared fields and HTML images with explicit archive policy and admission budgets.
/// ORM access/database failures are returned unchanged; malformed image content degrades in the report.
pub async fn load_with_options(
    tx: &mut Transaction,
    env: &Env,
    plan: &PrefetchPlan,
    roots: &[(&str, &[i64])],
    options: LoadOptions,
) -> Result<RecordStore, DataError> {
    let mut stage = DataStage::new(plan, roots, options.limits, options.document)?;
    let names_only: BTreeSet<String> = plan
        .levels()
        .iter()
        .filter(|level| level.display_name() && level.fields().is_empty())
        .map(|level| level.model().to_owned())
        .collect();
    let mut labels = Labels::new();
    #[allow(
        clippy::while_let_loop,
        reason = "a `while let` keeps the request, which owns a Vec, alive across `supply`"
    )]
    loop {
        // the request borrows the stage: it ends with this statement, before `supply`
        let records = match stage.request() {
            Some(request) => read(tx, env, &request, &names_only, &mut labels).await?,
            None => break,
        };
        stage.supply(records)?;
    }
    while let Some(request) = stage.helper_request()? {
        let results = helpers::execute(tx, env, &request).await?;
        stage.supply_helper(results)?;
    }
    html_images::load(tx, env, &mut stage, options.image_policy).await?;
    Ok(stage.finish()?)
}

/// What a field's values convert to, settled from the registry before the model is read.
enum Kind<'a> {
    Char,
    Text,
    Html,
    Integer,
    /// The decimal digits the field declares, if any.
    Float(Option<u8>),
    /// The currency field of the same record.
    Monetary(&'a str),
    Boolean,
    /// A binary or image field: the stored bytes.
    Binary,
    /// The declared `(key, label)` pairs.
    Selection(&'a [(String, String)]),
    Date,
    Datetime,
    /// The target model.
    One(&'a str),
    /// The target model.
    Many(&'a str),
}

fn value_error(model: &str, field: &str, reason: impl Into<String>) -> DataError {
    DataError::Value {
        model: model.to_owned(),
        field: field.to_owned(),
        reason: reason.into(),
    }
}

fn unsupported(model: &str, field: &str, kind: impl Into<String>) -> DataError {
    DataError::UnsupportedField {
        model: model.to_owned(),
        field: field.to_owned(),
        kind: kind.into(),
    }
}

/// What kind of value the ORM returned, for an error text (never the value itself).
fn shape(value: &Value) -> &'static str {
    match value {
        Value::False => "no value",
        Value::Bool(_) => "a boolean",
        Value::Int(_) => "an integer",
        Value::Float(_) => "a float",
        Value::Decimal(_) => "a decimal",
        Value::Str(_) => "a string",
        Value::Bytes(_) => "bytes",
        Value::Date(_) => "a date",
        Value::DateTime(_) => "a datetime",
        Value::List(_) => "a list",
        _ => "a value of another kind",
    }
}

fn mismatch(model: &str, field: &str, expected: &str, value: &Value) -> DataError {
    value_error(
        model,
        field,
        format!("expected {expected}, the read returned {}", shape(value)),
    )
}

/// The many2one labels read so far, by target model and id: the display name, or `None` for a
/// target that no longer exists.
type Labels = BTreeMap<String, BTreeMap<i64, Option<String>>>;

/// One read of the stage: the request's fields of all its records, as the ORM returns them for
/// `env`, converted by field type. A request for display names only is answered from `labels`
/// for the ids they hold; the many2one fields read here whose target model is in `names_only`
/// add their labels.
async fn read(
    tx: &mut Transaction,
    env: &Env,
    request: &ReadRequest<'_>,
    names_only: &BTreeSet<String>,
    labels: &mut Labels,
) -> Result<Vec<LoadedRecord>, DataError> {
    let model = request.model();
    let mut loaded = Vec::new();
    let mut ids = request.ids().to_vec();
    if request.fields().is_empty()
        && request.display_name()
        && let Some(known) = labels.get(model)
    {
        ids.retain(|id| match known.get(id) {
            Some(Some(name)) => {
                loaded.push(LoadedRecord::new(*id).with_display_name(name.as_str()));
                false
            }
            // a target that no longer exists is left out, as a read leaves it out
            Some(None) => false,
            None => true,
        });
        if ids.is_empty() {
            return Ok(loaded);
        }
    }
    let records = env.records(model)?.browse(ids)?;
    let registry = Arc::clone(env.registry());
    let spec = registry.model_by_id(records.model_id());
    let fields = request
        .fields()
        .iter()
        .map(|name| {
            spec.field(name)
                .ok_or_else(|| value_error(model, name, "the model has no such field"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut names: Vec<&str> = request.fields().to_vec();
    if request.display_name() {
        names.push(DISPLAY_NAME);
    }
    if names.is_empty() {
        return Err(DataError::EmptyRequest {
            model: model.to_owned(),
        });
    }
    // every field is settled before the read, so an unprintable field costs no statement
    let fixed = fields
        .iter()
        .map(|field| fixed_kind(model, field))
        .collect::<Result<Vec<_>, _>>()?;
    let named = named_digits(tx, &records, &fields).await?;
    let mut columns = Vec::with_capacity(fields.len());
    for (field, kind) in fields.iter().zip(fixed) {
        let kind = match kind {
            Some(kind) => kind,
            // a float with a named precision
            None => Kind::Float(Some(named.get(field.name).copied().ok_or_else(|| {
                value_error(model, field.name, "its decimal precision was not resolved")
            })?)),
        };
        columns.push((field.name, kind));
    }
    // A read leaves out the records that no longer exist only when one of its fields is read
    // from a column. Without one, the existing records are asked for first.
    let records = if reads_a_column(spec, &fields, request.display_name()) {
        records
    } else {
        records.exists(tx).await?
    };
    let rows = records.read_with(tx, &names, Load::Raw).await?;
    let labelled: Vec<(&str, &str)> = columns
        .iter()
        .filter_map(|(name, kind)| match kind {
            Kind::One(target) if names_only.contains(*target) => Some((*name, *target)),
            _ => None,
        })
        .collect();
    if !labelled.is_empty() {
        let fields: Vec<&str> = labelled.iter().map(|(name, _)| *name).collect();
        let classic = records.read_with(tx, &fields, Load::Classic).await?;
        add_labels(model, &rows, &classic, &labelled, labels)?;
    }
    loaded.reserve(rows.len());
    for row in &rows {
        let id = match row.get("id") {
            Some(Value::Int(id)) => *id,
            Some(other) => return Err(mismatch(model, "id", "an integer", other)),
            None => return Err(value_error(model, "id", "missing from the read")),
        };
        let mut record = LoadedRecord::new(id);
        if request.display_name() {
            record = record.with_display_name(match row.get(DISPLAY_NAME) {
                Some(Value::Str(name)) => &**name,
                // a record whose name is not set
                Some(Value::False) => "",
                Some(other) => return Err(mismatch(model, DISPLAY_NAME, "a string", other)),
                None => return Err(value_error(model, DISPLAY_NAME, "missing from the read")),
            });
        }
        for (name, kind) in &columns {
            let value = row
                .get(*name)
                .ok_or_else(|| value_error(model, name, "missing from the read"))?;
            record = record.with_field(*name, convert(model, name, kind, value)?);
        }
        loaded.push(record);
    }
    Ok(loaded)
}

/// Add to `labels` the targets of the many2one fields `labelled` (field, target model) of the rows
/// `raw` (ids) and `classic` (`[id, display_name]`, `False` for no target or a gone one), which are
/// the same records in the same order.
fn add_labels(
    model: &str,
    raw: &[BTreeMap<String, Value>],
    classic: &[BTreeMap<String, Value>],
    labelled: &[(&str, &str)],
    labels: &mut Labels,
) -> Result<(), DataError> {
    if raw.len() != classic.len()
        || raw
            .iter()
            .zip(classic)
            .any(|(a, b)| a.get("id") != b.get("id"))
    {
        return Err(value_error(
            model,
            "id",
            "the many2one names were read for other records than the fields",
        ));
    }
    for (row, named) in raw.iter().zip(classic) {
        for (field, target) in labelled {
            let id = match row.get(*field) {
                Some(Value::Int(id)) if *id != 0 => *id,
                _ => continue,
            };
            let value = named
                .get(*field)
                .ok_or_else(|| value_error(model, field, "missing from the read"))?;
            let label = match value {
                // the target no longer exists
                Value::False => None,
                Value::List(pair) => match &pair[..] {
                    [Value::Int(of), Value::Str(name)] if *of == id => Some(name.to_string()),
                    // a target whose name is not set
                    [Value::Int(of), Value::False] if *of == id => Some(String::new()),
                    _ => return Err(mismatch(model, field, "the target's id and name", value)),
                },
                other => return Err(mismatch(model, field, "the target's id and name", other)),
            };
            labels
                .entry((*target).to_owned())
                .or_default()
                .insert(id, label);
        }
    }
    Ok(())
}

/// Whether a read of `fields` (and the display name) of `spec` fetches a stored column, which is
/// what tells an existing record from a deleted one.
fn reads_a_column(spec: &ModelSpec, fields: &[&FieldSpec], display_name: bool) -> bool {
    fields.iter().any(|field| field.has_column())
        || (display_name
            && spec
                .rec_name
                .is_some_and(|name| spec.field_by_id(name).has_column()))
}

/// The kind of `field`, or `None` for a float whose digits are a named precision (resolved by
/// [`named_digits`]).
fn fixed_kind<'a>(model: &str, field: &'a FieldSpec) -> Result<Option<Kind<'a>>, DataError> {
    Ok(Some(match &field.ty {
        FieldType::Char { .. } => Kind::Char,
        FieldType::Text => Kind::Text,
        FieldType::Html => Kind::Html,
        FieldType::Id | FieldType::Integer => Kind::Integer,
        FieldType::Float { digits } => match digits {
            None | Some(Digits::Unrounded) => Kind::Float(None),
            Some(Digits::Fixed { scale, .. }) => {
                Kind::Float(Some(u8::try_from(*scale).map_err(|_| {
                    value_error(
                        model,
                        field.name,
                        format!("it declares {scale} decimal digits"),
                    )
                })?))
            }
            Some(Digits::Named(_)) => return Ok(None),
        },
        FieldType::Monetary { currency_field } => {
            Kind::Monetary(currency_field.as_deref().ok_or_else(|| {
                value_error(
                    model,
                    field.name,
                    "the monetary field has no currency field",
                )
            })?)
        }
        FieldType::Boolean => Kind::Boolean,
        FieldType::Binary { .. } => Kind::Binary,
        FieldType::Selection {
            values: Some(values),
            ..
        } => Kind::Selection(values),
        FieldType::Selection { values: None, .. } => {
            return Err(unsupported(
                model,
                field.name,
                "selection (its labels come from a method)",
            ));
        }
        FieldType::Date => Kind::Date,
        FieldType::Datetime => Kind::Datetime,
        FieldType::Many2one { comodel, .. } => Kind::One(comodel),
        FieldType::One2many { comodel, .. } | FieldType::Many2many { comodel, .. } => {
            Kind::Many(comodel)
        }
        other => return Err(unsupported(model, field.name, other.type_name())),
    }))
}

/// The decimal digits of the floats of `fields` that declare a named precision, by field name.
/// The ORM resolves them through `fields_get`; no statement when no field has one.
async fn named_digits(
    tx: &mut Transaction,
    records: &Records,
    fields: &[&FieldSpec],
) -> Result<BTreeMap<&'static str, u8>, DataError> {
    let names: Vec<&'static str> = fields
        .iter()
        .filter(|field| {
            matches!(
                field.ty,
                FieldType::Float {
                    digits: Some(Digits::Named(_))
                }
            )
        })
        .map(|field| field.name)
        .collect();
    let mut digits = BTreeMap::new();
    if names.is_empty() {
        return Ok(digits);
    }
    let model = records.model_name();
    let described = records
        .fields_get(tx, Some(names.as_slice()), Some(["digits"].as_slice()))
        .await?;
    for name in names {
        // `digits` is described as `[precision, scale]`
        let scale = match described.get(name).and_then(|field| field.get("digits")) {
            Some(Value::List(pair)) => match pair.get(1) {
                Some(Value::Int(scale)) => u8::try_from(*scale).ok(),
                _ => None,
            },
            _ => None,
        };
        let scale = scale.ok_or_else(|| {
            value_error(
                model,
                name,
                "its named decimal precision was not described as [precision, scale]",
            )
        })?;
        digits.insert(name, scale);
    }
    Ok(digits)
}

fn string<'a>(model: &str, field: &str, value: &'a Value) -> Result<&'a str, DataError> {
    match value {
        Value::Str(text) => Ok(text),
        Value::False => Ok(""),
        other => Err(mismatch(model, field, "a string", other)),
    }
}

fn number(model: &str, field: &str, value: &Value) -> Result<f64, DataError> {
    match value {
        Value::Float(number) => Ok(*number),
        Value::Decimal(number) => f64::try_from(*number)
            .map_err(|_| value_error(model, field, format!("{number} does not fit a float"))),
        other => Err(mismatch(model, field, "a number", other)),
    }
}

/// A calendar or clock part as the report value takes it.
fn part(model: &str, field: &str, value: u32) -> Result<u8, DataError> {
    u8::try_from(value)
        .map_err(|_| value_error(model, field, format!("{value} is not a date or time part")))
}

/// The report value of `value`, the ORM's raw read of a field of `kind`.
fn convert(
    model: &str,
    field: &str,
    kind: &Kind<'_>,
    value: &Value,
) -> Result<FieldValue, DataError> {
    Ok(match kind {
        Kind::Char => FieldValue::char(string(model, field, value)?),
        Kind::Text => FieldValue::text(string(model, field, value)?),
        Kind::Html => FieldValue::html(string(model, field, value)?),
        Kind::Integer => match value {
            Value::Int(number) => FieldValue::integer(*number),
            other => return Err(mismatch(model, field, "an integer", other)),
        },
        Kind::Float(digits) => FieldValue::float(number(model, field, value)?, *digits),
        Kind::Monetary(currency) => FieldValue::monetary(number(model, field, value)?, currency),
        Kind::Boolean => match value {
            Value::Bool(flag) => FieldValue::boolean(*flag),
            Value::False => FieldValue::boolean(false),
            other => return Err(mismatch(model, field, "a boolean", other)),
        },
        Kind::Binary => match value {
            Value::False => FieldValue::binary(&[]),
            Value::Bytes(bytes) => FieldValue::binary(bytes),
            other => return Err(mismatch(model, field, "bytes", other)),
        },
        Kind::Selection(values) => match value {
            Value::False => FieldValue::selection("", ""),
            Value::Str(key) => {
                let (_, label) = values
                    .iter()
                    .find(|(declared, _)| declared.as_str() == &**key)
                    .ok_or_else(|| {
                        value_error(
                            model,
                            field,
                            format!("the stored key '{key}' has no declared label"),
                        )
                    })?;
                FieldValue::selection(key, label)
            }
            other => return Err(mismatch(model, field, "a selection key", other)),
        },
        Kind::Date => match value {
            Value::False => FieldValue::no_date(),
            Value::Date(date) => {
                let (year, month, day) = date.ymd();
                FieldValue::date(year, part(model, field, month)?, part(model, field, day)?)?
            }
            other => return Err(mismatch(model, field, "a date", other)),
        },
        Kind::Datetime => match value {
            Value::False => FieldValue::no_datetime(),
            Value::DateTime(stamp) => {
                let (year, month, day) = stamp.date().ymd();
                // the report value keeps whole seconds
                let (hour, minute, second, _micro) = stamp.time();
                FieldValue::datetime(
                    (year, part(model, field, month)?, part(model, field, day)?),
                    (
                        part(model, field, hour)?,
                        part(model, field, minute)?,
                        part(model, field, second)?,
                    ),
                )?
            }
            other => return Err(mismatch(model, field, "a datetime", other)),
        },
        Kind::One(comodel) => match value {
            Value::False => FieldValue::many2one(comodel, None),
            Value::Int(id) if *id > 0 => FieldValue::many2one(comodel, Some(*id)),
            other => return Err(mismatch(model, field, "a record id", other)),
        },
        Kind::Many(comodel) => match value {
            Value::List(items) => {
                let ids = items
                    .iter()
                    .map(|item| match item {
                        Value::Int(id) => Ok(*id),
                        other => Err(mismatch(model, field, "a list of record ids", other)),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                FieldValue::many(comodel, &ids)
            }
            other => return Err(mismatch(model, field, "a list of record ids", other)),
        },
    })
}
