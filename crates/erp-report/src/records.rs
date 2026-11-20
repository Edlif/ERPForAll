// SPDX-License-Identifier: MIT OR Apache-2.0
//! The records an evaluation prints, already read. The data stage (`crate::DataStage`) fills a
//! [`RecordStore`] under the caller's rights; evaluation reads only from it, so a template cannot
//! reach a record or a field that was not loaded.
use crate::{ReportError, error::invalid};
use erp_expr::{Budget, ExprError, ExprObject, ExprResult, ExprValue, Meta};
use std::{borrow::Cow, collections::BTreeMap, fmt, sync::Arc};

/// Where a currency's symbol stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolPosition {
    Before,
    After,
}
#[derive(Debug, Clone)]
pub(crate) enum Kind {
    Char,
    Text,
    Html,
    Integer,
    /// The field's declared decimal digits, when it has any.
    Float(Option<u8>),
    /// The name of the field, on the same record, that holds the currency.
    Monetary(String),
    Date,
    Datetime,
    Boolean,
    /// The label of the stored key, already translated.
    Selection(String),
    One(String, Option<i64>),
    Many(String, Vec<i64>),
    /// The stored bytes of a binary field (an image, a file).
    Binary(Arc<[u8]>),
}
/// One field value with what a `t-field` widget needs to format it.
#[derive(Debug, Clone)]
pub struct FieldValue {
    pub(crate) kind: Kind,
    /// What an expression sees; `False` when the field is unset, as in the reference.
    pub(crate) value: ExprValue,
}
impl FieldValue {
    fn of(kind: Kind, value: ExprValue) -> Self {
        Self { kind, value }
    }
    fn string(kind: Kind, value: &str) -> Self {
        let value = match value {
            "" => ExprValue::Bool(false),
            value => ExprValue::str(value),
        };
        Self::of(kind, value)
    }
    /// A single-line text field; the empty string is the unset value.
    pub fn char(value: &str) -> Self {
        Self::string(Kind::Char, value)
    }
    /// A multi-line text field.
    pub fn text(value: &str) -> Self {
        Self::string(Kind::Text, value)
    }
    /// Rich text, converted through the bounded user-content mapping at evaluation.
    pub fn html(value: &str) -> Self {
        Self::string(Kind::Html, value)
    }
    pub fn integer(value: i64) -> Self {
        Self::of(Kind::Integer, ExprValue::Int(value))
    }
    /// A float with the field's declared decimal digits, if it declares any.
    pub fn float(value: f64, digits: Option<u8>) -> Self {
        Self::of(Kind::Float(digits), ExprValue::Float(value))
    }
    /// An amount whose currency is the record's `currency_field` (a many2one in the same store).
    pub fn monetary(value: f64, currency_field: &str) -> Self {
        Self::of(
            Kind::Monetary(currency_field.into()),
            ExprValue::Float(value),
        )
    }
    pub fn boolean(value: bool) -> Self {
        Self::of(Kind::Boolean, ExprValue::Bool(value))
    }
    /// A selection: the stored key and its label in the document's language.
    pub fn selection(key: &str, label: &str) -> Self {
        Self::string(Kind::Selection(label.into()), key)
    }
    pub fn date(year: i32, month: u8, day: u8) -> Result<Self, ReportError> {
        Ok(Self::of(
            Kind::Date,
            ExprValue::Date(date(year, month, day)?),
        ))
    }
    pub fn no_date() -> Self {
        Self::of(Kind::Date, ExprValue::Bool(false))
    }
    /// A timestamp in UTC, as stored.
    pub fn datetime(
        (year, month, day): (i32, u8, u8),
        (hour, minute, second): (u8, u8, u8),
    ) -> Result<Self, ReportError> {
        let time = time::Time::from_hms(hour, minute, second)
            .map_err(|_| invalid("field time is not a time of day"))?;
        let value = time::PrimitiveDateTime::new(date(year, month, day)?, time);
        Ok(Self::of(Kind::Datetime, ExprValue::DateTime(value)))
    }
    pub fn no_datetime() -> Self {
        Self::of(Kind::Datetime, ExprValue::Bool(false))
    }
    /// A many2one: the target model and the record it points to, if any.
    pub fn many2one(model: &str, id: Option<i64>) -> Self {
        Self::of(Kind::One(model.into(), id), ExprValue::None)
    }
    /// A one2many or many2many, in the order the records print.
    pub fn many(model: &str, ids: &[i64]) -> Self {
        Self::of(Kind::Many(model.into(), ids.to_vec()), ExprValue::None)
    }
    /// A binary field: the stored bytes, not their base64 text. No bytes is the unset value. The
    /// data stage checks an image against the image limits before it is kept.
    ///
    /// An expression never sees the bytes: in the store the value is an image value (true in a
    /// test, printed by `<img t-att-src="image_data_uri(record.field)"/>` and by the `image`
    /// widget), so an image is shared with the document and is not bound by the size limit of an
    /// expression value.
    pub fn binary(bytes: &[u8]) -> Self {
        // The store replaces the placeholder of set bytes with the image value when it takes them.
        Self::of(Kind::Binary(bytes.into()), ExprValue::Bool(false))
    }
    /// The target model and ids of a relational value, `None` for any other kind.
    pub(crate) fn relation(&self) -> Option<(&str, &[i64])> {
        match &self.kind {
            Kind::One(model, id) => Some((model, id.as_slice())),
            Kind::Many(model, ids) => Some((model, ids)),
            _ => None,
        }
    }
    /// The bytes of a binary value.
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        match &self.kind {
            Kind::Binary(bytes) => Some(bytes),
            _ => None,
        }
    }
}
fn date(year: i32, month: u8, day: u8) -> Result<time::Date, ReportError> {
    time::Month::try_from(month)
        .ok()
        .and_then(|month| time::Date::from_calendar_date(year, month, day).ok())
        .ok_or_else(|| invalid("field date is not a Gregorian date"))
}
#[derive(Debug)]
pub(crate) struct Record {
    /// `None` when the data stage did not read it: the plan saw no use of it.
    pub(crate) display_name: Option<String>,
    pub(crate) fields: BTreeMap<String, FieldValue>,
}
/// Every record a document prints, keyed by model and id.
#[derive(Debug, Default)]
pub struct RecordStore {
    pub(crate) precisions: BTreeMap<String, u8>,
    records: BTreeMap<(String, i64), Record>,
    pub(crate) html_images: crate::html_images::HtmlImages,
    pub(crate) helpers: Vec<HelperResults>,
    /// The bytes of every set binary field, in the order the records were added.
    images: Vec<Arc<[u8]>>,
    html: Vec<(Arc<dyn ExprObject>, Arc<str>, crate::HtmlSource)>,
}
#[derive(Debug)]
pub(crate) struct HelperResults {
    pub(crate) invocation: crate::helper::Invocation,
    pub(crate) values: BTreeMap<i64, ExprValue>,
}
#[derive(Debug)]
struct BoundHelper {
    store: Arc<RecordStore>,
    model: String,
    method: String,
    ids: Vec<i64>,
}
impl ExprObject for BoundHelper {
    fn type_name(&self) -> &str {
        "report.helper"
    }
    fn repr(&self) -> String {
        format!("<report helper {}.{}>", self.model, self.method)
    }
    fn call(
        &self,
        args: &[ExprValue],
        kwargs: &erp_expr::Kwargs,
        budget: &mut Budget,
    ) -> ExprResult<ExprValue> {
        let [id] = self.ids.as_slice() else {
            return Err(ExprError::value_error(format!(
                "Expected singleton for helper {}.{}",
                self.model, self.method
            )));
        };
        let invalid = || {
            ExprError::value_error(format!(
                "helper {}.{} has no cached result for these arguments",
                self.model, self.method
            ))
        };
        let mut argument_bytes = 0usize;
        for value in args.iter().chain(kwargs.values()) {
            budget.check_value(value)?;
            argument_bytes = argument_bytes.saturating_add(value.deep_size());
        }
        for name in kwargs.keys() {
            argument_bytes = argument_bytes.saturating_add(name.len());
        }
        budget.charge(argument_bytes)?;
        let args = args
            .iter()
            .map(crate::helper::json)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(invalid)?;
        let kwargs = kwargs
            .iter()
            .map(|(k, v)| Some((k.clone(), crate::helper::json(v)?)))
            .collect::<Option<BTreeMap<_, _>>>()
            .ok_or_else(invalid)?;
        for result in &self.store.helpers {
            budget.steps(1)?;
            let invocation = &result.invocation;
            if invocation.model != self.model || invocation.method != self.method {
                continue;
            }
            budget.charge(argument_bytes)?;
            let (mut args, mut kwargs) = (args.clone(), kwargs.clone());
            if let Some(parameters) = &invocation.parameters {
                crate::helper::normalize(&mut args, &mut kwargs, parameters).ok_or_else(invalid)?;
            }
            if invocation.args == args && invocation.kwargs == kwargs {
                let value = result.values.get(id).ok_or_else(invalid)?;
                budget.check_value(value)?;
                if let ExprValue::Str(text) = value {
                    budget.charge(text.len())?;
                }
                return Ok(value.clone());
            }
        }
        Err(invalid())
    }
}
/// The type name of the value a set binary field has in an expression.
pub(crate) const IMAGE: &str = "report.image";
/// What an expression sees of a set binary field: its place in the store's images. It is true,
/// has no attribute and prints as its description; only an `<img>` source and the `image`
/// widget read the bytes.
#[derive(Debug)]
struct ImageValue {
    index: usize,
    bytes: usize,
}
impl ExprObject for ImageValue {
    fn type_name(&self) -> &str {
        IMAGE
    }
    fn repr(&self) -> String {
        format!("<image of {} bytes>", self.bytes)
    }
    fn as_int(&self) -> ExprResult<i64> {
        Ok(i64::try_from(self.index).unwrap_or(-1))
    }
}
/// A store-issued opaque markup handle. Its text is not an expression capability.
#[derive(Debug)]
struct HtmlValue {
    index: usize,
    text: Arc<str>,
}
impl ExprObject for HtmlValue {
    fn type_name(&self) -> &str {
        "report.html"
    }
    fn repr(&self) -> String {
        self.text.to_string()
    }
    fn as_int(&self) -> ExprResult<i64> {
        i64::try_from(self.index).map_err(|_| ExprError::value_error("HTML handle overflow"))
    }
}
impl RecordStore {
    pub(crate) fn html(&self, value: &ExprValue) -> Option<(Arc<str>, crate::HtmlSource)> {
        let ExprValue::Object(object) = value else {
            return None;
        };
        if object.type_name() != "report.html" {
            return None;
        }
        let index = usize::try_from(object.as_int().ok()?).ok()?;
        let (issued, text, source) = self.html.get(index)?;
        Arc::ptr_eq(object, issued).then(|| (text.clone(), source.clone()))
    }
    pub fn new() -> Self {
        Self::default()
    }
    /// Add one record. A second record of the same model and id is refused.
    pub fn insert(
        &mut self,
        model: &str,
        id: i64,
        display_name: &str,
        fields: Vec<(&str, FieldValue)>,
    ) -> Result<(), ReportError> {
        let fields = fields.into_iter().map(|(k, v)| (k.into(), v)).collect();
        self.add(model, id, Some(display_name.into()), fields)
    }
    /// Add one record as the data stage read it.
    pub(crate) fn add(
        &mut self,
        model: &str,
        id: i64,
        display_name: Option<String>,
        fields: BTreeMap<String, FieldValue>,
    ) -> Result<(), ReportError> {
        if model.is_empty() {
            return Err(invalid("record model is empty"));
        }
        if self.records.contains_key(&(model.to_owned(), id)) {
            return Err(invalid("record was added to the store twice"));
        }
        let mut fields = fields;
        for (name, field) in &mut fields {
            if matches!(field.kind, Kind::Html)
                && let ExprValue::Str(text) = &field.value
            {
                let text: Arc<str> = text.as_str().into();
                let object: Arc<dyn ExprObject> = Arc::new(HtmlValue {
                    index: self.html.len(),
                    text: text.clone(),
                });
                let source = crate::HtmlSource {
                    record: display_name
                        .as_ref()
                        .map(|label| (model.into(), id, label.clone(), name.clone())),
                };
                self.html.push((object.clone(), text, source));
                field.value = ExprValue::Object(object);
            }
            if let Kind::Binary(bytes) = &field.kind
                && !bytes.is_empty()
            {
                field.value = ExprValue::Object(Arc::new(ImageValue {
                    index: self.images.len(),
                    bytes: bytes.len(),
                }));
                self.images.push(bytes.clone());
            }
        }
        let record = Record {
            display_name,
            fields,
        };
        self.records.insert((model.into(), id), record);
        Ok(())
    }
    /// How many records the store holds.
    pub fn len(&self) -> usize {
        self.records.len()
    }
    /// Whether the store holds no record.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    /// The recordset value a template context binds (`docs`, `doc`, `company`).
    pub fn recordset(self: &Arc<Self>, model: &str, ids: &[i64]) -> ExprValue {
        set(self, model, ids.to_vec())
    }
    pub(crate) fn get(&self, model: &str, id: i64) -> Option<&Record> {
        self.records.get(&(model.to_owned(), id))
    }
    /// The bytes of an image value of this store; `None` for any other value.
    pub(crate) fn image(&self, value: &ExprValue) -> Option<Arc<[u8]>> {
        let ExprValue::Object(object) = value else {
            return None;
        };
        if object.type_name() != IMAGE {
            return None;
        }
        let index = usize::try_from(object.as_int().ok()?).ok()?;
        self.images.get(index).cloned()
    }
    /// The kind of a field, read from any loaded record of the model.
    fn kind(&self, model: &str, field: &str) -> Option<&Kind> {
        self.records
            .range((model.to_owned(), i64::MIN)..)
            .take_while(|((m, _), _)| m == model)
            .find_map(|(_, record)| record.fields.get(field))
            .map(|value| &value.kind)
    }
}
fn set(store: &Arc<RecordStore>, model: &str, ids: Vec<i64>) -> ExprValue {
    ExprValue::Object(Arc::new(Recordset {
        store: store.clone(),
        model: model.into(),
        ids,
    }))
}
/// What expressions see of the store: a recordset with field access, and nothing else.
struct Recordset {
    store: Arc<RecordStore>,
    model: String,
    ids: Vec<i64>,
}
impl fmt::Debug for Recordset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{:?}", self.model, self.ids)
    }
}
impl Recordset {
    fn singleton(&self) -> ExprError {
        ExprError::value_error(format!("Expected singleton: {}", self.repr()))
    }
    fn field(&self, id: i64, name: &str) -> ExprResult<&FieldValue> {
        let record = self.store.get(&self.model, id).ok_or_else(|| {
            ExprError::value_error(format!(
                "record {}({id}) was not loaded for this report",
                self.model
            ))
        })?;
        record
            .fields
            .get(name)
            .ok_or_else(|| ExprError::attribute_error(&self.model, name))
    }
}
impl ExprObject for Recordset {
    fn type_name(&self) -> &str {
        &self.model
    }
    fn repr(&self) -> String {
        let ids: Vec<String> = self.ids.iter().map(i64::to_string).collect();
        format!("{}({})", self.model, ids.join(", "))
    }
    fn is_truthy(&self) -> bool {
        !self.ids.is_empty()
    }
    fn is_record(&self) -> bool {
        true
    }
    fn record_ids(&self) -> Option<Cow<'_, [i64]>> {
        Some(Cow::Borrowed(&self.ids))
    }
    fn py_eq(&self, other: &dyn ExprObject) -> bool {
        let mut mine = self.ids.clone();
        mine.sort_unstable();
        other.type_name() == self.model
            && other.record_ids().is_some_and(|ids| {
                let mut theirs = ids.into_owned();
                theirs.sort_unstable();
                theirs == mine
            })
    }
    fn meta(&self) -> Meta {
        Meta::scalar(self.ids.len() * std::mem::size_of::<i64>())
    }
    fn get_item(&self, key: &ExprValue, _budget: &mut Budget) -> ExprResult<ExprValue> {
        let ExprValue::Int(index) = key else {
            return Err(ExprError::type_error(format!(
                "recordset indices must be integers, not '{}'",
                key.type_name()
            )));
        };
        let len = i64::try_from(self.ids.len()).unwrap_or(i64::MAX);
        let index = if *index < 0 { index + len } else { *index };
        usize::try_from(index)
            .ok()
            .and_then(|i| self.ids.get(i))
            .map(|id| set(&self.store, &self.model, vec![*id]))
            .ok_or_else(|| ExprError::index_error("recordset index out of range"))
    }
    fn get_attr(&self, name: &str, _budget: &mut Budget) -> ExprResult<ExprValue> {
        if self
            .store
            .helpers
            .iter()
            .any(|r| r.invocation.model == self.model && r.invocation.method == name)
        {
            return Ok(ExprValue::Object(Arc::new(BoundHelper {
                store: self.store.clone(),
                model: self.model.clone(),
                method: name.into(),
                ids: self.ids.clone(),
            })));
        }
        if name == "ids" {
            return Ok(ExprValue::list(
                self.ids.iter().map(|id| ExprValue::Int(*id)).collect(),
            ));
        }
        match self.ids.as_slice() {
            // An empty recordset reads as the empty value of the field's kind.
            [] => Ok(match self.store.kind(&self.model, name) {
                Some(Kind::One(model, _) | Kind::Many(model, _)) => {
                    set(&self.store, model, Vec::new())
                }
                _ => ExprValue::Bool(false),
            }),
            [id] => match name {
                "id" => Ok(ExprValue::Int(*id)),
                "display_name" => {
                    let record = self.store.get(&self.model, *id).ok_or_else(|| {
                        ExprError::value_error(format!(
                            "record {}({id}) was not loaded for this report",
                            self.model
                        ))
                    })?;
                    record
                        .display_name
                        .as_deref()
                        .map(ExprValue::str)
                        .ok_or_else(|| {
                            ExprError::value_error(format!(
                                "display_name of {}({id}) was not loaded for this report",
                                self.model
                            ))
                        })
                }
                _ => {
                    let field = self.field(*id, name)?;
                    Ok(match &field.kind {
                        Kind::One(model, id) => {
                            set(&self.store, model, id.iter().copied().collect())
                        }
                        Kind::Many(model, ids) => set(&self.store, model, ids.clone()),
                        _ => field.value.clone(),
                    })
                }
            },
            ids => {
                // Relations of several records are their union, in first-seen order.
                let mut union: Vec<i64> = Vec::new();
                let mut seen = std::collections::BTreeSet::new();
                let mut target = None;
                for id in ids {
                    let (model, more): (&String, &[i64]) = match &self.field(*id, name)?.kind {
                        Kind::One(model, one) => (model, one.as_slice()),
                        Kind::Many(model, many) => (model, many),
                        _ => return Err(self.singleton()),
                    };
                    target = Some(model.clone());
                    union.extend(more.iter().copied().filter(|id| seen.insert(*id)));
                }
                Ok(set(&self.store, &target.unwrap_or_default(), union))
            }
        }
    }
}
