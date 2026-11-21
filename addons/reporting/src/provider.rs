// SPDX-License-Identifier: MIT OR Apache-2.0
//! Report providers and their registry (design §4).
//!
//! A provider answers the lines, the expansions and the drill-downs of the reports that name it.
//! A native addon registers one with [`crate::provider_declarations!`]; the registration counts only
//! in a database where that addon is installed. Two installed providers with one name fail every
//! lookup with an error naming both addons.
use erp_sdk::prelude::*;
use erp_sdk::serde_json::Value as Json;
use erp_sdk::{AddonDescriptor, BoxFuture};

use crate::definition::ReportDef;
use crate::line::{DrillTarget, ExpandPage, Expansion, LineId};
use crate::options::Options;
use crate::settings::Bounds;

/// When a provider's answers are computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Snapshot {
    /// Every call recomputes in its own read transaction.
    PerRequest,
    /// One answer for every unfolded line (ledger reports): refused until
    /// implements it.
    WholeTree,
}

/// What a provider declares once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProviderSpec {
    /// The name a `reporting.report` record gives in `provider`.
    pub name: &'static str,
    /// Bumped when its options or line ids change meaning.
    pub version: u32,
    /// [`Snapshot::PerRequest`] unless set.
    pub snapshot: Snapshot,
    server_search: bool,
}

impl ProviderSpec {
    /// A per-request provider.
    pub const fn new(name: &'static str, version: u32) -> Self {
        Self {
            name,
            version,
            snapshot: Snapshot::PerRequest,
            server_search: false,
        }
    }
    /// Declare that the provider applies the resolved search to the report's result.
    /// Screens and exports can then use that result without filtering display labels a second time.
    #[must_use]
    pub const fn server_search(mut self) -> Self {
        self.server_search = true;
        self
    }

    pub(crate) const fn searches_on_server(self) -> bool {
        self.server_search
    }

    /// With another snapshot kind.
    #[must_use]
    pub const fn snapshot(mut self, snapshot: Snapshot) -> Self {
        self.snapshot = snapshot;
        self
    }
}

/// The caller's environment and the call's read transaction. There is no elevated environment
/// here: a provider reads as the caller.
pub struct ReportCx<'a> {
    env: Env,
    tx: &'a mut Transaction,
    bounds: Bounds,
}

impl<'a> ReportCx<'a> {
    pub(crate) fn new(env: Env, tx: &'a mut Transaction, bounds: Bounds) -> Self {
        Self { env, tx, bounds }
    }
    /// The caller's environment.
    pub fn env(&self) -> &Env {
        &self.env
    }
    /// The call's transaction.
    pub fn tx(&mut self) -> &mut Transaction {
        self.tx
    }
    /// The environment and the transaction together.
    pub fn parts(&mut self) -> (&Env, &mut Transaction) {
        (&self.env, self.tx)
    }
    /// The bounds of the settings.
    pub fn bounds(&self) -> &Bounds {
        &self.bounds
    }
}

/// The lines of one report kind.
pub trait ReportProvider: Send + Sync + 'static {
    /// The declaration.
    fn spec(&self) -> &ProviderSpec;

    /// The provider's own option keys (`options.provider`), after the shared initializers;
    /// refuses a key it does not know. `strict` as for the shared options.
    fn init_options<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a mut Options,
        strict: bool,
    ) -> BoxFuture<'a, Result<()>>;

    /// The top-level lines, and the names of optional lines left out for the caller in
    /// [`Expansion::hidden`]. The runtime unfolds the lines the options ask for through
    /// [`ReportProvider::expand`].
    fn lines<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
    ) -> BoxFuture<'a, Result<Expansion>>;

    /// The children of `line`, one page; checks `line` against the report before any query.
    fn expand<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>>;

    /// The action behind a caret item or a cell, as an action result.
    fn drill<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>>;
}

/// One registration, bound to the declaring addon.
#[non_exhaustive]
pub struct ProviderContribution {
    /// The declaring addon.
    pub addon: &'static AddonDescriptor,
    /// The provider.
    pub provider: &'static dyn ReportProvider,
}

impl ProviderContribution {
    /// Bind `provider` to `addon`.
    pub const fn new(
        addon: &'static AddonDescriptor,
        provider: &'static dyn ReportProvider,
    ) -> Self {
        Self { addon, provider }
    }
}

/// Every provider linked into the binary.
#[erp_sdk::__private::linkme::distributed_slice]
#[linkme(crate = erp_sdk::__private::linkme)]
pub static PROVIDERS: [ProviderContribution];

/// Register a provider value (a `static` or a constant expression) for the calling crate's addon:
///
/// ```ignore
/// struct StockAging;
/// impl erp_addon_reporting::provider::ReportProvider for StockAging { /* ... */ }
/// erp_addon_reporting::provider_declarations!(StockAging);
/// ```
#[macro_export]
#[allow(
    clippy::crate_in_macro_def,
    reason = "`crate::ADDON` names the calling crate's addon: the provider belongs to it"
)]
macro_rules! provider_declarations {
    ($provider:expr) => {
        #[$crate::__linkme::distributed_slice($crate::provider::PROVIDERS)]
        #[linkme(crate = $crate::__linkme)]
        static REPORT_PROVIDER: $crate::provider::ProviderContribution =
            $crate::provider::ProviderContribution::new(&crate::ADDON, &$provider);
    };
}

/// The installed provider named `name`.
///
/// # Errors
/// `Validation` naming the provider when no installed addon registers it; `Programming` naming
/// both addons when two installed addons register one name (`reporting.definition` included), and
/// naming for a
/// whole-tree provider.
pub fn find(env: &Env, name: &str) -> Result<&'static dyn ReportProvider> {
    // the addon's own provider is not in the link-time slice: a slice element in its declaring
    // crate needs `unsafe_code`, which the workspace forbids
    static OWN: ProviderContribution =
        ProviderContribution::new(&crate::ADDON, &crate::definition::DefinitionProvider);
    let mut found: Option<&ProviderContribution> =
        (name == crate::definition::DEFINITION).then_some(&OWN);
    for entry in PROVIDERS.iter() {
        let installed = env
            .registry()
            .addons()
            .iter()
            .any(|a| a.name == entry.addon.name);
        if !installed || entry.provider.spec().name != name {
            continue;
        }
        if let Some(first) = found {
            return Err(Error::programming(format!(
                "Report provider {name} is registered twice (addons {} and {}).",
                first.addon.name, entry.addon.name
            )));
        }
        found = Some(entry);
    }
    let entry = found.ok_or_else(|| {
        Error::validation(format!(
            "No installed addon provides the report provider '{name}'."
        ))
    })?;
    if entry.provider.spec().snapshot == Snapshot::WholeTree {
        return Err(Error::programming(format!(
            "Report provider {name} needs whole-tree snapshots, not supported until a later release."
        )));
    }
    Ok(entry.provider)
}
