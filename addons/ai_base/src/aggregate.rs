// SPDX-License-Identifier: MIT OR Apache-2.0
//! Plan a bounded assistant aggregation query for the audited `web_read_group` dispatch.
use erp_sdk::{
    FieldType,
    access::Operation,
    ai::{DataSource, LabelledData, ToolCall, ToolName, ToolResult, ToolSpec},
    prelude::*,
    settings::ParameterDecl,
};
use serde_json::{Map, Value as Json, json};
use std::collections::BTreeSet;

const TOOL: &str = "aggregate_records";
const ROWS: &str = "ai_base.aggregate_rows";
const AGG_FIELDS: &str = "ai_base.aggregate_fields";
const GROUPBY_FIELDS: &str = "ai_base.aggregate_groupby";
const TERMS: &str = "ai_base.aggregate_terms";
const BYTES: &str = "ai_base.aggregate_result_bytes";
const OPERAND: &str = "ai_base.aggregate_operand_bytes";

fn parameters() -> Result<Vec<ParameterDecl>> {
    Ok(vec![
        ParameterDecl::integer(
            ROWS,
            "Assistant: maximum groups per aggregation",
            50,
            (1, 80),
        )?,
        ParameterDecl::integer(
            AGG_FIELDS,
            "Assistant: maximum aggregate measures",
            8,
            (1, 32),
        )?,
        ParameterDecl::integer(
            GROUPBY_FIELDS,
            "Assistant: maximum groupby fields",
            4,
            (1, 16),
        )?,
        ParameterDecl::integer(
            TERMS,
            "Assistant: maximum aggregate filter conditions",
            16,
            (1, 64),
        )?,
        ParameterDecl::integer(
            BYTES,
            "Assistant: maximum labelled aggregate result bytes",
            32768,
            (4096, 65536),
        )?,
        ParameterDecl::integer(
            OPERAND,
            "Assistant: maximum aggregate operand bytes",
            256,
            (1, 2048),
        )?,
    ])
}
erp_sdk::system_parameters!(parameters);

fn invalid() -> Error {
    Error::validation(
        "aggregate_records requires model, domain, groupby array, aggregates array, limit, and order.",
    )
}

fn denied() -> Error {
    Error::access("Assistant aggregation cannot access this model or field.")
}

fn name() -> Result<ToolName> {
    ToolName::new(TOOL).map_err(|e| Error::programming(e.to_string()))
}

fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A provider tool with a strict schema for group-by and aggregate calculations.
///
/// # Errors
/// Returns a programming error for an invalid built-in declaration.
pub fn aggregate_records_spec() -> Result<ToolSpec> {
    let scalar = json!({"anyOf":[{"type":"string"},{"type":"number"},{"type":"boolean"}]});
    let Json::Object(schema) = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "model": {
                "type": "string",
                "description": "Installed target model identifier (such as purchase.order or account.move)",
            },
            "domain": {
                "type": "array",
                "description": "List of AND conditions [[field, operator, scalar value]], or [] for all records in the company",
                "items": {"type": "array", "items": scalar}
            },
            "groupby": {
                "type": "array",
                "description": "List of field names to group by (e.g. ['partner_id'] or ['state'])",
                "items": {"type": "string"}
            },
            "aggregates": {
                "type": "array",
                "description": "List of aggregate measures (e.g. ['amount_total:sum', 'id:count'])",
                "items": {"type": "string"}
            },
            "limit": {
                "type": "integer",
                "description": "Maximum groups to return (1-50)"
            },
            "order": {
                "type": "string",
                "description": "Sort order for the groups (e.g. 'amount_total desc' or 'id:count desc')"
            }
        },
        "required": ["model", "domain", "groupby", "aggregates", "limit", "order"]
    }) else {
        return Err(Error::programming("Invalid aggregate_records schema."));
    };

    ToolSpec::new(
        name()?,
        "Compute aggregate sums, counts and group-by metrics across business records of an installed model.",
        schema,
    )
    .map_err(|e| Error::programming(e.to_string()))
}

/// Approved aggregation plan for standard `web_read_group` execution.
#[derive(Clone, PartialEq)]
pub struct AggregatePlan {
    model: String,
    groupby: Vec<String>,
    aggregates: Vec<String>,
    domain: Json,
    order: String,
    limit: usize,
    company: i64,
    byte_limit: usize,
}

impl std::fmt::Debug for AggregatePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggregatePlan")
            .field("model", &self.model)
            .field("groupby", &self.groupby)
            .field("aggregates", &self.aggregates)
            .finish_non_exhaustive()
    }
}

impl AggregatePlan {
    /// The installed model for the RPC call.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The arguments for the standard `web_read_group` binder.
    pub fn rpc_arguments(&self) -> Json {
        let mut map = serde_json::Map::new();
        map.insert("domain".to_string(), self.domain.clone());
        map.insert("groupby".to_string(), json!(self.groupby));
        if !self.aggregates.is_empty() {
            map.insert("aggregates".to_string(), json!(self.aggregates));
        }
        map.insert("limit".to_string(), json!(self.limit));
        map.insert("offset".to_string(), json!(0));
        if !self.order.is_empty() {
            map.insert("order".to_string(), json!(self.order));
        }
        map.insert(
            "context".to_string(),
            json!({ "allowed_company_ids": [self.company] }),
        );
        Json::Object(map)
    }

    /// Reduce an audited successful `web_read_group` response to bounded labelled data.
    ///
    /// # Errors
    /// Enforces response structure and result byte limits.
    pub fn project(&self, call: &ToolCall, reply: &Json) -> Result<ToolResult> {
        let malformed =
            || Error::internal("Unexpected web_read_group result for assistant aggregation.");
        let groups = reply["groups"].as_array().ok_or_else(malformed)?;
        let total_groups = reply["length"].as_u64().unwrap_or(groups.len() as u64);

        let mut projected_groups = Vec::new();
        for group in groups {
            let obj = group.as_object().ok_or_else(malformed)?;
            let mut row = Map::new();

            // Extract groupby values (scalars or [id, display_name])
            for g in &self.groupby {
                if let Some(val) = obj.get(g) {
                    row.insert(g.clone(), val.clone());
                }
            }

            // Extract aggregate metrics (e.g. amount_total, __count, etc.)
            for agg in &self.aggregates {
                let clean_name = agg.split(':').next().unwrap_or(agg.as_str());
                if let Some(val) = obj.get(clean_name).or_else(|| obj.get(agg.as_str())) {
                    row.insert(clean_name.to_string(), val.clone());
                }
            }
            if let Some(count) = obj.get("__count") {
                row.insert("__count".to_string(), count.clone());
            }

            projected_groups.push(Json::Object(row));
            if projected_groups.len() >= self.limit {
                break;
            }
        }

        let source = DataSource::new(name()?, Some(self.model.clone()), Vec::new())
            .map_err(|e| Error::programming(e.to_string()))?;

        let data = json!({
            "model": self.model,
            "groups": projected_groups,
            "total_groups": total_groups,
            "groupby": self.groupby,
            "aggregates": self.aggregates
        });

        let serialized_len = serde_json::to_vec(&data).map_err(|_| malformed())?.len();
        if serialized_len > self.byte_limit {
            return Err(Error::validation(
                "Assistant aggregation result exceeds byte budget.",
            ));
        }

        Ok(ToolResult::new(
            call.id().clone(),
            LabelledData::new(source, data),
            false,
        ))
    }
}

/// Validate a typed `aggregate_records` call against the transaction's user context,
/// company permissions, model access rights, and field visibility.
///
/// # Errors
/// Refuses unprivileged or missing company context, inaccessible models, invalid fields,
/// or malformed argument structures.
pub async fn prepare(tx: &mut Transaction, call: &ToolCall) -> Result<AggregatePlan> {
    let env = tx.env();
    if !crate::ordinary_user(&env) {
        return Err(denied());
    }
    let company = env.company()?.ok_or_else(denied)?.get();
    let args = call.arguments().as_object().ok_or_else(invalid)?;
    if call.name() != TOOL || args.len() != 6 {
        return Err(invalid());
    }

    let model = args
        .get("model")
        .and_then(Json::as_str)
        .ok_or_else(invalid)?;
    if model.len() > 128 || !model.split('.').all(ident) {
        return Err(invalid());
    }

    if env
        .registry()
        .model(model)
        .is_none_or(|spec| spec.is_abstract())
    {
        return Err(denied());
    }

    let recs = env.records(model)?;
    if !recs.has_access(tx, Operation::Read).await? {
        return Err(denied());
    }

    let limit = args
        .get("limit")
        .and_then(Json::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(invalid)?;
    let max_rows = crate::catalog::limit(tx, ROWS).await?;
    if limit == 0 || limit > max_rows {
        return Err(Error::validation(format!(
            "aggregate_records limit must be between 1 and {max_rows}."
        )));
    }

    let raw_groupby = args
        .get("groupby")
        .and_then(Json::as_array)
        .ok_or_else(invalid)?;
    let max_groupby = crate::catalog::limit(tx, GROUPBY_FIELDS).await?;
    if raw_groupby.is_empty() || raw_groupby.len() > max_groupby {
        return Err(Error::validation(format!(
            "aggregate_records groupby must have between 1 and {max_groupby} fields."
        )));
    }

    let mut groupby = Vec::new();
    for g in raw_groupby {
        let name = g.as_str().filter(|s| ident(s)).ok_or_else(invalid)?;
        groupby.push(name.to_string());
    }

    let raw_aggregates = args
        .get("aggregates")
        .and_then(Json::as_array)
        .ok_or_else(invalid)?;
    let max_aggs = crate::catalog::limit(tx, AGG_FIELDS).await?;
    if raw_aggregates.is_empty() || raw_aggregates.len() > max_aggs {
        return Err(Error::validation(format!(
            "aggregate_records aggregates must have between 1 and {max_aggs} measures."
        )));
    }

    let mut aggregates = Vec::new();
    for agg in raw_aggregates {
        let text = agg.as_str().ok_or_else(invalid)?;
        let mut parts = text.split(':');
        let clean_field = parts.next().unwrap_or("");
        let func = parts.next().unwrap_or("sum");
        if parts.next().is_some()
            || !ident(clean_field)
            || !matches!(
                func,
                "sum" | "avg" | "min" | "max" | "count" | "bool_and" | "bool_or"
            )
        {
            return Err(invalid());
        }
        aggregates.push(text.to_string());
    }

    let raw_domain = args
        .get("domain")
        .and_then(Json::as_array)
        .ok_or_else(invalid)?;
    let max_terms = crate::catalog::limit(tx, TERMS).await?;
    if raw_domain.len() > max_terms {
        return Err(Error::validation(format!(
            "aggregate_records domain exceeds {max_terms} conditions."
        )));
    }

    let order = args
        .get("order")
        .and_then(Json::as_str)
        .ok_or_else(invalid)?;
    if order.len() > 70 {
        return Err(invalid());
    }

    let order_field = if !order.trim().is_empty() {
        let mut order_parts = order.split_ascii_whitespace();
        let ofield = order_parts.next().ok_or_else(invalid)?;
        if let Some(dir) = order_parts.next()
            && !dir.eq_ignore_ascii_case("asc")
            && !dir.eq_ignore_ascii_case("desc")
        {
            return Err(invalid());
        }
        if order_parts.next().is_some() {
            return Err(invalid());
        }
        let clean_order_field = ofield.split(':').next().unwrap_or(ofield);
        if !ident(clean_order_field) && ofield != "__count" {
            return Err(invalid());
        }
        Some(clean_order_field.to_string())
    } else {
        None
    };

    // Verify fields exist and are accessible on the model
    let mut check_fields: BTreeSet<&str> = BTreeSet::new();
    for g in &groupby {
        check_fields.insert(g.as_str());
    }
    for agg in &aggregates {
        let clean = agg.split(':').next().unwrap_or(agg.as_str());
        check_fields.insert(clean);
    }
    if let Some(ref ofield) = order_field
        && ofield != "__count"
    {
        check_fields.insert(ofield.as_str());
    }

    let operand_limit = crate::catalog::limit(tx, OPERAND).await?;
    for item in raw_domain {
        let triple = item.as_array().ok_or_else(invalid)?;
        let [Json::String(field), Json::String(op), rhs] = triple.as_slice() else {
            return Err(invalid());
        };
        if !ident(field) || !matches!(op.as_str(), "=" | "!=" | ">" | "<" | ">=" | "<=" | "ilike") {
            return Err(invalid());
        }
        match rhs {
            Json::Bool(_) | Json::Number(_) if op != "ilike" => {}
            Json::String(v) if v.len() <= operand_limit => {}
            _ => return Err(invalid()),
        }
        check_fields.insert(field.as_str());
    }

    let check_vec: Vec<&str> = check_fields.into_iter().collect();
    let readable = recs
        .fields_get(tx, Some(&check_vec), Some(&["readonly"]))
        .await?;

    let is_safe_field = |field: &str| {
        readable.contains_key(field)
            && recs.spec().field(field).is_some_and(|s| {
                s.store
                    && !s.secret
                    && !s.is_generated()
                    && matches!(
                        s.ty,
                        FieldType::Id
                            | FieldType::Boolean
                            | FieldType::Integer
                            | FieldType::BigInt
                            | FieldType::Float { .. }
                            | FieldType::Monetary { .. }
                            | FieldType::Char { .. }
                            | FieldType::Selection { .. }
                            | FieldType::Date
                            | FieldType::Datetime
                            | FieldType::Many2one { .. }
                    )
            })
    };

    for f in &groupby {
        if !is_safe_field(f) {
            return Err(denied());
        }
    }
    for agg in &aggregates {
        let clean = agg.split(':').next().unwrap_or(agg.as_str());
        if !is_safe_field(clean) {
            return Err(denied());
        }
    }
    for item in raw_domain {
        let triple = item.as_array().ok_or_else(invalid)?;
        let [Json::String(field), _, _] = triple.as_slice() else {
            return Err(invalid());
        };
        if !is_safe_field(field.as_str()) {
            return Err(denied());
        }
    }
    if let Some(ref ofield) = order_field
        && ofield != "__count"
        && !is_safe_field(ofield.as_str())
    {
        return Err(denied());
    }

    let domain = Json::Array(raw_domain.clone());
    if !raw_domain.is_empty() {
        Domain::from_json(&domain).map_err(|_| invalid())?;
    }

    Ok(AggregatePlan {
        model: model.to_string(),
        groupby,
        aggregates,
        domain,
        order: order.to_string(),
        limit,
        company,
        byte_limit: crate::catalog::limit(tx, BYTES).await?,
    })
}
