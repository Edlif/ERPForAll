// SPDX-License-Identifier: MIT OR Apache-2.0
//! The wire form of every native method (WASM.WIRE-NATIVE).
//!
//! `#[methods]` emits one [`MethodWireImpl`] per native method into [`METHOD_WIRE`]. Its probe
//! answers, for the concrete argument tuple and result of the method, one of three things
//! ([`WireForm`]): the method's [`MethodCodec`] (both are `Serialize + DeserializeOwned + Send +
//! 'static`), the gap (the first argument or result type that is not), or that the method is
//! sealed (it never sits in a chain with a WebAssembly implementation): `#[method(sealed)]`, and
//! by default every `#[constrains]` and `#[onchange]` method ([`WireSeal`]). The core seals its
//! record-level methods on the access models besides (`SEALED_ON_MODEL`).
//!
//! A seal restricts **overriding**: who may call a method is decided elsewhere (RPC exposure for
//! clients, `CallableMethods` in `erp-wasm-host` for packages). A sealed implementation has no
//! wire codec, though, so a package's wire-form call into a chain of sealed entries only is
//! refused as well ([`WireSeal`]).
//!
//! A `linkme` slot is a `static`, and whether a type implements serde is a trait question a
//! constant cannot ask without specialization; so the decision is a `fn` the dispatch build calls,
//! and the macro resolves it by autoref specialization on the concrete types ([`SigProbe`],
//! [`WireProbe`]). The codec itself is an associated constant, so the `&'static` is a constant and
//! not a promoted temporary.
//!
//! Hand-registered core methods have no `#[methods]` block; their wire records are in
//! `BUILTIN_WIRE` (gaps and seals) and `wire.rs` (codecs).

use std::fmt;
use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dispatch::{MethodCodec, TypedCodec};

/// What a native method offers a chain that crosses into WebAssembly.
#[derive(Clone, Copy)]
pub enum WireForm {
    /// The argument tuple and the result are serialisable: the method's codec.
    Codec(&'static dyn MethodCodec),
    /// The first types that are not serialisable, each as the source text with its role
    /// (`` `Vec<RawVals>` (argument `vals_list`) ``, `` `Records` (result) ``).
    Gap(&'static [&'static str]),
    /// The method never runs in a chain with a WebAssembly implementation, whatever its types;
    /// the [`WireSeal`] says why.
    Sealed(WireSeal),
}

/// Why a native method is sealed ([`WireForm::Sealed`]).
///
/// A seal restricts **overriding**: what it refuses is a chain in which a WebAssembly
/// implementation sits next to the sealed native one. A sealed method keeps running natively and
/// over RPC. Which methods a package may call is decided elsewhere (`erp-wasm-host`'s
/// `CallableMethods`), but a sealed implementation contributes no wire codec, so a package's call
/// in the wire form reaches the method only when another native implementation of its chain has
/// one; a chain whose native entries are all sealed refuses that call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireSeal {
    /// `#[method(sealed)]` on the method.
    Attribute,
    /// A `#[constrains]` method: sealed by default. The ORM runs every constraint through its
    /// chain while it checks a write, so a package layer could skip the check by not calling
    /// `super`. Package overrides of constraints come with WASM.COMPUTE-REL.
    Constraint,
    /// An `#[onchange]` method: sealed by default, for the same reason. Package overrides of
    /// onchange methods come with WASM.ONCHANGE.
    Onchange,
}

impl fmt::Debug for WireForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(_) => f.write_str("Codec"),
            Self::Gap(types) => f.debug_tuple("Gap").field(types).finish(),
            Self::Sealed(seal) => f.debug_tuple("Sealed").field(seal).finish(),
        }
    }
}

/// The wire record of one native method, contributed by `#[methods]` (and by the core for its
/// hand-registered methods, `BUILTIN_WIRE`).
pub struct MethodWireImpl {
    /// Contributing addon.
    pub addon: &'static str,
    /// Model of the declaration it belongs to.
    pub model: &'static str,
    /// Method name.
    pub method: &'static str,
    /// Source file of the method (for a seal, the place that seals it).
    pub file: &'static str,
    /// Source line of the method.
    pub line: u32,
    /// Answers the method's [`WireForm`]; called when the dispatch table is built.
    pub probe: fn() -> WireForm,
}

impl MethodWireImpl {
    /// `file:line` of the method.
    #[must_use]
    pub fn location(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

impl fmt::Debug for MethodWireImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MethodWireImpl({}.{} of {} at {}:{})",
            self.model, self.method, self.addon, self.file, self.line
        )
    }
}

/// Wire records of the native methods linked into the binary.
#[linkme::distributed_slice]
pub static METHOD_WIRE: [MethodWireImpl];

/// Autoref probe of one type: `(&WireProbe::<T>::new()).wire_ok()` is `true` when `T` is
/// `Serialize + DeserializeOwned + Send + 'static` ([`WireProbeSerde`]) and `false` otherwise
/// ([`WireProbeOther`], found one autoref later). Only meaningful on a concrete `T`: macro output.
#[doc(hidden)]
pub struct WireProbe<T>(PhantomData<fn() -> T>);

impl<T> WireProbe<T> {
    /// A probe of `T`.
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> Default for WireProbe<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// The arm of [`WireProbe`] for a serialisable type.
#[doc(hidden)]
pub trait WireProbeSerde {
    /// `true`.
    fn wire_ok(&self) -> bool {
        true
    }
}

impl<T: Serialize + DeserializeOwned + Send + 'static> WireProbeSerde for WireProbe<T> {}

/// The arm of [`WireProbe`] for any other type.
#[doc(hidden)]
pub trait WireProbeOther {
    /// `false`.
    fn wire_ok(&self) -> bool {
        false
    }
}

impl<T> WireProbeOther for &WireProbe<T> {}

/// Autoref probe of a whole signature: `(&SigProbe::<A, R>::new()).wire_codec()` is the
/// [`TypedCodec`] of `A` and `R` when both are serialisable ([`SigProbeSerde`]) and `None`
/// otherwise ([`SigProbeOther`]). Only meaningful on concrete types: macro output.
#[doc(hidden)]
pub struct SigProbe<A, R>(PhantomData<fn(A) -> R>);

impl<A, R> SigProbe<A, R> {
    /// A probe of the signature `A -> R`.
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<A, R> Default for SigProbe<A, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A, R> SigProbe<A, R>
where
    A: Serialize + DeserializeOwned + Send + 'static,
    R: Serialize + DeserializeOwned + Send + 'static,
{
    /// The codec, a constant so the reference is `'static` without promotion.
    const CODEC: &'static dyn MethodCodec = &TypedCodec::<A, R>::new();
}

/// The arm of [`SigProbe`] for a serialisable signature.
#[doc(hidden)]
pub trait SigProbeSerde {
    /// The signature's codec.
    fn wire_codec(&self) -> Option<&'static dyn MethodCodec>;
}

impl<A, R> SigProbeSerde for SigProbe<A, R>
where
    A: Serialize + DeserializeOwned + Send + 'static,
    R: Serialize + DeserializeOwned + Send + 'static,
{
    fn wire_codec(&self) -> Option<&'static dyn MethodCodec> {
        Some(Self::CODEC)
    }
}

/// The arm of [`SigProbe`] for any other signature.
#[doc(hidden)]
pub trait SigProbeOther {
    /// `None`.
    fn wire_codec(&self) -> Option<&'static dyn MethodCodec> {
        None
    }
}

impl<A, R> SigProbeOther for &SigProbe<A, R> {}

/// Compile-time check of `#[method(wire)]`: `<WireCheck<T>>::OK` is `true` when `T` is
/// serialisable (the inherent constant, whose bounds hold) and `false` otherwise (the trait
/// constant of [`WireCheckOther`]); the macro asserts it per argument and result with a message
/// that names the method. Only meaningful on a concrete `T`: macro output.
#[doc(hidden)]
pub struct WireCheck<T>(PhantomData<fn() -> T>);

impl<T: Serialize + DeserializeOwned + Send + 'static> WireCheck<T> {
    /// `true`: `T` has a wire form.
    pub const OK: bool = true;
}

/// The fallback of [`WireCheck`].
#[doc(hidden)]
pub trait WireCheckOther {
    /// `false`: `T` has no wire form.
    const OK: bool = false;
}

impl<T> WireCheckOther for WireCheck<T> {}

fn sealed() -> WireForm {
    WireForm::Sealed(WireSeal::Attribute)
}

fn copy_gap() -> WireForm {
    WireForm::Gap(&["`Option<ValueMap>` (argument 1)", "`Records` (result)"])
}

fn copy_data_gap() -> WireForm {
    WireForm::Gap(&[
        "`Option<ValueMap>` (argument 1)",
        "`Vec<Option<ValueMap>>` (result)",
    ])
}

fn default_get_gap() -> WireForm {
    WireForm::Gap(&["`ValueMap` (result)"])
}

fn exists_gap() -> WireForm {
    WireForm::Gap(&["`Records` (result)"])
}

fn filtered_domain_gap() -> WireForm {
    WireForm::Gap(&["`Domain` (argument 1)", "`Records` (result)"])
}

fn upsert_gap() -> WireForm {
    WireForm::Gap(&["`UpsertCall` (argument 1)", "`UpsertResult` (result)"])
}

macro_rules! builtin_wire {
    ($name:ident, $method:expr, $probe:path) => {
        static $name: MethodWireImpl = MethodWireImpl {
            addon: "base",
            model: "base",
            method: $method,
            file: file!(),
            line: line!(),
            probe: $probe,
        };
    };
}

// The two module-system callbacks run schema and registry work for every model:
// sealed, so no package sits in their chain.
builtin_wire!(INIT_WIRE, crate::model_hooks::INIT, sealed);
builtin_wire!(
    REGISTER_HOOK_WIRE,
    crate::model_hooks::REGISTER_HOOK,
    sealed
);
// Core methods whose types carry records, value maps or domains: their codecs and guest helpers
// come with WASM.WIRE-NATIVE.CORE-CODECS.
builtin_wire!(COPY_WIRE, "copy", copy_gap);
builtin_wire!(COPY_DATA_WIRE, "copy_data", copy_data_gap);
builtin_wire!(DEFAULT_GET_WIRE, "default_get", default_get_gap);
builtin_wire!(EXISTS_WIRE, "exists", exists_gap);
builtin_wire!(FILTERED_DOMAIN_WIRE, "filtered_domain", filtered_domain_gap);
builtin_wire!(UPSERT_WIRE, "upsert", upsert_gap);

/// Wire records of the hand-registered core methods that have no codec: the seals of `init` and
/// `_register_hook`, and the named gaps of `copy`, `copy_data`, `default_get`, `exists`,
/// `filtered_domain` and `upsert`. A core method in neither this list nor `wire.rs` is refused
/// by the dispatch build as "no wire record" when a chain crosses.
pub(crate) static BUILTIN_WIRE: [&MethodWireImpl; 8] = [
    &INIT_WIRE,
    &REGISTER_HOOK_WIRE,
    &COPY_WIRE,
    &COPY_DATA_WIRE,
    &DEFAULT_GET_WIRE,
    &EXISTS_WIRE,
    &FILTERED_DOMAIN_WIRE,
    &UPSERT_WIRE,
];

/// One row of [`SEALED_ON_MODEL`]: core methods sealed on one model.
pub(crate) struct ModelSeal {
    /// The model whose chains the row seals (the chain's model, not the model a core entry is
    /// registered under: the core registers its methods under `base`).
    pub(crate) model: &'static str,
    /// The sealed methods.
    pub(crate) methods: &'static [&'static str],
    /// Source file of the row.
    pub(crate) file: &'static str,
    /// Source line of the row.
    pub(crate) line: u32,
}

impl ModelSeal {
    /// `file:line` of the row.
    pub(crate) fn location(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// The record-level core methods, sealed on the models that hold users, groups, access rows, API
/// keys and system parameters.
const CORE_RECORD_METHODS: &[&str] = &[
    "action_archive",
    "action_unarchive",
    "create",
    "write",
    "unlink",
    "copy",
    "copy_data",
    "default_get",
    "name_create",
    "upsert",
];

macro_rules! model_seal {
    ($model:expr) => {
        ModelSeal {
            model: $model,
            methods: CORE_RECORD_METHODS,
            file: file!(),
            line: line!(),
        }
    };
}

/// Core methods sealed per model (WASM.WIRE-NATIVE review, SHOULD-3).
///
/// The core's own codecs (`action_archive`, `action_unarchive`, `create`, `write`, `unlink`,
/// `name_create`, `wire.rs`) hold on every model, so without this list a package that extends
/// `res.users` could wrap `action_archive` and swallow a deactivation. A chain on one of these
/// models that crosses into a package and names one of these methods is refused whatever its
/// native entries say. `ir.rule` is not here: no Rust model of that name exists yet; add its row
/// when one does. `copy_data` and `upsert` have no codec yet; they are listed so the list stays
/// closed when WASM.WIRE-NATIVE.CORE-CODECS gives them one.
pub(crate) static SEALED_ON_MODEL: [ModelSeal; 5] = [
    model_seal!("res.users"),
    model_seal!("res.groups"),
    model_seal!("ir.access"),
    model_seal!("res.users.apikeys"),
    model_seal!("ir.config_parameter"),
];

/// The row of [`SEALED_ON_MODEL`] that seals `method` on `model`, if any.
pub(crate) fn model_seal(model: &str, method: &str) -> Option<&'static ModelSeal> {
    SEALED_ON_MODEL
        .iter()
        .find(|s| s.model == model && s.methods.contains(&method))
}

/// `file:line` of the core list row that seals `method` on `model`, if one does. Diagnostics for
/// tests and tools, not a stable API.
#[doc(hidden)]
#[must_use]
pub fn model_seal_location(model: &str, method: &str) -> Option<String> {
    model_seal(model, method).map(ModelSeal::location)
}

/// One row of [`wire_report`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireReportRow {
    /// Contributing addon.
    pub addon: &'static str,
    /// Model of the declaration.
    pub model: &'static str,
    /// Method name.
    pub method: &'static str,
    /// `file:line` of the method.
    pub location: String,
    /// `codec`, `sealed`, or `gap: ` followed by the blocking types.
    pub form: String,
}

/// Every native method linked into the binary with its wire form, sorted by addon, model and
/// method: what a package author can override, what is sealed and which type blocks the rest
/// (the gap report of WASM.WIRE-NATIVE). Diagnostics for tests and tools, not a stable API.
#[doc(hidden)]
#[must_use]
pub fn wire_report() -> Vec<WireReportRow> {
    let mut rows: Vec<WireReportRow> = METHOD_WIRE
        .iter()
        .chain(BUILTIN_WIRE.iter().copied())
        .map(|w| WireReportRow {
            addon: w.addon,
            model: w.model,
            method: w.method,
            location: w.location(),
            form: match (w.probe)() {
                WireForm::Codec(_) => "codec".to_owned(),
                WireForm::Sealed(WireSeal::Attribute) => "sealed".to_owned(),
                WireForm::Sealed(WireSeal::Constraint) => "sealed (constraint)".to_owned(),
                WireForm::Sealed(WireSeal::Onchange) => "sealed (onchange)".to_owned(),
                WireForm::Gap(types) => format!("gap: {}", types.join(", ")),
            },
        })
        .collect();
    rows.sort_by(|a, b| (a.addon, a.model, a.method).cmp(&(b.addon, b.model, b.method)));
    rows
}

#[cfg(test)]
#[allow(
    clippy::needless_borrow,
    reason = "autoref specialization: the borrow selects the probe arm"
)]
mod tests {
    use super::*;
    use crate::records::Records;
    use crate::upsert::{UpsertCall, UpsertResult};
    use crate::vals::RawVals;
    use erp_domain::Domain;
    use erp_fields::{Value, ValueMap};

    #[test]
    fn the_core_gap_types_have_no_wire_form() {
        assert!(!(&WireProbe::<Option<ValueMap>>::new()).wire_ok());
        assert!(!(&WireProbe::<Vec<Option<ValueMap>>>::new()).wire_ok());
        assert!(!(&WireProbe::<ValueMap>::new()).wire_ok());
        assert!(!(&WireProbe::<Value>::new()).wire_ok());
        assert!(!(&WireProbe::<RawVals>::new()).wire_ok());
        assert!(!(&WireProbe::<Records>::new()).wire_ok());
        assert!(!(&WireProbe::<Domain>::new()).wire_ok());
        assert!(!(&WireProbe::<UpsertCall>::new()).wire_ok());
        assert!(!(&WireProbe::<UpsertResult>::new()).wire_ok());
        assert!((&WireProbe::<Vec<String>>::new()).wire_ok());
        assert!((&WireProbe::<Option<i64>>::new()).wire_ok());
        assert!((&WireProbe::<()>::new()).wire_ok());
    }

    #[test]
    fn a_signature_probe_yields_the_codec_of_its_types() {
        let codec = (&SigProbe::<(Vec<i64>,), i64>::new())
            .wire_codec()
            .expect("plain data has a codec");
        assert_eq!(
            codec.signature(),
            std::any::TypeId::of::<crate::dispatch::MethodFn<(Vec<i64>,), i64>>()
        );
        assert!(
            (&SigProbe::<(Vec<RawVals>,), Records>::new())
                .wire_codec()
                .is_none()
        );
        assert!((&SigProbe::<(), ValueMap>::new()).wire_codec().is_none());
    }
}
