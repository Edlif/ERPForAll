// SPDX-License-Identifier: MIT OR Apache-2.0
//! The client action tag `reporting.report`: a menu or an action result opens the
//! report screen for the `reporting.report` named by the action's `params`: `{"report": <id>}`.
//! The screen itself is `static/src/client.ts`.
use erp_sdk::client_actions::ClientActionDecl;
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Map, Value as Json, json};

/// The tag the screen registers in the browser.
pub const TAG: &str = "reporting.report";

fn tags() -> Result<Vec<ClientActionDecl>> {
    Ok(vec![ClientActionDecl::new(TAG, "Dynamic report", check)?])
}
erp_sdk::client_action_declarations!(tags);

/// The report id of `params`; any other member, or a missing or non-positive id, is refused.
fn report_of(params: &Map<String, Json>) -> Result<i64> {
    if let Some(unknown) = params.keys().find(|k| k.as_str() != "report") {
        return Err(Error::validation(format!(
            "A report action takes only 'report' in its params, not '{unknown}'."
        )));
    }
    match params.get("report").and_then(Json::as_i64) {
        Some(id) if id >= 1 => Ok(id),
        _ => Err(Error::validation(
            "A report action needs the id of its report as an integer 'report' in its params.",
        )),
    }
}

/// Runs as the caller: reading the report's name is the read-access check.
fn check<'a>(
    env: &'a Env,
    tx: &'a mut Transaction,
    params: &'a Map<String, Json>,
) -> erp_sdk::BoxFuture<'a, Result<Map<String, Json>>> {
    Box::pin(async move {
        let id = report_of(params)?;
        let name = env
            .records("reporting.report")?
            .browse([id])?
            .values(tx, "name")
            .await?;
        if name.is_empty() {
            return Err(Error::validation(format!(
                "Report {id} does not exist or is not readable by you."
            )));
        }
        let mut normalized = Map::new();
        normalized.insert("report".to_owned(), json!(id));
        Ok(normalized)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_name_one_positive_report_id() {
        let object = |value: Json| match value {
            Json::Object(map) => map,
            other => panic!("not an object: {other}"),
        };
        assert_eq!(report_of(&object(json!({"report": 7}))).unwrap(), 7);
        for bad in [
            json!({}),
            json!({"report": 0}),
            json!({"report": "base.partner"}),
            json!({"report": 7, "extra": 1}),
        ] {
            assert!(report_of(&object(bad)).is_err());
        }
    }
}
