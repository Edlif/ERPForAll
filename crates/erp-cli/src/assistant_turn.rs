// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native assistant composition with multi-turn tool calling, live data aggregation, and canvas staging.
use crate::assistant_read;
use erp_addon_ai_base::{canvas, catalog, config::Credentials, ledger, routing};
use erp_fields::{Error, Result, Value};
use erp_orm::{BoxFuture, CompanyId, Context, CredentialOptions};
use erp_rpc::RpcService;
use erp_sdk::ai::{
    ContentPart, DataSource, Instruction, LabelledData, ToolName, ToolResult, ToolTurnRequest,
    TurnMessage, TurnRole, UserText,
};
use erp_security::auth::{Auth, SessionSecret};
use erp_server::web::WebAssistant;
use serde_json::{Value as Json, json};
use std::sync::Arc;

const ASSISTANT_SYSTEM_PROMPT: &str = "\
You are ERPForAll Assistant, an intelligent ERP agent. \
You have access to live ERP tools: \
- `find_models`: search installed business models by query or description. \
- `search_records`: read detailed records of a model with domain filter and field list. \
- `aggregate_records`: group and compute aggregates (sums, counts) on a model, such as ranking customer sales, monthly revenues, or invoice balances. \
- `render_canvas`: render interactive visual cards, dashboards, charts, tables, or workflow diagrams on the user's workspace canvas. \
Always use `aggregate_records` or `search_records` to verify live facts before answering questions about business data. \
When the user asks for a chart, graph, table, or visual workflow, call `render_canvas` with appropriate visual specifications.";

struct AssistantFallbackHost;
impl erp_rpc::RpcHost for AssistantFallbackHost {
    fn language_active(&self, code: &str) -> Result<bool> {
        Ok(code == "en_US" || code == "id_ID")
    }
    fn has_capability(&self, _: &erp_orm::Principal, _: &erp_orm::Capability) -> Result<bool> {
        Ok(false)
    }
}

/// Site-bound credential registry plus a node admission semaphore. Per-session and per-user
/// admission and spending limits are coordinated by PostgreSQL across nodes.
pub struct TextAssistant {
    credentials: Credentials,
    slots: tokio::sync::Semaphore,
    rpc: std::sync::RwLock<Option<Arc<RpcService>>>,
}

impl TextAssistant {
    /// Construct once at server startup from protected node configuration.
    pub fn new(credentials: Credentials) -> Self {
        let concurrent = credentials.max_concurrent_turns();
        Self {
            credentials,
            slots: tokio::sync::Semaphore::new(concurrent),
            rpc: std::sync::RwLock::new(None),
        }
    }

    /// Supply the runtime RPC service for tool dispatch.
    pub fn with_rpc(self, rpc: Arc<RpcService>) -> Self {
        if let Ok(mut guard) = self.rpc.write() {
            *guard = Some(rpc);
        }
        self
    }

    /// Set or update the runtime RPC service.
    pub fn set_rpc(&self, rpc: Arc<RpcService>) {
        if let Ok(mut guard) = self.rpc.write() {
            *guard = Some(rpc);
        }
    }
}

impl WebAssistant for TextAssistant {
    fn tab_history<'a>(&'a self, tx: &'a mut erp_orm::Transaction) -> BoxFuture<'a, Result<Json>> {
        Box::pin(ledger::tab_history(tx))
    }

    fn turn<'a>(
        &'a self,
        auth: &'a Arc<Auth>,
        secret: &'a SessionSecret,
        company: CompanyId,
        message: String,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(async move {
            let _slot = self.slots.try_acquire().map_err(|_| {
                Error::user("erp.ai.busy: the assistant is busy. Try again shortly.")
            })?;
            let options = || {
                CredentialOptions::new().context(Context::from_pairs([(
                    "allowed_company_ids",
                    Value::list([Value::Int(company.get())]),
                )]))
            };
            let mut begun = auth.begin_session(secret, options()).await?;
            let env = begun.tx.env();
            if !begun.tx.has_group(&env, "base.group_user").await?
                || env.principal().actor().is_some()
                || env.is_superuser()
            {
                return Err(Error::access(
                    "An ordinary internal user session is required.",
                ));
            }
            let (max_bytes, max_tokens) = ledger::limits(&mut begun.tx).await?;
            if message.trim().is_empty() || message.len() > max_bytes {
                return Err(Error::validation(
                    "erp.ai.message_invalid: enter a message within the configured length limit.",
                ));
            }
            let route = routing::resolve(&env, &mut begun.tx, &self.credentials)
                .await
                .map_err(|error| {
                    if error.message().starts_with("erp.ai.route_missing") {
                        Error::user("erp.ai.route_missing: configure an assistant route for this company.")
                    } else if error.message().contains("erp.ai.provider_unconfigured")
                        || error.message().contains("credential")
                    {
                        Error::user("erp.ai.provider_unconfigured: ask an administrator to configure the selected provider.")
                    } else {
                        error
                    }
                })?;
            let transport = route.transport(&self.credentials).map_err(|_| {
                Error::user(
                    "erp.ai.provider_unconfigured: the provider is unavailable for this session.",
                )
            })?;
            let invalid =
                |_| Error::validation("erp.ai.message_invalid: could not prepare this message.");

            let mut tools = Vec::new();
            if let Ok(spec) = catalog::find_models_spec() {
                tools.push(spec);
            }
            if let Ok(spec) = erp_addon_ai_base::search::search_records_spec() {
                tools.push(spec);
            }
            if let Ok(spec) = erp_addon_ai_base::aggregate::aggregate_records_spec() {
                tools.push(spec);
            }
            if let Ok(spec) = canvas::render_canvas_spec() {
                tools.push(spec);
            }

            let sys_msg = TurnMessage::new(
                TurnRole::System,
                vec![ContentPart::Instruction(Instruction::from_static(
                    ASSISTANT_SYSTEM_PROMPT,
                ))],
            )
            .map_err(invalid)?;
            let user_msg = TurnMessage::new(
                TurnRole::User,
                vec![ContentPart::UserText(UserText::new(message))],
            )
            .map_err(invalid)?;
            let mut messages = vec![sys_msg, user_msg];

            let initial_req = ToolTurnRequest::new(route.model(), messages.clone(), tools.clone())
                .map_err(invalid)?
                .with_max_tokens(max_tokens);

            // Bound the input reservation by the entire serialized provider payload, including its framing.
            let encoded = route
                .request_encoder()
                .and_then(|encoder| encoder.encode(&initial_req))
                .map_err(|_| {
                    Error::validation(
                        "erp.ai.message_invalid: message exceeds the provider request limit.",
                    )
                })?;
            let estimated = i64::try_from(
                serde_json::to_vec(&encoded)
                    .map_err(|_| Error::internal("Could not encode the provider request."))?
                    .len(),
            )
            .map_err(|_| Error::validation("erp.ai.message_invalid: message is too large."))?;
            let timeout = self.credentials.exchange_timeout();
            let expiry = i32::try_from(timeout.as_secs().saturating_mul(2).saturating_add(2))
                .map_err(|_| Error::internal("Invalid assistant timeout."))?;
            let reserved = ledger::reserve(
                &mut begun.tx,
                &begun.session.id().to_text(),
                &route,
                estimated,
                max_tokens,
                expiry,
            )
            .await?;
            let user = env.user_id();
            begun.tx.commit().await?;
            drop(begun);

            let service = self
                .rpc
                .read()
                .ok()
                .and_then(|guard| guard.clone())
                .unwrap_or_else(|| {
                    Arc::new(RpcService::new(
                        auth.orm().clone(),
                        AssistantFallbackHost,
                        erp_rpc::RpcConfig::default(),
                    ))
                });

            let secret_arc = Arc::new(
                SessionSecret::from_cookie(&secret.to_cookie())
                    .ok_or_else(|| Error::internal("Invalid session secret."))?,
            );
            let mut total_prompt_tokens: u64 = 0;
            let mut total_completion_tokens: u64 = 0;
            let mut last_model = route.model().to_string();
            let mut text = String::new();
            let mut staged_canvas: Option<Json> = None;

            const MAX_TURNS: usize = 6;
            for _turn_idx in 0..MAX_TURNS {
                let req = ToolTurnRequest::new(route.model(), messages.clone(), tools.clone())
                    .map_err(invalid)?
                    .with_max_tokens(max_tokens);

                let result = transport.complete_turn(&req).await;
                let reply = match result {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = auth
                            .orm()
                            .run(user, Context::new(), async move |tx| {
                                reserved.settle(tx, None).await
                            })
                            .await;
                        return Err(Error::user(format!("erp.ai.provider_failed: {e}")));
                    }
                };

                total_prompt_tokens =
                    total_prompt_tokens.saturating_add(u64::from(reply.usage().prompt_tokens));
                total_completion_tokens = total_completion_tokens
                    .saturating_add(u64::from(reply.usage().completion_tokens));
                last_model = reply.usage().model.clone();

                messages.push(
                    TurnMessage::new(TurnRole::Assistant, reply.parts().to_vec())
                        .map_err(invalid)?,
                );

                let mut tool_calls = Vec::new();
                for part in reply.parts() {
                    match part {
                        ContentPart::AssistantText(chunk) => {
                            if !text.is_empty() && !text.ends_with('\n') && !text.ends_with(' ') {
                                text.push('\n');
                            }
                            text.push_str(chunk);
                            if text.len() > 65536 {
                                text.truncate(65536);
                            }
                        }
                        ContentPart::ToolCall(call) => {
                            tool_calls.push(call.clone());
                        }
                        _ => {}
                    }
                }

                if tool_calls.is_empty() {
                    break;
                }

                const MAX_TOOL_CALLS_PER_TURN: usize = 8;
                if tool_calls.len() > MAX_TOOL_CALLS_PER_TURN {
                    tool_calls.truncate(MAX_TOOL_CALLS_PER_TURN);
                }

                let mut tool_results = Vec::new();
                for call in tool_calls {
                    let tool_result = match call.name() {
                        "find_models" => {
                            let mut tx = auth.begin_session(secret, options()).await?.tx;
                            let r = catalog::find_models(&mut tx, &call).await;
                            tx.rollback().await.ok();
                            r
                        }
                        "search_records" => {
                            assistant_read::search_records(
                                &service,
                                auth.clone(),
                                secret_arc.clone(),
                                company,
                                &call,
                            )
                            .await
                        }
                        "aggregate_records" => {
                            assistant_read::aggregate_records(
                                &service,
                                auth.clone(),
                                secret_arc.clone(),
                                company,
                                &call,
                            )
                            .await
                        }
                        "render_canvas" => {
                            let res = canvas::render_canvas(&call);
                            if res.is_ok()
                                && let Ok(payload) = canvas::extract_canvas_payload(&call)
                            {
                                staged_canvas = Some(payload);
                            }
                            res
                        }
                        other => Err(Error::validation(format!(
                            "Unknown assistant tool '{other}'."
                        ))),
                    };

                    match tool_result {
                        Ok(result) => {
                            tool_results.push(ContentPart::ToolResult(result));
                        }
                        Err(err) => {
                            let tool_name = ToolName::new(call.name())
                                .or_else(|_| ToolName::new("unknown"))
                                .map_err(|e| Error::programming(e.to_string()))?;
                            let source = DataSource::new(tool_name, None, Vec::new())
                                .map_err(|e| Error::programming(e.to_string()))?;
                            let err_result = ToolResult::new(
                                call.id().clone(),
                                LabelledData::new(
                                    source,
                                    json!({
                                        "error": err.message()
                                    }),
                                ),
                                true,
                            );
                            tool_results.push(ContentPart::ToolResult(err_result));
                        }
                    }
                }

                messages.push(TurnMessage::new(TurnRole::User, tool_results).map_err(invalid)?);
            }

            let usage = Some((
                i64::try_from(total_prompt_tokens).unwrap_or(i64::MAX),
                i64::try_from(total_completion_tokens).unwrap_or(i64::MAX),
            ));

            auth.orm()
                .run(user, Context::new(), async move |tx| {
                    let reservation = reserved;
                    reservation.settle(tx, usage).await
                })
                .await?;

            let mut check = auth.begin_session(secret, options()).await?;
            let current = check.tx.env();
            if !check.tx.has_group(&current, "base.group_user").await? {
                return Err(Error::access("An internal session is required."));
            }
            check.tx.rollback().await?;

            if text.trim().is_empty() && staged_canvas.is_none() {
                return Err(Error::user(
                    "erp.ai.provider_failed: the provider returned no text or canvas.",
                ));
            }

            let mut resp = json!({
                "text": text,
                "model": last_model,
                "usage": {
                    "input_tokens": total_prompt_tokens,
                    "output_tokens": total_completion_tokens
                }
            });
            if let Some(canvas) = staged_canvas {
                resp["canvas"] = canvas;
            }

            Ok(resp)
        })
    }
}
