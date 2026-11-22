// SPDX-License-Identifier: MIT OR Apache-2.0
//! Request bodies for the AI provider protocols supported by a configured route.

use std::collections::HashSet;

use erp_sdk::ai::{ContentPart, ProviderError, ToolTurnRequest, ToolTurnResponse, TurnRole};
use serde_json::{Map, Value, json};

pub(crate) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

mod admission;
mod arguments;
mod response;
mod strict_schema;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Anthropic,
    OpenAiCompatible,
}

/// Encodes requests and decodes replies for one configured assistant turn.
///
/// Obtain it from [the configured route](crate::routing::ConfiguredRoute::request_encoder)
/// once per turn. Reuse it for follow-up requests in the same turn so the delimiter around
/// labelled context stays stable.
pub struct RequestEncoder<'route> {
    provider: &'route str,
    model: &'route str,
    family: Family,
    nonce: String,
    request_bytes: usize,
}

impl<'route> RequestEncoder<'route> {
    pub(crate) fn new(
        provider: &'route str,
        model: &'route str,
        family: &str,
        request_bytes: usize,
    ) -> Result<Self, ProviderError> {
        let family = match family {
            "anthropic" => Family::Anthropic,
            "openai_compatible" => Family::OpenAiCompatible,
            _ => {
                return Err(ProviderError::InvalidRequest {
                    provider: provider.to_owned(),
                    message: "unsupported configured provider protocol family".to_owned(),
                });
            }
        };
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| ProviderError::Unavailable {
            provider: provider.to_owned(),
            message: "could not generate the labelled context delimiter".to_owned(),
        })?;
        let nonce = random.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            provider,
            model,
            family,
            nonce,
            request_bytes,
        })
    }

    /// Encodes the typed conversation, sampling options and offered tools as a JSON request body.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] when the request cannot be represented in the
    /// route's protocol or exceeds the request admission limits. The error omits message and argument values.
    pub fn encode(&self, request: &ToolTurnRequest) -> Result<Value, ProviderError> {
        self.encode_body(request)
            .map_err(|message| ProviderError::InvalidRequest {
                provider: self.provider.to_owned(),
                message,
            })
    }

    /// Decodes one complete, non-streaming reply from a successful provider exchange.
    ///
    /// The response's resolved model identifier and token counts are kept for metering.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidResponse`] for a malformed, oversized, unsupported or
    /// inconsistent response. Errors omit raw provider text and argument values.
    pub fn decode_response(&self, body: &[u8]) -> Result<ToolTurnResponse, ProviderError> {
        response::decode(self.family, body, self.provider).map_err(|message| {
            ProviderError::InvalidResponse {
                provider: self.provider.to_owned(),
                message: message.to_owned(),
            }
        })
    }

    fn encode_body(&self, request: &ToolTurnRequest) -> Result<Value, String> {
        admission::preflight(request, self.family, self.request_bytes).map_err(str::to_owned)?;
        if request.model().trim().is_empty() || request.model() != self.model {
            return Err("request model must equal the configured route model".to_owned());
        }
        let mut body = Map::new();
        body.insert("model".to_owned(), json!(self.model));

        let (token_field, max_temperature) = match self.family {
            Family::Anthropic => ("max_tokens", 1.0),
            Family::OpenAiCompatible => ("max_completion_tokens", 2.0),
        };
        match request.max_tokens() {
            Some(0) => return Err("max_tokens must be positive".to_owned()),
            Some(limit) => {
                body.insert(token_field.to_owned(), json!(limit));
            }
            None if self.family == Family::Anthropic => {
                return Err("Anthropic requires max_tokens".to_owned());
            }
            None => {}
        }
        if let Some(temperature) = request.temperature() {
            if !(0.0..=max_temperature).contains(&temperature) {
                return Err(format!(
                    "temperature must be finite and within 0 to {max_temperature}"
                ));
            }
            body.insert("temperature".to_owned(), json!(temperature));
        }

        let tools = request
            .tools()
            .iter()
            .map(|tool| match self.family {
                Family::Anthropic => Ok(json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "input_schema": tool.input_schema()
                })),
                Family::OpenAiCompatible => {
                    strict_schema::validate(tool.input_schema())
                        .map_err(|reason| format!("tool '{}': {reason}", tool.name()))?;
                    Ok(json!({
                        "type": "function",
                        "function": {
                            "name": tool.name(),
                            "description": tool.description(),
                            "parameters": tool.input_schema(),
                            "strict": true
                        }
                    }))
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        if !tools.is_empty() {
            body.insert("tools".to_owned(), Value::Array(tools));
        }

        let history = self.encode_history(request)?;
        body.insert("messages".to_owned(), Value::Array(history.messages));
        if !history.system.is_empty() {
            body.insert("system".to_owned(), Value::Array(history.system));
        }
        let body = Value::Object(body);
        admission::check_wire_size(&body, self.request_bytes).map_err(str::to_owned)?;
        Ok(body)
    }

    fn encode_history(&self, request: &ToolTurnRequest) -> Result<EncodedHistory, String> {
        let mut history = EncodedHistory::default();
        let mut pending = HashSet::new();
        let mut saw_user = false;

        for (index, message) in request.messages().iter().enumerate() {
            let role = message.role();
            if !saw_user && role == TurnRole::Assistant {
                return Err(format!(
                    "message {index}: history must begin with user-side content"
                ));
            }
            if role != TurnRole::User && !pending.is_empty() {
                return Err(format!(
                    "message {index}: previous tool calls are unanswered"
                ));
            }
            if self.family == Family::Anthropic && saw_user && role == TurnRole::System {
                return Err(format!(
                    "message {index}: Anthropic requires leading system instructions"
                ));
            }
            saw_user |= role == TurnRole::User;

            let mut content = Vec::new();
            let mut openai_calls = Vec::new();
            for part in message.parts() {
                if role == TurnRole::User
                    && !pending.is_empty()
                    && !matches!(part, ContentPart::ToolResult(_))
                {
                    return Err(format!(
                        "message {index}: all tool results must appear before user content"
                    ));
                }
                match part {
                    ContentPart::Instruction(instruction) => {
                        content.push(self.text_block(instruction.render(), index)?);
                    }
                    ContentPart::UserText(text) => {
                        content.push(self.text_block(text.as_str(), index)?);
                    }
                    ContentPart::Data(data) => {
                        // Initial context has no call ID in either protocol. JSON escaping keeps
                        // embedded newlines from introducing another delimiter on its own line.
                        let encoded = serde_json::to_string(data).map_err(|_| {
                            format!("message {index}: context JSON encoding failed")
                        })?;
                        let nonce = &self.nonce;
                        content.push(text_block(format!(
                            "<erp-data:{nonce}>\n{encoded}\n</erp-data:{nonce}>"
                        )));
                    }
                    ContentPart::AssistantText(text) => {
                        if !openai_calls.is_empty() {
                            return Err(format!(
                                "message {index}: OpenAI-compatible history cannot represent assistant text after a tool call"
                            ));
                        }
                        content.push(self.text_block(text, index)?);
                    }
                    ContentPart::ToolCall(call) => {
                        pending.insert(call.id().as_str());
                        match self.family {
                            Family::Anthropic => {
                                if !call.arguments().is_object() {
                                    return Err(format!(
                                        "message {index}: Anthropic tool use requires object arguments"
                                    ));
                                }
                                content.push(json!({
                                    "type": "tool_use",
                                    "id": call.id().as_str(),
                                    "name": call.name(),
                                    "input": call.arguments()
                                }));
                            }
                            Family::OpenAiCompatible => openai_calls.push(json!({
                                "type": "function",
                                "id": call.id().as_str(),
                                "function": {
                                    "name": call.name(),
                                    "arguments": call.arguments().to_string()
                                }
                            })),
                        }
                    }
                    ContentPart::ToolResult(result) => {
                        if !pending.remove(result.id().as_str()) {
                            return Err(format!(
                                "message {index}: tool result does not immediately follow its call"
                            ));
                        }
                        match self.family {
                            Family::Anthropic => {
                                let encoded =
                                    serde_json::to_string(result.data()).map_err(|_| {
                                        format!("message {index}: tool result JSON encoding failed")
                                    })?;
                                content.push(json!({
                                    "type": "tool_result",
                                    "tool_use_id": result.id().as_str(),
                                    "content": encoded,
                                    "is_error": result.is_error()
                                }));
                            }
                            Family::OpenAiCompatible => history.messages.push(json!({
                                "role": "tool",
                                "tool_call_id": result.id().as_str(),
                                "content": json!({
                                    "data": result.data(),
                                    "is_error": result.is_error()
                                }).to_string()
                            })),
                        }
                    }
                    _ => {
                        return Err(format!("message {index}: unsupported SDK content part"));
                    }
                }
            }
            match (self.family, role) {
                (Family::Anthropic, TurnRole::System) => history.system.extend(content),
                (Family::Anthropic, TurnRole::User | TurnRole::Assistant) => {
                    history.anthropic_message(role, content);
                }
                (
                    Family::OpenAiCompatible,
                    TurnRole::System | TurnRole::User | TurnRole::Assistant,
                ) => {
                    if !openai_calls.is_empty() {
                        let mut encoded = json!({"role": "assistant", "tool_calls": openai_calls});
                        if self.provider == "deepseek" || self.model.contains("deepseek") {
                            encoded["reasoning_content"] = json!("");
                        }
                        if !content.is_empty() {
                            encoded["content"] = Value::Array(content);
                        }
                        history.messages.push(encoded);
                    } else if !content.is_empty() {
                        history
                            .messages
                            .push(json!({"role": role.to_string(), "content": content}));
                    }
                }
                _ => return Err(format!("message {index}: unsupported SDK message role")),
            }
        }
        if !pending.is_empty() {
            return Err("conversation ends with unanswered tool calls".to_owned());
        }
        if !saw_user {
            return Err("conversation requires user-side content".to_owned());
        }
        Ok(history)
    }

    fn text_block(&self, text: impl AsRef<str>, index: usize) -> Result<Value, String> {
        if self.family == Family::Anthropic && text.as_ref().is_empty() {
            return Err(format!(
                "message {index}: Anthropic text blocks must be nonempty"
            ));
        }
        Ok(text_block(text.as_ref()))
    }
}

#[derive(Default)]
struct EncodedHistory {
    system: Vec<Value>,
    messages: Vec<Value>,
}

impl EncodedHistory {
    fn anthropic_message(&mut self, role: TurnRole, content: Vec<Value>) {
        // Anthropic combines adjacent messages with the same role. Explicitly combine only
        // adjacent user messages so split results arrive together, before other user content.
        let role = role.to_string();
        if role == "user"
            && let Some(previous) = self.messages.last_mut()
            && previous.get("role").and_then(Value::as_str) == Some(&role)
            && let Some(blocks) = previous.get_mut("content").and_then(Value::as_array_mut)
        {
            blocks.extend(content);
        } else {
            self.messages
                .push(json!({"role": role, "content": content}));
        }
    }
}

fn text_block(text: impl AsRef<str>) -> Value {
    json!({"type": "text", "text": text.as_ref()})
}
