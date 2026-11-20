// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use erp_sdk::ai::{
    ContentPart, DataSource, FinishReason, LabelledData, ToolName, ToolResult, ToolSpec,
    ToolTurnRequest, TurnMessage, TurnRole, UserText,
};
use serde_json::{Value, json};

const MODEL: &str = "example-model";
const PROVIDER: &str = "example-provider";

const ANTHROPIC_REPLY: &str = r#"{
    "id": "msg_example_09",
    "type": "message",
    "role": "assistant",
    "model": "example-model-snapshot",
    "content": [
        {"type": "text", "text": "I will check café stock."},
        {"type": "tool_use", "id": "toolu_example_09", "name": "check_stock", "input": {"sku":"A-7"}}
    ],
    "stop_reason": "tool_use",
    "usage": {"input_tokens": 8, "cache_creation_input_tokens": 3, "cache_read_input_tokens": 2, "output_tokens": 6}
}"#;

const OPENAI_REPLY: &str = r#"{
    "id": "chatcmpl-example-09",
    "object": "chat.completion",
    "model": "example-model-snapshot",
    "choices": [{
        "index": 0,
        "message": {
            "role": "assistant",
            "content": null,
            "refusal": null,
            "tool_calls": [{
                "type": "function", "id": "call_example_09",
                "function": {"name": "check_stock", "arguments": "{\"sku\":\"A-7\"}"}
            }]
        },
        "finish_reason": "tool_calls"
    }],
    "usage": {"prompt_tokens": 13, "completion_tokens": 6, "total_tokens": 19, "prompt_tokens_details": {"cached_tokens": 2}}
}"#;

fn route(family: &str) -> ConfiguredRoute {
    ConfiguredRoute {
        provider: PROVIDER.to_owned(),
        family: family.to_owned(),
        model: MODEL.to_owned(),
        base_url: "https://provider.example.invalid".to_owned(),
        request_bytes: crate::config::DEFAULT_REQUEST_BYTES,
    }
}

fn user() -> TurnMessage {
    TurnMessage::new(
        TurnRole::User,
        vec![ContentPart::UserText(UserText::new("Find A-7.".to_owned()))],
    )
    .unwrap()
}

fn request(messages: Vec<TurnMessage>) -> ToolTurnRequest {
    let Value::Object(schema) = json!({
        "type": "object", "additionalProperties": false, "required": ["sku"],
        "properties": {"sku": {"type": "string"}}
    }) else {
        unreachable!("literal schema is an object");
    };
    let tool = ToolSpec::new(
        ToolName::new("check_stock").unwrap(),
        "Check a sample SKU.",
        schema,
    )
    .unwrap();
    ToolTurnRequest::new(MODEL, messages, vec![tool])
        .unwrap()
        .with_max_tokens(384)
}

fn exchange(family: &str, body: &str, expected_id: &str) -> Value {
    let configured = route(family);
    let adapter = configured.request_encoder().unwrap();
    let first = adapter.encode(&request(vec![user()])).unwrap();
    assert_eq!(first["model"], MODEL);
    assert_eq!(first["messages"][0]["content"][0]["text"], "Find A-7.");

    let response = adapter.decode_response(body.as_bytes()).unwrap();
    assert_eq!(response.model(), "example-model-snapshot");
    assert_eq!(response.usage().model, "example-model-snapshot");
    assert_eq!(response.usage().provider, PROVIDER);
    assert_eq!(response.usage().prompt_tokens, 13);
    assert_eq!(response.usage().completion_tokens, 6);
    assert_eq!(response.usage().total_tokens, 19);
    assert_eq!(response.finish_reason(), Some(FinishReason::ToolUse));
    let Some(ContentPart::ToolCall(call)) = response.parts().last() else {
        panic!("expected a tool call");
    };
    assert_eq!(call.id().as_str(), expected_id);
    assert_eq!(call.name(), "check_stock");
    assert_eq!(call.arguments(), &json!({"sku": "A-7"}));
    let label = DataSource::new(ToolName::new("check_stock").unwrap(), None, vec![]).unwrap();
    let result = ToolResult::new(
        call.id().clone(),
        LabelledData::new(label, json!({"sample_units": 11})),
        false,
    );
    adapter
        .encode(&request(vec![
            user(),
            TurnMessage::new(TurnRole::Assistant, response.parts().to_vec()).unwrap(),
            TurnMessage::new(TurnRole::User, vec![ContentPart::ToolResult(result)]).unwrap(),
        ]))
        .unwrap()
}

#[test]
fn anthropic_offline_exchange_keeps_order_and_call_id() {
    let id = "toolu_example_09";
    let history = exchange("anthropic", ANTHROPIC_REPLY, id);
    assert_eq!(
        history["messages"][1]["content"][0]["text"],
        "I will check café stock."
    );
    assert_eq!(history["messages"][1]["content"][1]["id"], id);
    assert_eq!(history["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(history["messages"][2]["content"][0]["tool_use_id"], id);
}

#[test]
fn openai_offline_exchange_keeps_call_id() {
    let id = "call_example_09";
    let history = exchange("openai_compatible", OPENAI_REPLY, id);
    assert_eq!(history["messages"][1]["tool_calls"][0]["id"], id);
    assert_eq!(history["messages"][2]["role"], "tool");
    assert_eq!(history["messages"][2]["tool_call_id"], id);
}

fn changed(body: &str, change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut value = serde_json::from_str(body).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn invalid(family: &str, body: &[u8], message: &str) {
    assert_eq!(
        route(family)
            .request_encoder()
            .unwrap()
            .decode_response(body)
            .unwrap_err(),
        ProviderError::InvalidResponse {
            provider: PROVIDER.to_owned(),
            message: message.to_owned(),
        }
    );
}

#[test]
fn malformed_unsupported_and_incomplete_replies_fail() {
    invalid(
        "anthropic",
        b"",
        "invalid Anthropic response JSON or fields",
    );
    invalid(
        "anthropic",
        br#"{"message":"PRIVATE-SAMPLE"}"#,
        "invalid Anthropic response JSON or fields",
    );
    invalid(
        "anthropic",
        &changed(ANTHROPIC_REPLY, |body| {
            body["content"][0]["type"] = json!("thinking")
        }),
        "unsupported Anthropic content block (AI1.BASE-ADAPTERS)",
    );
    invalid(
        "anthropic",
        &changed(ANTHROPIC_REPLY, |body| {
            body["stop_reason"] = json!("pause_turn")
        }),
        "unsupported Anthropic stop reason (AI1.BASE-ADAPTERS)",
    );
    invalid(
        "openai_compatible",
        &changed(OPENAI_REPLY, |body| {
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
                json!("PRIVATE-SAMPLE");
        }),
        "OpenAI tool call arguments must contain one JSON value",
    );
    invalid(
        "openai_compatible",
        &changed(OPENAI_REPLY, |body| {
            body["choices"][0]["finish_reason"] = json!("length")
        }),
        "tool calls and finish reason disagree",
    );
    invalid(
        "openai_compatible",
        &changed(OPENAI_REPLY, |body| body["choices"] = json!([])),
        "OpenAI response must contain exactly one choice",
    );
}

#[test]
fn usage_is_required_and_validated() {
    invalid(
        "anthropic",
        &changed(ANTHROPIC_REPLY, |body| {
            body["usage"]["input_tokens"] = json!(u32::MAX)
        }),
        "Anthropic input token count exceeds the supported range",
    );
    invalid(
        "openai_compatible",
        &changed(OPENAI_REPLY, |body| {
            body["usage"]["total_tokens"] = json!(0)
        }),
        "OpenAI total token count does not match prompt and completion tokens",
    );
    invalid(
        "openai_compatible",
        &changed(OPENAI_REPLY, |body| body["usage"] = Value::Null),
        "invalid OpenAI response JSON or fields",
    );
}

#[test]
fn invalid_and_duplicate_call_ids_fail_without_echoing_provider_values() {
    invalid(
        "anthropic",
        &changed(ANTHROPIC_REPLY, |body| body["content"][1]["id"] = json!("")),
        "invalid tool call ID",
    );
    invalid(
        "openai_compatible",
        &changed(OPENAI_REPLY, |body| {
            let calls = body["choices"][0]["message"]["tool_calls"]
                .as_array_mut()
                .unwrap();
            calls.push(calls[0].clone());
        }),
        "assistant response repeats a tool call ID",
    );
}

#[test]
fn openai_refusal_remains_visible_and_scalar_arguments_reach_the_executor_boundary() {
    let configured = route("openai_compatible");
    let adapter = configured.request_encoder().unwrap();
    let refusal = changed(OPENAI_REPLY, |body| {
        body["choices"][0]["finish_reason"] = json!("stop");
        body["choices"][0]["message"] = json!({"role": "assistant", "refusal": "Cannot answer."});
    });
    let response = adapter.decode_response(&refusal).unwrap();
    assert_eq!(response.finish_reason(), Some(FinishReason::ContentFilter));
    assert_eq!(
        response.parts(),
        &[ContentPart::AssistantText("Cannot answer.".to_owned())]
    );

    let scalar = changed(OPENAI_REPLY, |body| {
        body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] = json!("42");
    });
    let response = adapter.decode_response(&scalar).unwrap();
    let Some(ContentPart::ToolCall(call)) = response.parts().first() else {
        panic!("expected a tool call");
    };
    assert_eq!(call.arguments(), &json!(42));
}

#[test]
fn response_byte_and_json_depth_limits_fail_closed() {
    invalid(
        "anthropic",
        &vec![b' '; (1024 * 1024) + 1],
        "response exceeds the decoder's maximum body size",
    );
    let too_deep = format!("{}0{}", "[".repeat(140), "]".repeat(140));
    invalid(
        "openai_compatible",
        &changed(OPENAI_REPLY, |body| {
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
                json!(too_deep);
        }),
        "OpenAI tool call arguments must contain one JSON value",
    );
}

#[test]
fn raw_provider_arguments_reject_duplicate_keys_at_every_depth() {
    let anthropic = ANTHROPIC_REPLY.replace(
        r#"{"sku":"A-7"}"#,
        r#"{"nested":[{"sample":"A-7","\u0073ample":"B-8"}]}"#,
    );
    invalid(
        "anthropic",
        anthropic.as_bytes(),
        "tool call arguments contain a duplicate object key",
    );
    let openai = changed(OPENAI_REPLY, |body| {
        body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
            json!(r#"{"nested":[{"sample":"A-7","\u0073ample":"B-8"}]}"#);
    });
    invalid(
        "openai_compatible",
        &openai,
        "tool call arguments contain a duplicate object key",
    );
}

#[test]
fn node_provider_request_limit_loads_named_bounds() {
    let dir = tempfile::tempdir().unwrap();
    for (value, accepted) in [
        (65_535, false),
        (65_536, true),
        (4_194_304, true),
        (4_194_305, false),
    ] {
        let config = format!("max_request_bytes = {value}")
            .parse::<toml::Value>()
            .unwrap();
        match Credentials::load(Some(&config), dir.path()) {
            Ok(config) => {
                assert!(accepted);
                assert_eq!(config.request_bytes(), value);
            }
            Err(error) => {
                assert!(!accepted);
                assert!(matches!(
                    error,
                    crate::config::CredentialError::Limit {
                        setting: "ai.max_request_bytes",
                        range: "65536..=4194304"
                    }
                ));
                assert_eq!(
                    error.to_string(),
                    "ai.max_request_bytes must be in 65536..=4194304"
                );
            }
        }
    }
    let config = Credentials::load(None, dir.path()).unwrap();
    assert_eq!(config.request_bytes(), crate::config::DEFAULT_REQUEST_BYTES);
    let mut configured = route("openai_compatible");
    configured.request_bytes = 65_536;
    let encoder = configured.request_encoder().unwrap();
    let oversized = TurnMessage::new(
        TurnRole::User,
        vec![ContentPart::UserText(UserText::new("a".repeat(65_536)))],
    )
    .unwrap();
    assert_eq!(
        encoder.encode(&request(vec![oversized])).unwrap_err(),
        ProviderError::InvalidRequest {
            provider: PROVIDER.to_owned(),
            message: "request exceeds ai.max_request_bytes".to_owned()
        },
    );
}
