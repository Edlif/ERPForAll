// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashSet;

use serde_json::{Map, Value};

const MAX_DEPTH: usize = 10;
const MAX_PROPERTIES: usize = 5_000;
const MAX_STRING_CHARS: usize = 120_000;
const MAX_ENUM_VALUES: usize = 1_000;
const LARGE_ENUM_VALUES: usize = 250;
const LARGE_ENUM_MAX_CHARS: usize = 15_000;

/// Checks the portable subset admitted with OpenAI strict mode. Keep the original schema
/// unchanged: closing an open object or making a property mandatory would change its meaning.
pub(super) fn validate(schema: &Map<String, Value>) -> Result<(), &'static str> {
    if schema.get("type").and_then(Value::as_str) != Some("object") || schema.contains_key("anyOf")
    {
        return Err("strict schema root must be an object without anyOf");
    }
    SchemaSize::default().visit(schema, 1)
}

#[derive(Default)]
struct SchemaSize {
    properties: usize,
    string_chars: usize,
    enum_values: usize,
}

impl SchemaSize {
    fn visit(&mut self, schema: &Map<String, Value>, depth: usize) -> Result<(), &'static str> {
        if depth > MAX_DEPTH {
            return Err("strict schema exceeds the nesting limit");
        }
        if schema
            .get("description")
            .is_some_and(|description| !description.is_string())
        {
            return Err("strict schema description must be a string");
        }
        if let Some(any_of) = schema.get("anyOf") {
            check_keys(schema, &["anyOf", "description"])?;
            let branches = any_of
                .as_array()
                .filter(|branches| !branches.is_empty())
                .ok_or("strict schema anyOf must list at least one schema")?;
            for branch in branches {
                self.visit(schema_object(branch)?, depth + 1)?;
            }
            return Ok(());
        }

        let (kind, nullable) = schema_type(schema.get("type"))?;
        match kind {
            "object" => {
                check_keys(
                    schema,
                    &[
                        "type",
                        "description",
                        "properties",
                        "required",
                        "additionalProperties",
                    ],
                )?;
                self.visit_object(schema, depth)
            }
            "array" => {
                check_keys(schema, &["type", "description", "items"])?;
                let items = schema
                    .get("items")
                    .ok_or("strict schema array requires an items schema")?;
                self.visit(schema_object(items)?, depth + 1)
            }
            _ => {
                check_keys(schema, &["type", "description", "enum"])?;
                if let Some(values) = schema.get("enum") {
                    self.visit_enum(values, kind, nullable)?;
                }
                Ok(())
            }
        }
    }

    fn visit_object(
        &mut self,
        schema: &Map<String, Value>,
        depth: usize,
    ) -> Result<(), &'static str> {
        if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
            return Err("strict schema objects require additionalProperties: false");
        }
        let properties = schema.get("properties").map(schema_object).transpose()?;
        let mut required = HashSet::new();
        if let Some(value) = schema.get("required") {
            for name in value
                .as_array()
                .ok_or("strict schema required must be an array")?
            {
                let name = name
                    .as_str()
                    .ok_or("strict schema required must contain property names")?;
                if !required.insert(name) {
                    return Err("strict schema required repeats a property name");
                }
            }
        }
        if required.len() != properties.map_or(0, Map::len) {
            return Err("strict schema required must list exactly all declared properties");
        }
        if let Some(properties) = properties {
            self.properties = bounded_sum(
                self.properties,
                properties.len(),
                MAX_PROPERTIES,
                "strict schema exceeds the total property limit",
            )?;
            for (name, property) in properties {
                if !required.contains(name.as_str()) {
                    return Err("strict schema required must list exactly all declared properties");
                }
                self.count_string_chars(name.chars().count())?;
                self.visit(schema_object(property)?, depth + 1)?;
            }
        }
        Ok(())
    }

    fn visit_enum(
        &mut self,
        values: &Value,
        kind: &str,
        nullable: bool,
    ) -> Result<(), &'static str> {
        let values = values
            .as_array()
            .filter(|values| !values.is_empty())
            .ok_or("strict schema enum must be a nonempty array")?;
        self.enum_values = bounded_sum(
            self.enum_values,
            values.len(),
            MAX_ENUM_VALUES,
            "strict schema exceeds the total enum value limit",
        )?;
        let mut seen = HashSet::new();
        let mut chars = 0;
        for value in values {
            let matches_type = match kind {
                "string" => value.is_string(),
                "number" => value.is_number(),
                "integer" => value.is_i64() || value.is_u64(),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                _ => false,
            };
            if !(matches_type || nullable && value.is_null()) {
                return Err("strict schema enum value must match the declared type");
            }
            if !seen.insert(value.to_string()) {
                return Err("strict schema enum repeats a value");
            }
            if let Some(text) = value.as_str() {
                chars = bounded_sum(
                    chars,
                    text.chars().count(),
                    MAX_STRING_CHARS,
                    "strict schema exceeds the total string size limit",
                )?;
            }
        }
        if values.len() > LARGE_ENUM_VALUES && chars > LARGE_ENUM_MAX_CHARS {
            return Err("strict schema exceeds the string size limit for a large enum");
        }
        self.count_string_chars(chars)
    }

    fn count_string_chars(&mut self, count: usize) -> Result<(), &'static str> {
        self.string_chars = bounded_sum(
            self.string_chars,
            count,
            MAX_STRING_CHARS,
            "strict schema exceeds the total string size limit",
        )?;
        Ok(())
    }
}

fn schema_type(value: Option<&Value>) -> Result<(&str, bool), &'static str> {
    match value {
        Some(Value::String(kind))
            if matches!(
                kind.as_str(),
                "object" | "array" | "string" | "boolean" | "integer" | "number" | "null"
            ) =>
        {
            Ok((kind, false))
        }
        Some(Value::Array(types)) => match types.as_slice() {
            [Value::String(left), Value::String(right)] => match (left.as_str(), right.as_str()) {
                ("null", kind) | (kind, "null")
                    if matches!(
                        kind,
                        "object" | "array" | "string" | "boolean" | "integer" | "number"
                    ) =>
                {
                    Ok((kind, true))
                }
                _ => Err("strict schema type arrays must pair a supported type with null"),
            },
            _ => Err("strict schema type arrays must pair a supported type with null"),
        },
        _ => Err("strict schema requires a supported type"),
    }
}

fn check_keys(schema: &Map<String, Value>, allowed: &[&str]) -> Result<(), &'static str> {
    if schema.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("strict schema contains an unsupported keyword");
    }
    Ok(())
}

fn schema_object(value: &Value) -> Result<&Map<String, Value>, &'static str> {
    value
        .as_object()
        .ok_or("strict schema nodes must be objects")
}

fn bounded_sum(
    previous: usize,
    added: usize,
    limit: usize,
    error: &'static str,
) -> Result<usize, &'static str> {
    previous
        .checked_add(added)
        .filter(|total| *total <= limit)
        .ok_or(error)
}
