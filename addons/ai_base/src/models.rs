// SPDX-License-Identifier: MIT OR Apache-2.0
//! Administrator configuration; keys and key paths are deliberately absent from every model.
use erp_sdk::prelude::*;
use std::collections::BTreeSet;

/// Globally shared provider metadata. The name maps to node-local credential configuration.
#[model(name = "ai.provider", description = "AI Provider", order = "name, id", constraints = [("_name_unique", "UNIQUE(name)", "AI provider names must be unique.")])]
pub struct AiProvider {
    /// Stable credential configuration name, never a key or a path.
    #[field(char, required = true, size = 63)]
    pub name: Option<Arc<str>>,
    /// Provider protocol family; a local model uses the compatible protocol.
    #[field(selection, required = true, default = "openai_compatible", selection = [("anthropic", "Anthropic"), ("openai_compatible", "OpenAI compatible")])]
    pub family: Option<Arc<str>>,
    /// Operator-controlled endpoint; HTTP allows locally hosted inference.
    #[field(char, required = true, size = 8192)]
    pub base_url: Option<Arc<str>>,
    /// Explicit model identifiers admitted for routing, as a JSON array of unique strings.
    #[field(json, required = true)]
    pub allowed_models: Option<serde_json::Value>,
    /// Operator-selected maximum number of allowed model identifiers, 1..=1024.
    #[field(integer, required = true, default = 128)]
    pub model_limit: i32,
    /// Operator-recorded processing region; not inferred from endpoint geography.
    #[field(char, size = 256)]
    pub region: Option<Arc<str>>,
    /// Operator-recorded provider policy; this flag does not verify a provider contract.
    #[field(boolean, default = false)]
    pub no_training: bool,
    /// Reference to the applicable processing terms; never fetched by this addon.
    #[field(char, size = 2048)]
    pub terms_reference: Option<Arc<str>>,
    /// Explicit activation; the shipped local provider is disabled until configured.
    #[field(boolean, default = false)]
    pub active: bool,
}

/// A single explicit model for the assistant in one exact company, without fallback.
#[model(name = "ai.route", description = "AI Route", order = "company_id, task_kind, id", company_access = direct(field = "company_id", branch = "exact", empty = "deny"), constraints = [("_company_task_unique", "UNIQUE(company_id, task_kind)", "An AI task has exactly one route per company.")])]
pub struct AiRoute {
    /// Operator-controlled display label.
    #[field(char, required = true, size = 256)]
    pub name: Option<Arc<str>>,
    /// Required exact owning company; company policy also guards explicit id access.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        index = true,
        ondelete = "cascade"
    )]
    pub company_id: Option<RecId>,
    /// Only the interactive assistant is implemented; later tasks extend the selection.
    #[field(selection, required = true, default = "assistant", selection = [("assistant", "Assistant")])]
    pub task_kind: Option<Arc<str>>,
    /// The only provider this route may select; deletion is refused while routed.
    #[field(
        many2one,
        comodel = "ai.provider",
        required = true,
        index = true,
        ondelete = "restrict"
    )]
    pub provider_id: Option<RecId>,
    /// Exact allowed model identifier, never selected implicitly by the provider adapter.
    #[field(char, required = true, size = 1024)]
    pub model: Option<Arc<str>>,
}

pub(crate) fn text(cell: Cell, field: &str) -> Result<String> {
    match cell {
        Cell::Str(value) => Ok(value.to_string()),
        // Required metadata must not silently become an empty/default string.
        _ => Err(Error::validation(format!("{field} must contain text"))),
    }
}
pub(crate) fn id(cell: Cell, field: &str) -> Result<i64> {
    match cell {
        Cell::Id(value) => Ok(value),
        _ => Err(Error::validation(format!(
            "{field} must reference a record"
        ))),
    }
}
pub(crate) fn model_names(cell: Cell, limit: i32) -> Result<BTreeSet<String>> {
    let invalid = || {
        Error::validation(
            "ai.provider.allowed_models must contain 1..=model_limit unique nonempty ASCII model identifiers of at most 1024 bytes",
        )
    };
    let Cell::Json(value) = cell else {
        return Err(invalid());
    };
    let Some(values) = value.as_array() else {
        return Err(invalid());
    };
    let limit = usize::try_from(limit).map_err(|_| invalid())?;
    if values.is_empty() || values.len() > limit {
        return Err(invalid());
    }
    let mut models = BTreeSet::new();
    for value in values {
        let Some(name) = value.as_str() else {
            return Err(invalid());
        };
        if name.is_empty()
            || name.len() > 1024
            || !name.bytes().all(|b| b.is_ascii_graphic())
            || !models.insert(name.to_owned())
        {
            return Err(invalid());
        }
    }
    Ok(models)
}
pub(crate) async fn allowed_models(
    provider: &Records,
    tx: &mut Transaction,
) -> Result<BTreeSet<String>> {
    let Cell::Int(limit) = provider.cell(tx, "model_limit").await? else {
        return Err(Error::validation(
            "ai.provider.model_limit must be in 1..=1024",
        ));
    };
    if !(1..=1024).contains(&limit) {
        return Err(Error::validation(
            "ai.provider.model_limit must be in 1..=1024",
        ));
    }
    model_names(provider.cell(tx, "allowed_models").await?, limit)
}

#[methods(AiProvider, addon = "ai_base")]
impl AiProvider {
    #[constrains("name", "base_url", "allowed_models", "model_limit")]
    async fn _check_provider(this: &RecordSet<AiProvider>, tx: &mut Transaction) -> Result<()> {
        for row in this.as_records().iter() {
            let name = text(row.cell(tx, "name").await?, "ai.provider.name")?;
            if name.is_empty()
                || name.len() > 63
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err(Error::validation(
                    "ai.provider.name must be 1..=63 ASCII letters, digits, underscores or hyphens",
                ));
            }
            let endpoint = text(row.cell(tx, "base_url").await?, "ai.provider.base_url")?;
            let invalid = || {
                Error::validation(
                    "ai.provider.base_url must be HTTP(S) with a host and without credentials, query or fragment",
                )
            };
            let url = url::Url::parse(&endpoint).map_err(|_| invalid())?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid());
            }
            allowed_models(&row, tx).await?;
        }
        Ok(())
    }
}
#[methods(AiRoute, addon = "ai_base")]
impl AiRoute {
    #[constrains("provider_id", "model")]
    async fn _check_model(this: &RecordSet<AiRoute>, tx: &mut Transaction) -> Result<()> {
        for row in this.as_records().iter() {
            let provider_id = id(row.cell(tx, "provider_id").await?, "ai.route.provider_id")?;
            let provider = this.env().records("ai.provider")?.browse([provider_id])?;
            let model = text(row.cell(tx, "model").await?, "ai.route.model")?;
            if !allowed_models(&provider, tx).await?.contains(&model) {
                return Err(Error::validation(
                    "ai.route.model must be in ai.provider.allowed_models",
                ));
            }
        }
        Ok(())
    }
}
