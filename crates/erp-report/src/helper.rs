// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure helper invocation identities, permission snapshots and cached scalar values.
use crate::ReportError;
use erp_expr::{Expr, ExprValue, Literal, UnaryOp};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub(crate) struct Invocation {
    pub(crate) model: String,
    pub(crate) method: String,
    pub(crate) parameters: Option<Vec<String>>,
    pub(crate) args: Vec<Value>,
    pub(crate) kwargs: BTreeMap<String, Value>,
    pub(crate) paths: BTreeSet<usize>,
    pub(crate) dependencies: Vec<String>,
}
impl Invocation {
    pub(crate) fn same_call(&self, other: &Self) -> bool {
        self.model == other.model
            && self.method == other.method
            && self.args == other.args
            && self.kwargs == other.kwargs
    }
    pub(crate) fn byte_len(&self) -> usize {
        // Saturation guarantees the caller's bounded work/byte charge refuses overflow.
        [self.model.len(), self.method.len()]
            .into_iter()
            .chain(self.args.iter().map(scalar_bytes))
            .chain(
                self.kwargs
                    .iter()
                    .map(|(k, v)| k.len().saturating_add(scalar_bytes(v))),
            )
            .chain(self.dependencies.iter().map(String::len))
            .fold(0, usize::saturating_add)
    }
}
/// An exact field permission set for these records, scoped to one helper invocation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HelperRead {
    pub(crate) model: String,
    pub(crate) ids: Vec<i64>,
    pub(crate) fields: Vec<String>,
}
impl HelperRead {
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn ids(&self) -> &[i64] {
        &self.ids
    }
    pub fn fields(&self) -> &[String] {
        &self.fields
    }
}
/// An owned, authorized batch to execute after all ordinary reads have completed.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HelperRequest {
    pub(crate) invocation: Invocation,
    pub(crate) ids: Vec<i64>,
    pub(crate) reads: Vec<HelperRead>,
}
impl HelperRequest {
    pub fn model(&self) -> &str {
        &self.invocation.model
    }
    pub fn method(&self) -> &str {
        &self.invocation.method
    }
    pub fn ids(&self) -> &[i64] {
        &self.ids
    }
    pub fn args(&self) -> &[Value] {
        &self.invocation.args
    }
    pub fn kwargs(&self) -> &BTreeMap<String, Value> {
        &self.invocation.kwargs
    }
    pub fn dependencies(&self) -> &[String] {
        &self.invocation.dependencies
    }
    pub fn reads(&self) -> &[HelperRead] {
        &self.reads
    }
}
pub(crate) fn scalar_bytes(value: &Value) -> usize {
    let payload = match value {
        Value::String(s) => s.len(),
        _ => 0,
    };
    // The expression engine's logical slot size applies even when the payload is empty.
    ExprValue::None.deep_size().saturating_add(payload)
}
pub(crate) fn scalar(value: Value) -> Result<ExprValue, ReportError> {
    Ok(match value {
        Value::Null => ExprValue::None,
        Value::Bool(v) => ExprValue::Bool(v),
        Value::String(v) => ExprValue::Str(v),
        Value::Number(v) if v.is_i64() => {
            ExprValue::Int(v.as_i64().ok_or_else(|| bad("invalid integer"))?)
        }
        Value::Number(v) if v.is_f64() => ExprValue::Float(
            v.as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| bad("non-finite number"))?,
        ),
        _ => {
            return Err(bad(
                "helper results must be null, boolean, signed integer, finite float or string",
            ));
        }
    })
}
pub(crate) fn bad(reason: impl Into<String>) -> ReportError {
    ReportError::Data {
        reason: reason.into(),
    }
}
pub(crate) fn json(value: &ExprValue) -> Option<Value> {
    Some(match value {
        ExprValue::None => Value::Null,
        ExprValue::Bool(v) => Value::Bool(*v),
        ExprValue::Int(v) => Value::from(*v),
        ExprValue::Float(v) => Value::Number(serde_json::Number::from_f64(*v)?),
        ExprValue::Str(v) => Value::String(v.clone()),
        _ => return None,
    })
}
fn literal(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Literal(value) => Some(match value {
            Literal::None => Value::Null,
            Literal::Bool(v) => (*v).into(),
            Literal::Int(v) => (*v).into(),
            Literal::Float(v) => Value::Number(serde_json::Number::from_f64(*v)?),
            Literal::Str(v) => v.clone().into(),
            _ => return None,
        }),
        Expr::Unary { op, operand } => {
            let Expr::Literal(value) = operand.as_ref() else {
                return None;
            };
            match (op, value) {
                (UnaryOp::UAdd, Literal::Int(_) | Literal::Float(_)) => literal(operand),
                (UnaryOp::USub, Literal::Int(v)) => Some(v.checked_neg()?.into()),
                (UnaryOp::USub, Literal::Float(v)) => {
                    Some(Value::Number(serde_json::Number::from_f64(-v)?))
                }
                _ => None,
            }
        }
        _ => None,
    }
}
pub(crate) fn static_args(
    args: &[Expr],
    keywords: &[(Option<String>, Expr)],
) -> Option<(Vec<Value>, BTreeMap<String, Value>)> {
    let args = args.iter().map(literal).collect::<Option<Vec<_>>>()?;
    let mut kwargs = BTreeMap::new();
    for (name, value) in keywords {
        if kwargs
            .insert(name.as_ref()?.clone(), literal(value)?)
            .is_some()
        {
            return None;
        }
    }
    Some((args, kwargs))
}

pub(crate) fn normalize(
    args: &mut Vec<Value>,
    kwargs: &mut BTreeMap<String, Value>,
    parameters: &[String],
) -> Option<()> {
    if args.len() > parameters.len() || kwargs.keys().any(|key| !parameters.contains(key)) {
        return None;
    }
    for (name, value) in parameters.iter().zip(std::mem::take(args)) {
        if kwargs.insert(name.clone(), value).is_some() {
            return None;
        }
    }
    Some(())
}
