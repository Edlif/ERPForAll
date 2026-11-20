// SPDX-License-Identifier: MIT OR Apache-2.0
//! Caller-rights metadata loading, independent of the elevated field-description adapter.
use crate::{DataError, mismatch};
use erp_domain::{Domain, Operator, Value as DomainValue};
use erp_fields::Value;
use erp_orm::{Env, SearchOptions, Transaction};
use erp_report::{LoadedPrecision, ReportError};

pub(crate) async fn read(
    tx: &mut Transaction,
    env: &Env,
    names: &[&str],
) -> Result<Vec<LoadedPrecision>, DataError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let domain = Domain::condition(
        "name",
        Operator::In,
        DomainValue::List(names.iter().map(|name| DomainValue::from(*name)).collect()),
    )
    .map_err(|error| ReportError::Data {
        reason: format!("decimal precision name domain: {error}"),
    })?;
    let limit = names
        .len()
        .checked_add(1)
        .ok_or_else(|| ReportError::Data {
            reason: "decimal precision request size overflow".into(),
        })?;
    let limit = u64::try_from(limit).map_err(|_| ReportError::Data {
        reason: "decimal precision request size overflow".into(),
    })?;
    let rows = env
        .records("decimal.precision")?
        .search_read(
            tx,
            &domain,
            &["name", "digits"],
            &SearchOptions {
                limit: Some(limit),
                ..SearchOptions::default()
            },
        )
        .await?;
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let name = match row.get("name") {
            Some(Value::Str(name)) => name.as_ref(),
            Some(other) => return Err(mismatch("decimal.precision", "name", "a string", other)),
            None => {
                return Err(crate::value_error(
                    "decimal.precision",
                    "name",
                    "the read returned no name",
                ));
            }
        };
        let digits = match row.get("digits") {
            Some(Value::Int(digits)) => *digits,
            Some(other) => {
                return Err(mismatch("decimal.precision", "digits", "an integer", other));
            }
            None => {
                return Err(crate::value_error(
                    "decimal.precision",
                    "digits",
                    "the read returned no digits",
                ));
            }
        };
        values.push(LoadedPrecision::new(name, digits)?);
    }
    Ok(values)
}
