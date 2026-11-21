// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native one-message assistant composition. Only the user's text is sent to the selected provider.
use erp_addon_ai_base::{config::Credentials, ledger, routing};
use erp_fields::{Error, Result, Value};
use erp_orm::{BoxFuture, CompanyId, Context, CredentialOptions};
use erp_sdk::ai::{ContentPart, ToolTurnRequest, TurnMessage, TurnRole, UserText};
use erp_security::auth::{Auth, SessionSecret};
use erp_server::web::WebAssistant;
use serde_json::{Value as Json, json};
use std::sync::Arc;

/// Site-bound credential registry plus a node admission semaphore. Per-session and per-user
/// admission and spending limits are coordinated by PostgreSQL across nodes.
pub struct TextAssistant {
    credentials: Credentials,
    slots: tokio::sync::Semaphore,
}
impl TextAssistant {
    /// Construct once at server startup from protected node configuration.
    pub fn new(credentials: Credentials) -> Self {
        let concurrent = credentials.max_concurrent_turns();
        Self {
            credentials,
            slots: tokio::sync::Semaphore::new(concurrent),
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
            let route=routing::resolve(&env,&mut begun.tx,&self.credentials).await.map_err(|error|{
                if error.message().starts_with("erp.ai.route_missing") {Error::user("erp.ai.route_missing: configure an assistant route for this company.")}
                else if error.message().contains("erp.ai.provider_unconfigured") || error.message().contains("credential") {Error::user("erp.ai.provider_unconfigured: ask an administrator to configure the selected provider.")}
                else {error}
            })?;
            let transport = route.transport(&self.credentials).map_err(|_| {
                Error::user(
                    "erp.ai.provider_unconfigured: the provider is unavailable for this session.",
                )
            })?;
            let invalid =
                |_| Error::validation("erp.ai.message_invalid: could not prepare this message.");
            let msg = TurnMessage::new(
                TurnRole::User,
                vec![ContentPart::UserText(UserText::new(message))],
            )
            .map_err(invalid)?;
            let req = ToolTurnRequest::new(route.model(), vec![msg], vec![])
                .map_err(invalid)?
                .with_max_tokens(max_tokens);
            // Bound the input reservation by the entire serialized provider payload, including its framing.
            let encoded = route
                .request_encoder()
                .and_then(|encoder| encoder.encode(&req))
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
            let result = transport.complete_turn(&req).await;
            let usage = result.as_ref().ok().map(|r| {
                (
                    i64::from(r.usage().prompt_tokens),
                    i64::from(r.usage().completion_tokens),
                )
            });
            // Accounting must complete even if the browser session was concurrently revoked.
            // The native reservation owns its ID; no target row or user comes from the client.
            // Only settle the already-returned provider response inside the ORM's bounded
            // serialization/deadlock retry. The external exchange is never replayed.
            auth.orm()
                .run(user, Context::new(), async move |tx| {
                    let reservation = reserved;
                    let actual = usage;
                    reservation.settle(tx, actual).await
                })
                .await?;
            let reply = result.map_err(|_| {
                Error::user(
                    "erp.ai.provider_failed: the selected provider could not complete the request.",
                )
            })?;
            let mut check = auth.begin_session(secret, options()).await?;
            let current = check.tx.env();
            if !check.tx.has_group(&current, "base.group_user").await? {
                return Err(Error::access("An internal session is required."));
            }
            check.tx.rollback().await?;
            let mut text = String::new();
            for part in reply.parts() {
                if let ContentPart::AssistantText(chunk) = part {
                    text.push_str(chunk);
                } else {
                    return Err(Error::user(
                        "erp.ai.provider_failed: the provider returned an unsupported reply.",
                    ));
                }
            }
            if text.trim().is_empty() {
                return Err(Error::user(
                    "erp.ai.provider_failed: the provider returned no text.",
                ));
            }
            Ok(
                json!({"text":text,"model":reply.usage().model,"usage":{"input_tokens":reply.usage().prompt_tokens,"output_tokens":reply.usage().completion_tokens}}),
            )
        })
    }
}
