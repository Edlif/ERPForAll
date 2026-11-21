// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native assistant model discovery in the current caller's transaction.
use erp_sdk::{
    access::Operation,
    ai::{DataSource, LabelledData, ToolCall, ToolName, ToolResult, ToolSpec},
    prelude::*,
    settings::ParameterDecl,
};
use serde_json::{Value, json};

const TOOL: &str = "find_models";
const QUERY_BYTES: &str = "ai_base.catalog_query_bytes";
const RESULT_ROWS: &str = "ai_base.catalog_result_rows";
const HELP_CHARS: &str = "ai_base.catalog_help_chars";
const RESULT_BYTES: &str = "ai_base.catalog_result_bytes";

fn parameters() -> Result<Vec<ParameterDecl>> {
    Ok(vec![
        ParameterDecl::integer(
            QUERY_BYTES,
            "Assistant: maximum model search query bytes",
            256,
            (1, 1024),
        )?,
        ParameterDecl::integer(
            RESULT_ROWS,
            "Assistant: maximum models per search",
            20,
            (1, 80),
        )?,
        ParameterDecl::integer(
            HELP_CHARS,
            "Assistant: maximum model description characters",
            160,
            (1, 2000),
        )?,
        ParameterDecl::integer(
            RESULT_BYTES,
            "Assistant: maximum model search result bytes",
            32768,
            (4096, 65536),
        )?,
    ])
}
erp_sdk::system_parameters!(parameters);

pub(crate) async fn limit(tx: &mut Transaction, key: &str) -> Result<usize> {
    let configured = erp_sdk::params::get_int(tx, &crate::ADDON, key).await?;
    usize::try_from(configured).map_err(|err| Error::internal(format!("{key}: {err}")))
}

fn tool_name() -> Result<ToolName> {
    ToolName::new(TOOL).map_err(|error| Error::programming(error.to_string()))
}

/// The code-declared tool offered to a provider by the assistant loop.
/// User-editable registry descriptions are returned as labelled result data only.
///
/// # Errors
/// Reports an invalid built-in tool definition.
pub fn find_models_spec() -> Result<ToolSpec> {
    let Value::Object(schema) = json!({
        "type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false
    }) else {
        return Err(Error::programming("Invalid find_models tool schema."));
    };
    ToolSpec::new(
        tool_name()?,
        "Find readable installed models by technical name or description.",
        schema,
    )
    .map_err(|error| Error::programming(error.to_string()))
}

fn result(call: &ToolCall, source: DataSource, rows: Vec<Value>, has_more: bool) -> ToolResult {
    ToolResult::new(
        call.id().clone(),
        LabelledData::new(source, json!({"models":rows,"has_more":has_more})),
        false,
    )
}

fn wire_len(value: &impl serde::Serialize) -> Result<usize> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| {
            Error::internal(format!("Could not serialize model catalog result: {error}"))
        })
}

/// Execute the typed `find_models` call under the transaction's ordinary user and company context.
/// Returns model names and bounded descriptions in stable name order. Inaccessible models do
/// not affect pagination. The result contains no records, field names, counts or technical IDs.
///
/// # Errors
/// Refuses elevated, non-user and impersonated transactions, a missing company, malformed or
/// oversized arguments, invalid parameter state and ORM failures. The interactive HTTP entry
/// must separately verify the session and CSRF before dispatching assistant tools.
pub async fn find_models(tx: &mut Transaction, call: &ToolCall) -> Result<ToolResult> {
    let env = tx.env();
    if !crate::ordinary_user(&env) {
        return Err(Error::access(
            "Assistant tools require an unprivileged interactive user.",
        ));
    }
    if env.company()?.is_none() {
        return Err(Error::access("Assistant tools require a current company."));
    }
    let invalid =
        || Error::validation("find_models requires exactly one string argument named query.");
    let args = call.arguments().as_object().ok_or_else(invalid)?;
    if call.name() != TOOL || args.len() != 1 {
        return Err(invalid());
    }
    let text = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let query_limit = limit(tx, QUERY_BYTES).await?;
    if text.len() > query_limit {
        return Err(Error::validation(format!(
            "find_models query exceeds {query_limit} bytes."
        )));
    }
    let query = text.trim().to_lowercase();
    let result_limit = limit(tx, RESULT_ROWS).await?;
    let help_limit = limit(tx, HELP_CHARS).await?;
    let byte_limit = limit(tx, RESULT_BYTES).await?;
    let source = DataSource::new(tool_name()?, None, Vec::new())
        .map_err(|error| Error::programming(error.to_string()))?;
    // Bound the labelled JSON payload consumed by both provider adapters, including its source.
    let mut bytes = wire_len(result(call, source.clone(), Vec::new(), false).data())?;
    let mut models: Vec<_> = env
        .registry()
        .models()
        .iter()
        .filter(|spec| !spec.is_abstract())
        .collect();
    models.sort_unstable_by_key(|spec| spec.name);
    let mut rows = Vec::new();
    for spec in models {
        let mut chars = spec.description.chars();
        let description: String = chars.by_ref().take(help_limit).collect();
        if !spec.name.to_ascii_lowercase().contains(&query)
            && !description.to_lowercase().contains(&query)
        {
            continue;
        }
        if !env
            .records(spec.name)?
            .has_access(tx, Operation::Read)
            .await?
        {
            continue;
        }
        let mut row = json!({"name":spec.name,"description":description,"description_truncated":chars.next().is_some()});
        let room = byte_limit.saturating_sub(bytes);
        let mut added = wire_len(&row)? + usize::from(!rows.is_empty());
        if rows.is_empty() && added > room {
            row["description"] = Value::String(String::new());
            row["description_truncated"] = Value::Bool(true);
            added = wire_len(&row)?;
            if added > room {
                return Err(Error::validation(format!(
                    "{RESULT_BYTES} cannot fit a model name."
                )));
            }
        }
        if rows.len() == result_limit || added > room {
            return Ok(result(call, source, rows, true));
        }
        bytes += added;
        rows.push(row);
    }
    Ok(result(call, source, rows, false))
}
