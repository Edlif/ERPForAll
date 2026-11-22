// SPDX-License-Identifier: MIT OR Apache-2.0
//! Assistant canvas rendering tool definition and payload handling.
use erp_sdk::{
    ai::{DataSource, LabelledData, ToolCall, ToolName, ToolResult, ToolSpec},
    prelude::*,
};
use serde_json::{Value as Json, json};

const TOOL: &str = "render_canvas";

fn name() -> Result<ToolName> {
    ToolName::new(TOOL).map_err(|e| Error::programming(e.to_string()))
}

/// A provider tool for commanding the AI Workspace dynamic canvas.
///
/// # Errors
/// Returns a programming error if the built-in tool schema is invalid.
pub fn render_canvas_spec() -> Result<ToolSpec> {
    let Json::Object(schema) = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "canvas_type": {
                "type": "string",
                "enum": ["chart", "table", "flow", "dashboard", "simulation"],
                "description": "Visual type: 'chart' (bar, line, donut), 'table' (rich sortable/filterable), 'flow' (process diagram), 'dashboard' (multi-card KPI), 'simulation' (interactive what-if)"
            },
            "title": {
                "type": "string",
                "description": "Heading or title for the canvas card"
            },
            "description": {
                "type": "string",
                "description": "Brief takeaway or subtitle explaining the visualization"
            },
            "data_json": {
                "type": "string",
                "description": "Serialized JSON string of the dataset or configuration (e.g. series for charts, columns & rows for tables, steps/nodes for flows)"
            }
        },
        "required": ["canvas_type", "title", "description", "data_json"]
    }) else {
        return Err(Error::programming("Invalid render_canvas schema."));
    };

    ToolSpec::new(
        name()?,
        "Render an interactive visual widget, chart, table, or animated workflow diagram on the AI Workspace canvas.",
        schema,
    )
    .map_err(|e| Error::programming(e.to_string()))
}

/// Validate and stage a `render_canvas` tool invocation.
///
/// # Errors
/// Returns a validation error if arguments are missing or if `data_json` is not valid JSON.
pub fn render_canvas(call: &ToolCall) -> Result<ToolResult> {
    let args = call.arguments().as_object().ok_or_else(|| {
        Error::validation("render_canvas requires an object argument with canvas_type, title, description, and data_json.")
    })?;

    let canvas_type = args
        .get("canvas_type")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::validation("render_canvas requires a string canvas_type."))?;

    if !matches!(
        canvas_type,
        "chart" | "table" | "flow" | "dashboard" | "simulation"
    ) {
        return Err(Error::validation(format!(
            "Unsupported canvas_type '{canvas_type}'. Expected one of: chart, table, flow, dashboard, simulation."
        )));
    }

    let title = args
        .get("title")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::validation("render_canvas requires a string title."))?;

    if title.len() > 256 {
        return Err(Error::validation(
            "render_canvas title exceeds 256 characters.",
        ));
    }

    let description = args.get("description").and_then(Json::as_str).unwrap_or("");

    if description.len() > 1024 {
        return Err(Error::validation(
            "render_canvas description exceeds 1024 characters.",
        ));
    }

    let data_json = args
        .get("data_json")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::validation("render_canvas requires a string data_json."))?;

    if data_json.len() > 65536 {
        return Err(Error::validation(
            "render_canvas data_json exceeds 64KB limit.",
        ));
    }

    let _parsed: Json = serde_json::from_str(data_json).map_err(|err| {
        Error::validation(format!("render_canvas data_json is not valid JSON: {err}"))
    })?;

    let source = DataSource::new(name()?, None, Vec::new())
        .map_err(|e| Error::programming(e.to_string()))?;

    Ok(ToolResult::new(
        call.id().clone(),
        LabelledData::new(
            source,
            json!({
                "status": "staged",
                "canvas_type": canvas_type,
                "title": title
            }),
        ),
        false,
    ))
}

/// Extract the full parsed canvas object ready for sending to the web client.
pub fn extract_canvas_payload(call: &ToolCall) -> Result<Json> {
    let args = call
        .arguments()
        .as_object()
        .ok_or_else(|| Error::validation("render_canvas arguments must be an object."))?;

    let canvas_type = args
        .get("canvas_type")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::validation("render_canvas requires canvas_type."))?;

    let title = args
        .get("title")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::validation("render_canvas requires title."))?;

    let description = args.get("description").and_then(Json::as_str).unwrap_or("");

    let data_json = args
        .get("data_json")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::validation("render_canvas requires data_json."))?;

    if data_json.len() > 65536 {
        return Err(Error::validation(
            "render_canvas data_json exceeds 64KB limit.",
        ));
    }

    let parsed_data: Json = serde_json::from_str(data_json).map_err(|err| {
        Error::validation(format!("render_canvas data_json is not valid JSON: {err}"))
    })?;

    Ok(json!({
        "canvas_type": canvas_type,
        "title": title,
        "description": description,
        "data": parsed_data
    }))
}
