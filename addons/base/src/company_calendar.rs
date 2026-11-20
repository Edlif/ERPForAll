// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company business dates use the transaction's start instant and the company's configured zone.
use std::collections::BTreeMap;

use erp_sdk::prelude::*;
use erp_sdk::{Operation, RawVals};

fn zone(name: &str) -> Result<jiff::tz::TimeZone> {
    if !crate::res_users_preferences::known_time_zone(name) {
        return Err(Error::validation(format!(
            "Unknown company time zone '{name}'."
        )));
    }
    jiff::tz::TimeZone::get(name)
        .map_err(|e| Error::validation(format!("Company time zone '{name}' cannot be loaded: {e}")))
}

pub(crate) fn validate(vals: &RawVals) -> Result<()> {
    match vals.get("tz") {
        Some(Cell::Str(name)) => zone(name).map(|_| ()),
        None => Ok(()),
        _ => Err(Error::validation("Choose a company time zone.")),
    }
}

/// The default in both an unopened form and a create without an explicit company zone.
pub(crate) async fn creator_zone(tx: &mut Transaction, env: &Env) -> Result<Value> {
    let user = env.records("res.users")?.browse([env.uid()])?;
    // A singleton field read checks access and fetches the user. An absent bootstrap
    // user provides no preference; a hidden user or unreadable field still refuses.
    match user.cell(tx, "tz").await {
        Ok(Cell::Str(name)) if !name.is_empty() => return Ok(Value::Str(name)),
        Ok(Cell::Null | Cell::Bool(false) | Cell::Str(_)) => {}
        Ok(_) => {
            return Err(Error::validation(
                "Choose a valid time zone for the creating user.",
            ));
        }
        Err(error) if error.kind() == erp_sdk::ErrorKind::Missing => {}
        Err(error) => return Err(error),
    }
    Ok(Value::Str("UTC".into()))
}

/// A stock cutoff is the first real instant of the local day, encoded in the system's UTC format.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct BusinessDay {
    /// Calendar date of the company's transaction.
    pub date: Date,
    /// Start of that local date in UTC, including time-zone transitions.
    pub start_utc: DateTime,
}

/// Convert an already authorized company zone and UTC instant into its local business day.
/// Native services with read-committed SQL transactions supply their admitted zone here.
/// Invalid zone names or unrepresentable dates return a named error.
pub fn business_day_at(now: DateTime, name: &str) -> Result<BusinessDay> {
    let instant = jiff::Timestamp::from_microsecond(now.micros())
        .map_err(|e| Error::validation(format!("Company clock is out of range: {e}")))?;
    let local = instant.to_zoned(zone(name)?);
    let date = Date::from_ymd(
        i32::from(local.year()),
        local.month() as u32,
        local.day() as u32,
    )
    .ok_or_else(|| Error::validation("Company calendar date is outside the supported range."))?;
    let start = local.start_of_day().map_err(|e| {
        Error::validation(format!(
            "Cannot determine the start of the company day: {e}"
        ))
    })?;
    Ok(BusinessDay {
        date,
        start_utc: DateTime::from_micros(start.timestamp().as_microsecond()),
    })
}

/// Read a single company's local business date at transaction start.
/// Errors when the company is unavailable to the caller or has an invalid zone.
pub async fn business_date(tx: &mut Transaction, company: i64) -> Result<Date> {
    let env = tx.env();
    business_days(tx, &env, &[company])
        .await?
        .get(&company)
        .map(|day| day.date)
        .ok_or_else(|| Error::missing(format!("Company {company} is unavailable.")))
}

/// Return one business day per requested company in the caller's transaction.
/// Errors when the caller cannot read a requested company or its time zone is invalid.
pub async fn business_days(
    tx: &mut Transaction,
    env: &Env,
    ids: &[i64],
) -> Result<BTreeMap<i64, BusinessDay>> {
    let mut days = BTreeMap::new();
    if ids.is_empty() {
        return Ok(days);
    }
    env.records("res.company")?
        .browse(ids.iter().copied())?
        .check_access(tx, Operation::Read)
        .await?;
    if !tx
        .has_field_access(env, "res.company", "tz", erp_sdk::FieldOp::Read)
        .await?
    {
        return Err(Error::access(
            "Reading the company time zone is not allowed.",
        ));
    }
    // Raw SQL must see a pending timezone write without flushing unrelated compute graphs.
    let records = env.records("res.company")?;
    let field = records
        .spec()
        .field("tz")
        .ok_or_else(|| Error::programming("The company calendar requires res.company.tz."))?
        .id;
    tx.flush_fields(records.model_id(), &[field]).await?;
    let rows = tx.query(
        "SELECT id::int8, tz, transaction_timestamp() AT TIME ZONE 'UTC' FROM res_company WHERE id = ANY($1::int8[])",
        &[&ids],
    ).await?;
    for row in rows {
        let id: i64 = row
            .try_get(0)
            .map_err(|e| Error::db(format!("res.company.id: {e}")))?;
        let name: String = row
            .try_get(1)
            .map_err(|e| Error::db(format!("res.company.tz: {e}")))?;
        let now: DateTime = row
            .try_get(2)
            .map_err(|e| Error::db(format!("Company clock: {e}")))?;
        days.insert(id, business_day_at(now, &name)?);
    }
    for id in ids {
        if !days.contains_key(id) {
            return Err(Error::missing(format!("Company {id} is unavailable.")));
        }
    }
    Ok(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_day_uses_its_zone_across_the_date_line_and_dst() -> Result<()> {
        let instant = "2026-11-19T10:30:00Z"
            .parse::<jiff::Timestamp>()
            .map_err(|e| Error::internal(e.to_string()))?;
        assert_eq!(
            business_day_at(
                DateTime::from_micros(instant.as_microsecond()),
                "Pacific/Kiritimati"
            )?
            .date
            .to_string(),
            "2026-11-20"
        );
        assert_eq!(
            business_day_at(
                DateTime::from_micros(instant.as_microsecond()),
                "Pacific/Pago_Pago"
            )?
            .date
            .to_string(),
            "2026-11-18"
        );
        assert_eq!(
            business_day_at(DateTime::from_micros(instant.as_microsecond()), "UTC")?
                .date
                .to_string(),
            "2026-11-19"
        );
        let transition = "2018-11-04T16:00:00Z"
            .parse::<jiff::Timestamp>()
            .map_err(|e| Error::internal(e.to_string()))?;
        assert_eq!(
            business_day_at(
                DateTime::from_micros(transition.as_microsecond()),
                "America/Sao_Paulo"
            )?
            .start_utc
            .to_string(),
            "2018-11-04 03:00:00"
        );
        Ok(())
    }
}
