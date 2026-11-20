// SPDX-License-Identifier: MIT OR Apache-2.0
//! Execution stays on the ORM side of the database-free report protocol.
use crate::DataError;
use erp_fields::{Error, Value, ValueMap};
use erp_orm::{
    Env, Transaction,
    report_helpers::ReportRead,
    rpc::{BindError, RpcArgs},
};
use erp_report::{HelperRequest, ReportError, ReportHelper};
use std::collections::BTreeMap;

/// The authoritative installed helper catalog for planning and attachment admission.
pub fn report_helpers(tx: &Transaction) -> Vec<ReportHelper<'_>> {
    tx.exposure()
        .report_helpers()
        .iter()
        .map(|h| {
            ReportHelper::new(h.model(), h.method(), h.dependencies())
                .with_parameters(h.parameters())
        })
        .collect()
}
fn error(request: &HelperRequest, reason: impl std::fmt::Display) -> DataError {
    ReportError::Data {
        reason: format!(
            "report helper {}.{}: {reason}",
            request.model(),
            request.method()
        ),
    }
    .into()
}
fn argument(value: &serde_json::Value) -> Result<Value, &'static str> {
    Ok(match value {
        serde_json::Value::Null => Value::False,
        serde_json::Value::Bool(v) => Value::Bool(*v),
        serde_json::Value::String(v) => Value::str(v),
        serde_json::Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                Value::Int(i)
            } else if !v.is_f64() {
                return Err("integer outside i64 range");
            } else {
                Value::Float(v.as_f64().ok_or("invalid float")?)
            }
        }
        _ => return Err("only scalar arguments are supported"),
    })
}
fn result(value: &Value) -> Result<serde_json::Value, &'static str> {
    Ok(match value {
        Value::False => serde_json::Value::Null,
        Value::Bool(v) => serde_json::Value::Bool(*v),
        Value::Int(v) => serde_json::Value::from(*v),
        Value::Float(v) => {
            serde_json::Value::Number(serde_json::Number::from_f64(*v).ok_or("non-finite result")?)
        }
        Value::Str(v) => serde_json::Value::String(v.to_string()),
        _ => return Err("expected a scalar result for each record"),
    })
}
pub(crate) async fn execute(
    tx: &mut Transaction,
    env: &Env,
    request: &HelperRequest,
) -> Result<Vec<serde_json::Value>, DataError> {
    let registered = tx
        .exposure()
        .report_helpers()
        .iter()
        .find(|h| h.model() == request.model() && h.method() == request.method())
        .ok_or_else(|| error(request, "not registered in this transaction"))?;
    let actual: std::collections::BTreeSet<_> =
        request.dependencies().iter().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<_> =
        registered.dependencies().iter().copied().collect();
    if actual != expected {
        return Err(error(
            request,
            "dependency declaration differs from the registry",
        ));
    }
    let records = env
        .records(request.model())?
        .browse(request.ids().to_vec())?;
    let callable = tx
        .data_callable(records.model_id(), request.method())
        .cloned()
        .ok_or_else(|| error(request, "not data-callable"))?;
    let mut supplied: BTreeMap<String, serde_json::Value> = request.kwargs().clone();
    if request.args().len() > callable.params().len() {
        return Err(error(request, "too many positional arguments"));
    }
    for (arg, param) in request.args().iter().zip(callable.params()) {
        if supplied
            .insert(param.name().to_owned(), arg.clone())
            .is_some()
        {
            return Err(error(
                request,
                format!("argument {} supplied twice", param.name()),
            ));
        }
    }
    for name in supplied.keys() {
        if !callable.params().iter().any(|p| p.name() == name) {
            return Err(error(request, format!("unknown argument {name}")));
        }
    }
    for param in callable.params() {
        if param.required() && !supplied.contains_key(param.name()) {
            return Err(error(request, format!("missing argument {}", param.name())));
        }
    }
    let args: ValueMap = supplied
        .iter()
        .map(|(k, v)| {
            argument(v)
                .map(|v| (k.clone(), v))
                .map_err(|why| error(request, format!("argument {k}: {why}")))
        })
        .collect::<Result<_, _>>()?;
    let reads = request
        .reads()
        .iter()
        .map(|read| {
            let fields: Vec<_> = read.fields().iter().map(String::as_str).collect();
            ReportRead::new(env.registry(), read.model(), read.ids(), &fields)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut scope = tx.report_helper_scope(&records, request.method(), reads)?;
    let answer = callable.call(&mut scope, records, RpcArgs::new(args)).await;
    scope.finish()?;
    let values = match answer {
        Ok(Value::List(values)) => values,
        Ok(_) => return Err(error(request, "expected an ordered list of results")),
        Err(BindError::Call(e)) => return Err(e.into()),
        Err(e) => {
            return Err(Error::validation(format!(
                "report helper {}.{}: {e}",
                request.model(),
                request.method()
            ))
            .into());
        }
    };
    values
        .iter()
        .map(|v| result(v).map_err(|why| error(request, why)))
        .collect()
}
