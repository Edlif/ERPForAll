// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit caller-company routing; ordinary model access and no provider fallback.
use crate::{
    config::Credentials,
    encoding::RequestEncoder,
    models::{allowed_models, id, text},
};
use erp_sdk::{ai::ProviderError, prelude::*};

/// Validated route metadata. It contains no credential bytes or protected file paths.
#[derive(Debug)]
#[non_exhaustive]
pub struct ConfiguredRoute {
    provider: String,
    model: String,
    family: String,
    base_url: String,
    request_bytes: usize,
    pub(crate) interactive: bool,
}
impl ConfiguredRoute {
    /// Stable configured provider name.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }
    /// Explicit model selected for the company.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
    /// Declared provider protocol family.
    #[must_use]
    pub fn family(&self) -> &str {
        &self.family
    }
    /// Creates a request encoder using this route's provider family and model.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] for an unsupported provider protocol
    /// and [`ProviderError::Unavailable`] when the operating system cannot supply randomness.
    pub fn request_encoder(&self) -> std::result::Result<RequestEncoder<'_>, ProviderError> {
        RequestEncoder::new(
            self.provider(),
            self.model(),
            self.family(),
            self.request_bytes,
        )
    }

    /// Administrator-selected API base. Transport also requires a matching node credential binding.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}
/// Resolve the route for `task_kind` in the caller's current company under ordinary
/// model/record access. Every resolution revalidates current provider activation, model
/// allowance and node credentials.
/// # Errors
/// Refuses a missing/ambiguous company route naming the task kind, unreadable
/// records, invalidated models and inactive or unconfigured providers. Never tries another
/// company, provider or model.
pub async fn resolve_task(
    env: &Env,
    tx: &mut Transaction,
    credentials: &Credentials,
    task_kind: &str,
) -> Result<ConfiguredRoute> {
    let company = env
        .company_id()?
        .ok_or_else(|| Error::validation("erp.ai.route_missing: current company is required"))?;
    let domain = Domain::condition("company_id", Operator::Eq, company)?
        & Domain::condition("task_kind", Operator::Eq, task_kind)?;
    let routes = env.records("ai.route")?.search(tx, &domain).await?;
    if routes.len() != 1 {
        return Err(Error::validation(format!(
            "erp.ai.route_missing: task '{task_kind}' company {company}"
        )));
    }
    let provider_id = id(
        routes.cell(tx, "provider_id").await?,
        "ai.route.provider_id",
    )?;
    let model = text(routes.cell(tx, "model").await?, "ai.route.model")?;
    let provider = env.records("ai.provider")?.browse([provider_id])?;
    let name = text(provider.cell(tx, "name").await?, "ai.provider.name")?;
    if provider.cell(tx, "active").await? != Cell::Bool(true) {
        return Err(Error::validation(format!(
            "erp.ai.provider_unconfigured: provider '{name}'"
        )));
    }
    credentials
        .require(&name)
        .map_err(|error| Error::validation(error.to_string()).with_source(error))?;
    if !allowed_models(&provider, tx).await?.contains(&model) {
        return Err(Error::validation(
            "ai.route.model must be in ai.provider.allowed_models",
        ));
    }
    Ok(ConfiguredRoute {
        provider: name,
        model,
        family: text(provider.cell(tx, "family").await?, "ai.provider.family")?,
        base_url: text(provider.cell(tx, "base_url").await?, "ai.provider.base_url")?,
        request_bytes: credentials.request_bytes(),
        interactive: crate::ordinary_user(env),
    })
}

/// Resolve the interactive assistant route; see [`resolve_task`].
///
/// # Errors
/// As [`resolve_task`], for the `assistant` task kind.
pub async fn resolve(
    env: &Env,
    tx: &mut Transaction,
    credentials: &Credentials,
) -> Result<ConfiguredRoute> {
    resolve_task(env, tx, credentials, "assistant").await
}

#[cfg(test)]
mod response_tests;
