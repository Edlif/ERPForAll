// SPDX-License-Identifier: MIT OR Apache-2.0
//! `t-field` widgets and `t-out` with a `widget` option: a value becomes formatted text in the
//! document's language (design §5.3). Widgets that need a document
//! node or data this slice does not have are refused by name.
use crate::{
    ReportError,
    format::{self, LangFormat},
    program::{At, FORMATS_TASK},
    records::{FieldValue, Kind, RecordStore, SymbolPosition},
};
use erp_expr::{ExprValue, Map};

const NBSP: char = '\u{a0}';
/// Follows a minus sign so that a line never breaks between it and the digits.
const MINUS: &str = "-\u{feff}";

/// What a widget prints.
#[derive(Debug, PartialEq)]
pub(crate) enum Piece {
    Text(String),
    Break,
}
pub(crate) struct Env<'a> {
    pub(crate) formats: &'a LangFormat,
    pub(crate) utc_offset: i32,
    pub(crate) records: &'a RecordStore,
}
struct Currency {
    symbol: String,
    position: SymbolPosition,
    decimals: usize,
}
type Printed = Result<Option<Vec<Piece>>, ReportError>;

fn text(value: impl Into<String>) -> Option<Vec<Piece>> {
    Some(vec![Piece::Text(value.into())])
}
/// Line feeds become line breaks, as the reference does for text and display names.
fn lines(value: &str) -> Option<Vec<Piece>> {
    let mut out = Vec::new();
    for (i, line) in value.split('\n').enumerate() {
        if i > 0 {
            out.push(Piece::Break);
        }
        if !line.is_empty() {
            out.push(Piece::Text(line.into()));
        }
    }
    Some(out)
}
fn option<'a>(options: Option<&'a Map>, name: &str) -> Option<&'a ExprValue> {
    options
        .and_then(|options| options.get_str(name))
        .filter(|value| !matches!(value, ExprValue::None))
}
fn flag(options: Option<&Map>, name: &str) -> bool {
    option(options, name).is_some_and(ExprValue::is_truthy)
}
fn digits(options: Option<&Map>, name: &str, at: &At) -> Result<Option<usize>, ReportError> {
    match option(options, name) {
        None => Ok(None),
        Some(ExprValue::Int(n)) if (0..=14).contains(n) => Ok(usize::try_from(*n).ok()),
        Some(_) => Err(at.evaluate(format!(
            "widget option `{name}` must be an integer in 0..=14"
        ))),
    }
}
/// The single record a value points to, if it is a recordset of one.
fn single(value: &ExprValue) -> Option<(&str, i64)> {
    let ExprValue::Object(object) = value else {
        return None;
    };
    match object.record_ids()?.as_ref() {
        [id] if object.is_record() => Some((object.type_name(), *id)),
        _ => None,
    }
}
fn currency(env: &Env<'_>, value: &ExprValue, at: &At) -> Result<Currency, ReportError> {
    let (model, id) = single(value)
        .ok_or_else(|| at.evaluate("the monetary widget needs one currency record"))?;
    currency_record(env, model, id, at)
}
fn number(value: &ExprValue, at: &At) -> Result<f64, ReportError> {
    match value {
        ExprValue::Bool(_) => None,
        other => other.as_f64(),
    }
    .ok_or_else(|| {
        at.evaluate(format!(
            "a numeric widget cannot format a value of type {}",
            value.type_name()
        ))
    })
}
fn amount(env: &Env<'_>, value: f64, currency: &Currency) -> Result<String, ReportError> {
    let (negative, integer, decimals) = format::split(value, currency.decimals)?;
    let formatted = env
        .formats
        .number(negative, &integer, &decimals)
        .replace(' ', &NBSP.to_string())
        .replace('-', MINUS);
    Ok(match currency.position {
        SymbolPosition::Before => format!("{}{NBSP}{formatted}", currency.symbol),
        SymbolPosition::After => format!("{formatted}{NBSP}{}", currency.symbol),
    })
}
/// The float widget's rule: at most six decimals when nothing sets a precision, never more
/// decimals than a double carries, and trailing zeros dropped down to the minimum.
fn float(
    env: &Env<'_>,
    value: f64,
    precision: Option<usize>,
    minimum: Option<usize>,
) -> Result<String, ReportError> {
    let (precision, minimum) = match precision {
        Some(precision) => (precision, minimum),
        None => (6, minimum.or(Some(1))),
    };
    let integer_digits = if value == 0.0 || !value.is_finite() {
        1
    } else {
        // Truncation toward zero, as the reference counts them: every |value| < 10 has one digit.
        let magnitude = value.abs().log10().trunc();
        (0u8..14)
            .find(|n| f64::from(*n) >= magnitude)
            .map_or(15, |n| usize::from(n) + 1)
    };
    let precision = precision.min(14usize.saturating_sub(integer_digits));
    let (negative, integer, mut decimals) = format::split(value, precision)?;
    if let Some(minimum) = minimum.filter(|minimum| *minimum < precision) {
        let used = decimals.trim_end_matches('0').len();
        decimals.truncate(used.max(minimum));
    }
    Ok(env
        .formats
        .number(negative, &integer, &decimals)
        .replace('-', MINUS))
}
fn display_names(env: &Env<'_>, model: &str, ids: &[i64], at: &At) -> Printed {
    let mut names = Vec::new();
    for id in ids {
        let record = env.records.get(model, *id).ok_or_else(|| {
            at.evaluate(format!(
                "record {model}({id}) was not loaded for this report"
            ))
        })?;
        names.push(record.display_name.as_deref().ok_or_else(|| {
            at.evaluate(format!(
                "display_name of {model}({id}) was not loaded for this report"
            ))
        })?);
    }
    let joined = names.join(", ");
    Ok(if joined.is_empty() {
        None
    } else {
        lines(&joined)
    })
}
fn date_parts(
    value: &ExprValue,
    env: &Env<'_>,
    shift: bool,
    at: &At,
) -> Result<Stamp, ReportError> {
    match value {
        ExprValue::Date(date) => Ok(Stamp {
            date: (date.year(), u8::from(date.month()), date.day()),
            time: (0, 0, 0),
            ordinal: date.ordinal(),
        }),
        ExprValue::DateTime(stamp) => {
            let stamp = if shift {
                stamp
                    .checked_add(time::Duration::seconds(i64::from(env.utc_offset)))
                    .ok_or_else(|| {
                        at.evaluate("a timestamp leaves the calendar in the user's timezone")
                    })?
            } else {
                *stamp
            };
            Ok(Stamp {
                date: (stamp.year(), u8::from(stamp.month()), stamp.day()),
                time: (stamp.hour(), stamp.minute(), stamp.second()),
                ordinal: stamp.ordinal(),
            })
        }
        other => Err(at.evaluate(format!(
            "a date widget cannot format a value of type {}",
            other.type_name()
        ))),
    }
}
struct Stamp {
    date: (i32, u8, u8),
    time: (u8, u8, u8),
    ordinal: u16,
}
/// Format a raw value with a named widget.
fn raw(
    env: &Env<'_>,
    value: &ExprValue,
    widget: &str,
    options: Option<&Map>,
    field_digits: Option<usize>,
    at: &At,
) -> Printed {
    let unsupported = |feature, task| Err(ReportError::Unsupported { feature, task });
    if option(options, "format").is_some() {
        return unsupported("the format option of date widgets", FORMATS_TASK);
    }
    if option(options, "decimal_precision").is_some() {
        return unsupported("the decimal_precision widget option", "a later release");
    }
    Ok(match widget {
        "char" => text(value.to_py_str()),
        "text" => lines(&value.to_py_str()),
        "integer" => {
            let value = match value {
                ExprValue::Int(n) => *n,
                other => {
                    let n = number(other, at)?.trunc();
                    if n.abs() >= 9.0e18 {
                        return Err(at.evaluate("an integer to format is too large"));
                    }
                    format!("{n:.0}").parse::<i64>().unwrap_or(0)
                }
            };
            text(env.formats.integer(value).replace('-', MINUS))
        }
        "float" => {
            let precision = digits(options, "precision", at)?.or(field_digits);
            let minimum = digits(options, "min_precision", at)?;
            text(float(env, number(value, at)?, precision, minimum)?)
        }
        "monetary" => {
            let holder = option(options, "display_currency").ok_or_else(|| {
                at.evaluate("the monetary widget needs the display_currency option")
            })?;
            let currency = currency(env, holder, at)?;
            text(amount(env, number(value, at)?, &currency)?)
        }
        "date" => {
            let stamp = date_parts(value, env, false, at)?;
            text(format::datetime(
                env.formats.date_format(),
                stamp.date,
                stamp.time,
                stamp.ordinal,
            ))
        }
        "datetime" => {
            let stamp = date_parts(value, env, true, at)?;
            let time = if flag(options, "hide_seconds") {
                env.formats.time_format().replace(":%S", "")
            } else {
                env.formats.time_format().to_owned()
            };
            let pattern = match (flag(options, "date_only"), flag(options, "time_only")) {
                (true, _) => env.formats.date_format().to_owned(),
                (false, true) => time,
                (false, false) => format!("{} {time}", env.formats.date_format()),
            };
            text(format::datetime(
                &pattern,
                stamp.date,
                stamp.time,
                stamp.ordinal,
            ))
        }
        "selection" => {
            let key = value.to_py_str();
            let label = match option(options, "selection") {
                Some(ExprValue::Dict(labels)) => labels
                    .get_str(&key)
                    .filter(|label| label.is_truthy())
                    .map(ExprValue::to_py_str),
                Some(_) => return Err(at.evaluate("the selection option must be a mapping")),
                None => None,
            };
            text(label.unwrap_or(key))
        }
        "many2one" | "many2many" | "one2many" => {
            let ExprValue::Object(object) = value else {
                return Err(at.evaluate("a relational widget needs a recordset"));
            };
            let ids = object
                .record_ids()
                .ok_or_else(|| at.evaluate("a relational widget needs a recordset"))?;
            return display_names(env, object.type_name(), &ids, at);
        }
        "html" => return unsupported("html field content", "a later release"),
        "image" | "image_url" | "barcode" => {
            return unsupported("image and barcode widgets", "a later release");
        }
        "contact" | "duration" | "relative" | "float_time" => {
            return unsupported("contact, duration and relative widgets", FORMATS_TASK);
        }
        other => return Err(at.evaluate(format!("unknown widget `{other}`"))),
    })
}
fn widget_option<'a>(options: Option<&'a Map>, at: &At) -> Result<Option<&'a str>, ReportError> {
    match option(options, "widget") {
        None => Ok(None),
        Some(ExprValue::Str(name)) => Ok(Some(name)),
        Some(_) => Err(at.evaluate("the widget option must be a string")),
    }
}
/// `t-out` with options: the value goes through the named widget; without one it prints as text.
pub(crate) fn value(env: &Env<'_>, value: &ExprValue, options: Option<&Map>, at: &At) -> Printed {
    match widget_option(options, at)? {
        Some(widget) => raw(env, value, widget, options, None, at),
        None => Ok(text(value.to_py_str())),
    }
}
/// `t-field`: the widget of the field's kind unless the options name another. An unset field
/// prints nothing, as in the reference.
pub(crate) fn field(
    env: &Env<'_>,
    record: (&str, i64),
    field: &FieldValue,
    options: Option<&Map>,
    at: &At,
) -> Printed {
    let named = widget_option(options, at)?;
    let unset = matches!(field.value, ExprValue::Bool(false));
    match &field.kind {
        Kind::One(model, id) if matches!(named, None | Some("many2one")) => {
            return display_names(env, model, id.as_slice(), at);
        }
        Kind::Many(model, ids) if matches!(named, None | Some("many2many") | Some("one2many")) => {
            return display_names(env, model, ids, at);
        }
        Kind::One(..) | Kind::Many(..) if named == Some("contact") => {
            return Err(ReportError::Unsupported {
                feature: "contact, duration and relative widgets",
                task: FORMATS_TASK,
            });
        }
        Kind::One(..) | Kind::Many(..) => {
            return Err(at.evaluate(format!(
                "widget `{}` cannot print a relational field",
                named.unwrap_or_default()
            )));
        }
        _ if unset => return Ok(None),
        Kind::Selection(label) if matches!(named, None | Some("selection")) => {
            if option(options, "selection").is_none() {
                return Ok(text(label.as_str()));
            }
        }
        Kind::Monetary(currency_field)
            if matches!(named, None | Some("monetary"))
                && option(options, "display_currency").is_none() =>
        {
            let (model, id) = record;
            let holder = env
                .records
                .get(model, id)
                .and_then(|record| record.fields.get(currency_field));
            let Some(Kind::One(currency_model, Some(currency_id))) = holder.map(|f| &f.kind) else {
                return Err(at.evaluate(format!(
                    "monetary field of {model}({id}) has no currency in `{currency_field}`"
                )));
            };
            let currency = currency_record(env, currency_model, *currency_id, at)?;
            return Ok(text(amount(env, number(&field.value, at)?, &currency)?));
        }
        _ => {} // Every other kind prints through the widget of its name.
    }
    let (default, field_digits) = match &field.kind {
        Kind::Char | Kind::Boolean => ("char", None),
        Kind::Text => ("text", None),
        Kind::Html => ("html", None),
        Kind::Integer => ("integer", None),
        Kind::Float(digits) => ("float", digits.map(usize::from)),
        Kind::Monetary(_) => ("monetary", None),
        Kind::Date => ("date", None),
        Kind::Datetime => ("datetime", None),
        Kind::Selection(_) => ("selection", None),
        Kind::One(..) => ("many2one", None),
        Kind::Many(..) => ("many2many", None),
        Kind::Binary(_) => ("image", None),
    };
    raw(
        env,
        &field.value,
        named.unwrap_or(default),
        options,
        field_digits,
        at,
    )
}
fn currency_record(env: &Env<'_>, model: &str, id: i64, at: &At) -> Result<Currency, ReportError> {
    let record = env.records.get(model, id).ok_or_else(|| {
        at.evaluate(format!(
            "record {model}({id}) was not loaded for this report"
        ))
    })?;
    let field = |name: &str| record.fields.get(name).map(|field| &field.value);
    match (field("symbol"), field("position"), field("decimal_places")) {
        (Some(ExprValue::Str(symbol)), Some(ExprValue::Str(position)), Some(ExprValue::Int(n)))
            if (0..=14).contains(n) && matches!(position.as_str(), "before" | "after") =>
        {
            Ok(Currency {
                symbol: symbol.clone(),
                position: if position == "before" {
                    SymbolPosition::Before
                } else {
                    SymbolPosition::After
                },
                decimals: usize::try_from(*n).unwrap_or(0),
            })
        }
        _ => Err(at.evaluate(format!(
            "currency {model}({id}) needs symbol, position (before or after) and decimal_places in 0..=14"
        ))),
    }
}
