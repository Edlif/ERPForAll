// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io::{self, Write};

use erp_sdk::ai::{ContentPart, LabelledData, ToolTurnRequest};
use serde_json::{Map, Value};

use super::Family;

// These are hard structural safety limits, independent of the node's copy/wire byte budget.
const MAX_NODES: usize = 16_384;
const MAX_JSON_DEPTH: usize = 40;
const BYTES: &str = "request exceeds ai.max_request_bytes";
const NODES: &str = "request exceeds the 16384-entry structural safety limit";
const DEPTH: &str = "request JSON exceeds the 40-level nesting safety limit";

pub(super) fn preflight(
    request: &ToolTurnRequest,
    family: Family,
    limit: usize,
) -> Result<(), &'static str> {
    let mut budget = Budget {
        bytes: limit,
        nodes: MAX_NODES,
    };
    // Allow for the model/options and the top-level arrays.
    budget.bytes(256)?;
    budget.text(request.model(), 1)?;
    budget.nodes(request.tools().len())?;
    budget.nodes(request.messages().len())?;
    for tool in request.tools() {
        budget.bytes(128)?;
        budget.text(tool.name().as_str(), 1)?;
        budget.text(tool.description(), 1)?;
        budget.map(tool.input_schema(), 1)?;
    }
    for message in request.messages() {
        budget.bytes(64)?;
        budget.nodes(message.parts().len())?;
        for part in message.parts() {
            // Covers the message/part field names, type tags, separators, and data delimiter.
            budget.bytes(256)?;
            match part {
                ContentPart::Instruction(instruction) => {
                    let (bytes, nodes) = instruction.render_budget(budget.bytes / 6, budget.nodes)
                        .ok_or("instruction exceeds ai.max_request_bytes or the structural safety limit")?;
                    budget.nodes(nodes)?;
                    // The SDK measures rendered UTF-8 bytes without expanding any text.
                    budget.bytes(bytes.checked_mul(6).ok_or(BYTES)?)?;
                }
                ContentPart::UserText(text) => budget.text(text.as_str(), 1)?,
                ContentPart::AssistantText(text) => budget.text(text, 1)?,
                ContentPart::Data(data) => budget.labelled(data)?,
                ContentPart::ToolResult(result) => {
                    budget.text(result.id().as_str(), 1)?;
                    budget.labelled(result.data())?;
                }
                ContentPart::ToolCall(call) => {
                    budget.text(call.id().as_str(), 1)?;
                    budget.text(call.name(), 1)?;
                    // OpenAI arguments are a compact JSON document inside an outer JSON string.
                    let copies = if family == Family::OpenAiCompatible {
                        2
                    } else {
                        1
                    };
                    budget.walk(&[(call.arguments(), 1)], copies)?;
                }
                _ => return Err("unsupported SDK content part"),
            }
        }
    }
    Ok(())
}

struct Budget {
    bytes: usize,
    nodes: usize,
}

impl Budget {
    fn bytes(&mut self, count: usize) -> Result<(), &'static str> {
        self.bytes = self.bytes.checked_sub(count).ok_or(BYTES)?;
        Ok(())
    }

    fn nodes(&mut self, count: usize) -> Result<(), &'static str> {
        self.nodes = self.nodes.checked_sub(count).ok_or(NODES)?;
        Ok(())
    }

    fn scaled(&mut self, count: usize, copies: usize) -> Result<(), &'static str> {
        self.bytes(count.checked_mul(copies).ok_or(BYTES)?)
    }

    fn text(&mut self, text: &str, copies: usize) -> Result<(), &'static str> {
        // Refuse large input from its byte length before scanning it for JSON escapes.
        self.scaled(text.len(), copies)?;
        self.scaled(2, copies)?;
        for byte in text.bytes() {
            let expansion = match byte {
                b'"' | b'\\' | 0x08..=0x0a | 0x0c..=0x0d => 1,
                0x00..=0x1f => 5,
                _ => continue,
            };
            self.scaled(expansion, copies)?;
        }
        Ok(())
    }

    fn labelled(&mut self, data: &LabelledData) -> Result<(), &'static str> {
        let source = data.source();
        self.text(source.tool().as_str(), 2)?;
        if let Some(model) = source.model() {
            self.text(model, 2)?;
        }
        self.nodes(source.ids().len())?;
        // A signed i64 is at most 20 decimal characters; the extra byte covers the comma.
        self.scaled(source.ids().len().checked_mul(21).ok_or(BYTES)?, 2)?;
        self.walk(&[(data.value(), 1)], 2)
    }

    fn map(&mut self, map: &Map<String, Value>, copies: usize) -> Result<(), &'static str> {
        let mut stack = Vec::new();
        self.push_map(map, 1, copies, &mut stack)?;
        self.visit(stack, copies)
    }

    fn walk(&mut self, roots: &[(&Value, usize)], copies: usize) -> Result<(), &'static str> {
        self.nodes(roots.len())?;
        self.visit(roots.to_vec(), copies)
    }

    fn push_map<'a>(
        &mut self,
        map: &'a Map<String, Value>,
        depth: usize,
        copies: usize,
        stack: &mut Vec<(&'a Value, usize)>,
    ) -> Result<(), &'static str> {
        self.nodes(map.len())?;
        self.scaled(2, copies)?;
        for (key, value) in map {
            self.text(key, copies)?;
            self.scaled(2, copies)?; // colon and comma
            stack.push((value, depth));
        }
        Ok(())
    }

    fn visit(
        &mut self,
        mut stack: Vec<(&Value, usize)>,
        copies: usize,
    ) -> Result<(), &'static str> {
        while let Some((value, depth)) = stack.pop() {
            if depth > MAX_JSON_DEPTH {
                return Err(DEPTH);
            }
            match value {
                Value::Null | Value::Bool(_) => self.scaled(5, copies)?,
                // serde_json's native i64/u64/f64 representations fit in 32 output bytes.
                Value::Number(_) => self.scaled(32, copies)?,
                Value::String(text) => self.text(text, copies)?,
                Value::Object(map) => self.push_map(map, depth + 1, copies, &mut stack)?,
                Value::Array(items) => {
                    self.nodes(items.len())?;
                    self.scaled(2, copies)?;
                    self.scaled(items.len(), copies)?;
                    stack.extend(items.iter().map(|item| (item, depth + 1)));
                }
            }
        }
        Ok(())
    }
}

// Count the actual compact provider JSON with no body buffer. The preflight already
// bounds serialization work and nesting; this last check covers all wire scaffolding.
pub(super) fn check_wire_size(body: &Value, limit: usize) -> Result<(), &'static str> {
    let mut wire = WireSize {
        remaining: limit,
        exceeded: false,
    };
    serde_json::to_writer(&mut wire, body).map_err(|_| {
        if wire.exceeded {
            BYTES
        } else {
            "provider request JSON encoding failed"
        }
    })
}

struct WireSize {
    remaining: usize,
    exceeded: bool,
}

impl Write for WireSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(remaining) = self.remaining.checked_sub(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::other(BYTES));
        };
        self.remaining = remaining;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
