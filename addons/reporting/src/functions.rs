// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native expression functions (design §4): a Rust function an `engine = native` expression names.
//!
//! An addon registers one with [`crate::expression_function_declarations!`]; the registration counts
//! only in a database where that addon is installed. The function reads as the caller (the report
//! context has no elevated environment); an addon that must read tables the caller has no grant on
//! says so at its own call site.
use erp_sdk::prelude::*;
use erp_sdk::{AddonDescriptor, BoxFuture};

use crate::options::Options;
use crate::provider::ReportCx;

/// What a native expression is asked for: one period of one line.
#[non_exhaustive]
pub struct NativeRequest<'a> {
    /// The model the expression declares (`model` of the expression record).
    pub model: &'a str,
    /// The optional source measure declared by the expression.
    pub field: Option<&'a str>,
    /// The expression's domain, its period, the selected companies and the keys of the line's
    /// parents, as the aggregate expressions of the line read them.
    /// For a More total this excludes all cumulatively loaded group keys (including null
    /// when loaded), so the function applies its own precision once to the remaining pool.
    pub domain: Domain,
    /// The many2one field the answer is grouped by, or `None` for one total.
    pub group: Option<&'a str>,
}

/// One value per group key (`None`: the record has no value in the field), or one entry for a
/// total.
pub type NativeValues = Vec<(Option<i64>, Decimal)>;

/// A registered function.
pub type NativeFn = for<'a> fn(
    &'a mut ReportCx<'_>,
    &'a Options,
    NativeRequest<'a>,
) -> BoxFuture<'a, Result<NativeValues>>;

/// Extra source restrictions of a native expression, used when opening contributing records.
pub type NativeDomain =
    for<'a> fn(&'a Env, &'a mut Transaction, &'a Options, &'a str) -> BoxFuture<'a, Result<Domain>>;

/// One registration, bound to the declaring addon.
#[non_exhaustive]
pub struct FunctionContribution {
    /// The declaring addon.
    pub addon: &'static AddonDescriptor,
    /// The name an expression gives in `function`.
    pub name: &'static str,
    /// The function.
    pub function: NativeFn,
    /// Restrictions applied by the native calculation in addition to the expression domain.
    pub drill_domain: NativeDomain,
}

impl FunctionContribution {
    /// Bind `function` to `addon` under `name`.
    pub const fn new(
        addon: &'static AddonDescriptor,
        name: &'static str,
        function: NativeFn,
        drill_domain: NativeDomain,
    ) -> Self {
        Self {
            addon,
            name,
            function,
            drill_domain,
        }
    }
}

/// Every function linked into the binary.
#[erp_sdk::__private::linkme::distributed_slice]
#[linkme(crate = erp_sdk::__private::linkme)]
pub static FUNCTIONS: [FunctionContribution];

/// Register a function for the calling crate's addon:
///
/// ```ignore
/// erp_addon_reporting::expression_function_declarations!(VALUE, "my_addon.value", value, source_domain);
/// ```
#[macro_export]
#[allow(
    clippy::crate_in_macro_def,
    reason = "`crate::ADDON` names the calling crate's addon: the function belongs to it"
)]
macro_rules! expression_function_declarations {
    ($id:ident, $name:expr, $function:path, $domain:path) => {
        #[$crate::__linkme::distributed_slice($crate::functions::FUNCTIONS)]
        #[linkme(crate = $crate::__linkme)]
        static $id: $crate::functions::FunctionContribution =
            $crate::functions::FunctionContribution::new(&crate::ADDON, $name, $function, $domain);
    };
}

/// The installed function named `name`.
///
/// # Errors
/// `Validation` naming the function when no installed addon registers it; `Programming` naming
/// both addons when two installed addons register one name.
pub fn find(env: &Env, name: &str) -> Result<&'static FunctionContribution> {
    let mut found: Option<&FunctionContribution> = None;
    for entry in FUNCTIONS.iter() {
        let installed = env
            .registry()
            .addons()
            .iter()
            .any(|a| a.name == entry.addon.name);
        if !installed || entry.name != name {
            continue;
        }
        if let Some(first) = found {
            return Err(Error::programming(format!(
                "Report function {name} is registered twice (addons {} and {}).",
                first.addon.name, entry.addon.name
            )));
        }
        found = Some(entry);
    }
    found.ok_or_else(|| {
        Error::validation(format!(
            "No installed addon provides the report function '{name}'."
        ))
    })
}
