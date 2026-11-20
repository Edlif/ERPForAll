// SPDX-License-Identifier: MIT OR Apache-2.0

use erp_sdk::ai::{
    ContentPart, FinishReason, ToolCall, ToolCallId, ToolTurnResponse, ToolTypeError, UsageRecord,
};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};

use super::{Family, arguments::parse_arguments};

// Hard decoder admission bound. A transport must enforce its own body limit while reading.
const MAX_BODY_BYTES: usize = 1024 * 1024;

pub(super) fn decode(
    family: Family,
    bytes: &[u8],
    provider: &str,
) -> Result<ToolTurnResponse, &'static str> {
    if bytes.len() > MAX_BODY_BYTES {
        return Err("response exceeds the decoder's maximum body size");
    }
    match family {
        Family::Anthropic => {
            let reply: AnthropicReply = serde_json::from_slice(bytes)
                .map_err(|_| "invalid Anthropic response JSON or fields")?;
            reply.decode(provider)
        }
        Family::OpenAiCompatible => {
            let reply: OpenAiReply = serde_json::from_slice(bytes)
                .map_err(|_| "invalid OpenAI response JSON or fields")?;
            reply.decode(provider)
        }
    }
}

#[derive(Deserialize)]
struct AnthropicReply {
    #[serde(rename = "type")]
    kind: String,
    role: String,
    model: String,
    content: Vec<AnthropicBlock>,
    stop_reason: String,
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    // This must deserialize directly from the wire, before internally tagged enum
    // buffering or Value conversion can erase the original key occurrences.
    input: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
}

impl AnthropicReply {
    fn decode(self, provider: &str) -> Result<ToolTurnResponse, &'static str> {
        if self.kind != "message" || self.role != "assistant" {
            return Err("Anthropic response must be an assistant message");
        }
        let reason = match self.stop_reason.as_str() {
            "end_turn" | "stop_sequence" => FinishReason::Stop,
            "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
            "refusal" => FinishReason::ContentFilter,
            "tool_use" => FinishReason::ToolUse,
            _ => return Err("unsupported Anthropic stop reason (AI1.BASE-ADAPTERS)"),
        };
        let mut parts = Vec::with_capacity(self.content.len());
        for block in self.content {
            match block.kind.as_str() {
                "text" => parts.push(assistant_text(
                    block.text.ok_or("Anthropic text block has no text")?,
                )?),
                "tool_use" => {
                    let id = block.id.ok_or("Anthropic tool block has no ID")?;
                    let name = block.name.ok_or("Anthropic tool block has no name")?;
                    let invalid = "Anthropic tool input must contain one JSON object";
                    let raw = block.input.ok_or(invalid)?;
                    let input = parse_arguments(raw.get(), invalid)?;
                    if !input.is_object() {
                        return Err(invalid);
                    }
                    parts.push(tool_call(&id, name, input)?);
                }
                _ => return Err("unsupported Anthropic content block (AI1.BASE-ADAPTERS)"),
            }
        }
        // The provider reports cache reads and writes outside input_tokens.
        let prompt_tokens = self
            .usage
            .input_tokens
            .checked_add(self.usage.cache_creation_input_tokens.unwrap_or_default())
            .and_then(|tokens| {
                tokens.checked_add(self.usage.cache_read_input_tokens.unwrap_or_default())
            })
            .ok_or("Anthropic input token count exceeds the supported range")?;
        make_response(
            provider,
            self.model,
            parts,
            reason,
            prompt_tokens,
            self.usage.output_tokens,
        )
    }
}

#[derive(Deserialize)]
struct OpenAiReply {
    object: String,
    model: String,
    choices: Vec<OpenAiChoice>,
    usage: OpenAiUsage,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    index: u32,
    message: OpenAiMessage,
    finish_reason: String,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    role: String,
    content: Option<String>,
    refusal: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCall>>,
    function_call: Option<Value>,
    audio: Option<Value>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiToolCall {
    Function {
        id: String,
        function: OpenAiFunction,
    },
    #[serde(other)]
    Unsupported,
}

#[derive(Deserialize)]
struct OpenAiFunction {
    name: String,
    arguments: String,
}

impl OpenAiReply {
    fn decode(self, provider: &str) -> Result<ToolTurnResponse, &'static str> {
        if self.object != "chat.completion" {
            return Err("OpenAI response must be a complete chat completion");
        }
        let [choice]: [OpenAiChoice; 1] = self
            .choices
            .try_into()
            .map_err(|_| "OpenAI response must contain exactly one choice")?;
        if choice.index != 0 || choice.message.role != "assistant" {
            return Err("OpenAI choice must be assistant choice zero");
        }
        let usage = self.usage;
        if usage
            .prompt_tokens
            .checked_add(usage.completion_tokens)
            .ok_or("total token count exceeds the supported range")?
            != usage.total_tokens
        {
            return Err("OpenAI total token count does not match prompt and completion tokens");
        }
        let (parts, reason) = choice.decode()?;
        make_response(
            provider,
            self.model,
            parts,
            reason,
            usage.prompt_tokens,
            usage.completion_tokens,
        )
    }
}

impl OpenAiChoice {
    fn decode(self) -> Result<(Vec<ContentPart>, FinishReason), &'static str> {
        let msg = self.message;
        if msg.function_call.is_some() || msg.audio.is_some() {
            return Err("unsupported OpenAI response content (AI1.BASE-ADAPTERS)");
        }
        let mut reason = match self.finish_reason.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "content_filter" => FinishReason::ContentFilter,
            "tool_calls" => FinishReason::ToolUse,
            _ => return Err("unsupported OpenAI finish reason (AI1.BASE-ADAPTERS)"),
        };
        let mut parts = Vec::new();
        let calls = msg.tool_calls.unwrap_or_default();
        if let Some(refusal) = msg.refusal {
            if msg.content.as_ref().is_some_and(|text| !text.is_empty())
                || !calls.is_empty()
                || !matches!(reason, FinishReason::Stop | FinishReason::ContentFilter)
            {
                return Err("OpenAI refusal conflicts with the response content or finish reason");
            }
            reason = FinishReason::ContentFilter;
            parts.push(assistant_text(refusal)?);
        } else if let Some(text) = msg.content.filter(|text| !text.is_empty()) {
            parts.push(assistant_text(text)?);
        }
        for call in calls {
            let OpenAiToolCall::Function { id, function } = call else {
                return Err("unsupported OpenAI tool call type (AI1.BASE-ADAPTERS)");
            };
            let arguments = parse_arguments(
                &function.arguments,
                "OpenAI tool call arguments must contain one JSON value",
            )?;
            parts.push(tool_call(&id, function.name, arguments)?);
        }
        Ok((parts, reason))
    }
}

fn make_response(
    provider: &str,
    model: String,
    parts: Vec<ContentPart>,
    reason: FinishReason,
    input: u32,
    output: u32,
) -> Result<ToolTurnResponse, &'static str> {
    if model.trim().is_empty() {
        return Err("assistant response model is empty");
    }
    let has_calls = parts
        .iter()
        .any(|part| matches!(part, ContentPart::ToolCall(_)));
    if has_calls != (reason == FinishReason::ToolUse) {
        return Err("tool calls and finish reason disagree");
    }
    input
        .checked_add(output)
        .ok_or("total token count exceeds the supported range")?;
    let usage = UsageRecord::new(provider, &model, input, output);
    ToolTurnResponse::new(model, parts, usage, Some(reason)).map_err(|err| match err {
        ToolTypeError::DuplicateResponseToolCallId { .. } => {
            "assistant response repeats a tool call ID"
        }
        ToolTypeError::EmptyResponse => "assistant response contains no representable content",
        _ => "invalid assistant response parts",
    })
}

fn assistant_text(text: String) -> Result<ContentPart, &'static str> {
    if text.is_empty() {
        return Err("assistant response contains an empty text block");
    }
    Ok(ContentPart::AssistantText(text))
}

fn tool_call(id: &str, name: String, arguments: Value) -> Result<ContentPart, &'static str> {
    let id = ToolCallId::new(id).map_err(|_| "invalid tool call ID")?;
    if name.trim().is_empty() {
        return Err("tool call name is empty");
    }
    let call = ToolCall::new(id, name, arguments).map_err(|_| "invalid tool call name")?;
    Ok(ContentPart::ToolCall(call))
}
