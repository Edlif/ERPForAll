// SPDX-License-Identifier: MIT OR Apache-2.0
//! Client action tags declared by native addons.
//!
//! An addon that ships a whole-screen client action declares its tag here, with a validator for the
//! parameters of `ir.actions.client.params`. The server serves an `ir.actions.client` (menu, direct open,
//! action result) only for a tag an installed addon declared; the browser binds the same tag to code with
//! `registerClientAction`..
use crate::serde_json::{Map, Value as Json};
use crate::{AddonDescriptor, BoxFuture, Env, Error, Registry, Result, Transaction};
use std::collections::BTreeMap;

/// Longest tag, in bytes.
pub const MAX_TAG_BYTES: usize = 64;

/// Validates and normalizes the `params` of one client action, with the caller's own rights.
pub type ParamsValidator = for<'a> fn(
    &'a Env,
    &'a mut Transaction,
    &'a Map<String, Json>,
) -> BoxFuture<'a, Result<Map<String, Json>>>;

/// One declared client action tag.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct ClientActionDecl {
    tag: &'static str,
    label: &'static str,
    validate: ParamsValidator,
}
impl std::fmt::Debug for ClientActionDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientActionDecl")
            .field("tag", &self.tag)
            .finish_non_exhaustive()
    }
}
impl ClientActionDecl {
    /// Declare `tag` (`<addon>.<name>`, `<name>` matching `[a-z][a-z0-9_]*`, at most 64 bytes).
    /// # Errors
    /// `Programming` for a malformed tag or an empty label.
    pub fn new(tag: &'static str, label: &'static str, validate: ParamsValidator) -> Result<Self> {
        let valid = tag.len() <= MAX_TAG_BYTES
            && tag.split_once('.').is_some_and(|(addon, name)| {
                !addon.is_empty()
                    && addon
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            });
        if !valid {
            return Err(Error::programming(format!(
                "Client action tag '{tag}' must be <addon>.<name> with <name> matching [a-z][a-z0-9_]* \
                 and at most {MAX_TAG_BYTES} bytes."
            )));
        }
        if label.trim().is_empty() {
            return Err(Error::programming(format!(
                "Client action '{tag}' needs a label."
            )));
        }
        Ok(Self {
            tag,
            label,
            validate,
        })
    }
    /// The tag.
    pub const fn tag(&self) -> &'static str {
        self.tag
    }
    /// The label shown in technical screens.
    pub const fn label(&self) -> &'static str {
        self.label
    }
}

/// An addon's client action declarations, bound to its descriptor.
#[derive(Debug)]
#[non_exhaustive]
pub struct Contribution {
    /// Native addon identity.
    pub addon: &'static AddonDescriptor,
    /// Pure declarations, called when the registry is inspected.
    pub declarations: fn() -> Result<Vec<ClientActionDecl>>,
}
impl Contribution {
    /// Bind a declaration factory to a native addon descriptor.
    pub const fn new(
        addon: &'static AddonDescriptor,
        declarations: fn() -> Result<Vec<ClientActionDecl>>,
    ) -> Self {
        Self {
            addon,
            declarations,
        }
    }
}

/// Link-time collection; only installed addons' entries are active.
#[linkme::distributed_slice]
pub static CLIENT_ACTION_CONTRIBUTIONS: [Contribution];

/// Register this addon's client action tags using its generated `ADDON` identity.
///
/// ```rust,ignore
/// fn tags() -> erp_sdk::Result<Vec<erp_sdk::client_actions::ClientActionDecl>> {
///     Ok(vec![erp_sdk::client_actions::ClientActionDecl::new("my_addon.board", "Board", check)?])
/// }
/// erp_sdk::client_action_declarations!(tags);
/// ```
#[macro_export]
#[allow(
    clippy::crate_in_macro_def,
    reason = "the invoking addon crate owns the generated ADDON descriptor"
)]
macro_rules! client_action_declarations {
    ($factory:path) => {
        #[$crate::__private::linkme::distributed_slice(
            $crate::client_actions::CLIENT_ACTION_CONTRIBUTIONS
        )]
        #[linkme(crate = $crate::__private::linkme)]
        static CLIENT_ACTIONS_CONTRIBUTION: $crate::client_actions::Contribution =
            $crate::client_actions::Contribution::new(&crate::ADDON, $factory);
    };
}

/// The tags of the installed addons.
#[derive(Debug)]
pub struct ClientActionRegistry {
    tags: BTreeMap<&'static str, ClientActionDecl>,
}
impl ClientActionRegistry {
    /// The declaration of `tag`, when an installed addon declares it.
    pub fn get(&self, tag: &str) -> Option<&ClientActionDecl> {
        self.tags.get(tag)
    }
    /// Run the tag's validator on `params`: the normalized parameters.
    /// # Errors
    /// `Validation` naming the tag when no installed addon declares it; the validator's own error.
    pub async fn validate(
        &self,
        env: &Env,
        tx: &mut Transaction,
        tag: &str,
        params: &Map<String, Json>,
    ) -> Result<Map<String, Json>> {
        let decl = self.get(tag).ok_or_else(|| unknown(tag))?;
        (decl.validate)(env, tx, params).await
    }
}

/// The refusal for a tag no installed addon declares.
pub fn unknown(tag: &str) -> Error {
    Error::validation(format!(
        "Client action tag '{}' is not declared by an installed addon.",
        tag.chars().take(MAX_TAG_BYTES).collect::<String>()
    ))
}

/// The client action tags of `env`'s installed addons.
/// # Errors
/// `Programming` for a tag outside its addon's namespace, a duplicate tag, or a contribution that
/// does not come from the installed descriptor.
pub fn registry(env: &Env) -> Result<ClientActionRegistry> {
    registry_of(env.registry())
}

/// [`registry`] for a model registry.
/// # Errors
/// As [`registry`].
pub fn registry_of(registry: &Registry) -> Result<ClientActionRegistry> {
    let mut tags = BTreeMap::new();
    for entry in CLIENT_ACTION_CONTRIBUTIONS {
        let Some(installed) = registry
            .addons()
            .iter()
            .find(|a| a.name == entry.addon.name)
        else {
            continue;
        };
        if installed.depends != entry.addon.depends {
            return Err(Error::programming(format!(
                "The client actions of addon {} do not come from its installed descriptor.",
                entry.addon.name
            )));
        }
        for decl in (entry.declarations)()? {
            let own = decl
                .tag
                .split_once('.')
                .is_some_and(|(addon, _)| addon == entry.addon.name);
            if !own {
                return Err(Error::programming(format!(
                    "Client action tag '{}' is not in the namespace of addon {}.",
                    decl.tag, entry.addon.name
                )));
            }
            if tags.insert(decl.tag, decl).is_some() {
                return Err(Error::programming(format!(
                    "Client action tag '{}' is declared twice.",
                    decl.tag
                )));
            }
        }
    }
    Ok(ClientActionRegistry { tags })
}
