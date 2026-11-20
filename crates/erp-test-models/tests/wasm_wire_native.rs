// SPDX-License-Identifier: MIT OR Apache-2.0
//! The automatic wire form of native methods (WASM.WIRE-NATIVE,
//! (WASM.WIRE-NATIVE.CORE-CODECS).
//!
//! The WebAssembly layer is a stand-in `MethodInvoker` written here (as in
//! `wasm_dispatch_chain.rs`), in the addon `test_wire_layer`, which only extends models. Below it
//! `test_wire_lower` defines `test.wire.seam` and its methods natively, none marked
//! `#[method(wire)]`; above it `test_wire_upper` overrides one natively. `test_wire_layer` also
//! extends `res.users`, `account.fiscal.position` and `ir.ui.view`, so the stand-in can sit in
//! their chains.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::{Arc, Mutex};

use erp_addon_account::fpos::FiscalPosition;
use erp_addon_base::ir_ui_view::IrUiView;
use erp_orm::{
    BoxFuture, DynSuper, ImplKind, MethodInvoker, Orm, Records, Transaction, WasmMethodImpl,
    call_method, call_method_json, is_wasm_abi1_native_refusal, model_seal_location,
    wasm_abi1_value_to_json, wire_report,
};
use erp_sdk::prelude::*;
use erp_sdk::{ErrorKind, ValueMap};
use erp_testkit::Harness;

const MODEL: &str = "test.wire.seam";
/// The keyed fixture model (`upsert`, and a field without a form in the frozen ABI).
const KEYED: &str = "test.wire.keyed";
/// Its declared business key.
const KEYED_KEY: &str = "_code_key";
/// The name of the record the core-codec tests create, and of its copy.
const SEAM_NAME: &str = "Seam one";
const COPY_NAME: &str = "Seam copy";
/// The key value and the accumulated amount of the upsert tests.
const KEYED_CODE: &str = "A";
const KEYED_HITS: i32 = 2;

/// What the native top layer adds to `plain_total`.
const UPPER_ADD: i64 = 1000;
/// What the WebAssembly stand-in adds to `plain_total`.
const LAYER_ADD: i64 = 100;

mod lower {
    use super::*;

    erp_sdk::addon!(name = "test_wire_lower", depends = ["base"]);

    /// No serde implementation: a method that takes it has no wire form.
    pub struct Note(pub i64);

    #[model(
        name = "test.wire.seam",
        description = "Wire form fixture",
        addon = "test_wire_lower"
    )]
    pub struct Seam {
        #[field(char, required = true)]
        pub name: Option<Arc<str>>,
        #[field(boolean, default = true)]
        pub active: bool,
    }

    /// A model with a business key, for `upsert`, and with a field (`big`) that has no form in
    /// the frozen ABI.
    #[model(
        name = "test.wire.keyed",
        description = "Wire form fixture with a key",
        addon = "test_wire_lower",
        keys = [key(name = "_code_key", fields = ["code"], message = "The code must be unique.")]
    )]
    pub struct Keyed {
        #[field(char, required)]
        pub code: Option<Arc<str>>,
        #[field(integer)]
        pub hits: i32,
        #[field(bigint)]
        pub big: Option<BigInt>,
    }

    #[methods(Seam, addon = "test_wire_lower")]
    impl Seam {
        /// Serialisable and not marked `wire`: the sum of the values.
        #[method]
        async fn plain_total(
            _this: &RecordSet<Seam>,
            _tx: &mut Transaction,
            values: Vec<i64>,
        ) -> Result<i64> {
            Ok(values.iter().sum())
        }

        /// Takes a type without serde.
        #[method]
        async fn opaque(_this: &RecordSet<Seam>, _tx: &mut Transaction, note: Note) -> Result<i64> {
            Ok(note.0)
        }

        /// Serialisable, but sealed.
        #[method(sealed)]
        async fn guarded(_this: &RecordSet<Seam>, _tx: &mut Transaction) -> Result<()> {
            Ok(())
        }

        /// A constraint: `()` -> `()`, so it would have a codec, but a constraint is sealed by
        /// default.
        #[constrains("name")]
        async fn _check_seam_name(_this: &RecordSet<Seam>, _tx: &mut Transaction) -> Result<()> {
            Ok(())
        }

        /// An onchange handler: sealed by default like a constraint.
        #[onchange("name")]
        async fn _onchange_seam_name(
            _this: &RecordSet<Seam>,
            _tx: &mut Transaction,
        ) -> Result<OnchangeResult> {
            Ok(OnchangeResult::none())
        }
    }
}

mod layer {
    use super::lower::{Keyed, Seam};
    use super::*;

    erp_sdk::addon!(
        name = "test_wire_layer",
        depends = ["test_wire_lower", "account"]
    );

    #[extend(Seam, addon = "test_wire_layer")]
    pub struct SeamLayer {}

    #[extend(Keyed, addon = "test_wire_layer")]
    pub struct KeyedLayer {}

    #[extend(ResUsers, addon = "test_wire_layer")]
    pub struct UsersLayer {}

    #[extend(FiscalPosition, addon = "test_wire_layer")]
    pub struct FiscalPositionLayer {}

    #[extend(IrUiView, addon = "test_wire_layer")]
    pub struct ViewLayer {}
}

mod upper {
    use super::lower::Seam;
    use super::*;

    erp_sdk::addon!(name = "test_wire_upper", depends = ["test_wire_layer"]);

    #[extend(Seam, addon = "test_wire_upper")]
    pub struct SeamUpper {}

    #[methods(Seam, addon = "test_wire_upper")]
    impl SeamUpper {
        /// The native top layer: the layers below plus [`UPPER_ADD`].
        #[method(override)]
        async fn plain_total(
            this: &RecordSet<Seam>,
            tx: &mut Transaction,
            values: Vec<i64>,
            sup: Super,
        ) -> Result<i64> {
            Ok(sup.call(tx, this.as_records(), (values,)).await? + UPPER_ADD)
        }
    }
}

/// The stand-in WebAssembly layer: logs the wire arguments it got and what its `super()` returned,
/// and for `plain_total` adds [`LAYER_ADD`].
#[derive(Debug)]
struct StandIn {
    model: &'static str,
    method: &'static str,
    log: Arc<Mutex<Vec<String>>>,
}

impl MethodInvoker for StandIn {
    fn addon(&self) -> &str {
        layer::ADDON.name
    }

    fn kind(&self) -> ImplKind {
        ImplKind::Override
    }

    fn invoke<'a>(
        &'a self,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
        sup: DynSuper,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let inner = sup.call_json(tx, records, args_json).await?;
            self.log.lock().unwrap().push(format!(
                "{} {} args {args_json} super {inner}",
                self.model, self.method
            ));
            Ok(match self.method {
                "plain_total" => {
                    let total: i64 = serde_json::from_str(&inner).unwrap();
                    serde_json::to_string(&(total + LAYER_ADD)).unwrap()
                }
                _ => inner,
            })
        })
    }
}

fn stand_in(
    model: &'static str,
    method: &'static str,
    log: &Arc<Mutex<Vec<String>>>,
) -> WasmMethodImpl {
    WasmMethodImpl {
        model: model.to_owned(),
        method: method.to_owned(),
        invoker: Arc::new(StandIn {
            model,
            method,
            log: Arc::clone(log),
        }),
    }
}

async fn harness() -> Harness {
    Harness::builder([
        &erp_addon_base::ADDON,
        &erp_addon_web::ADDON,
        &erp_addon_uom::ADDON,
        &erp_addon_product::ADDON,
        &erp_addon_analytic::ADDON,
        &erp_addon_account::ADDON,
        &lower::ADDON,
        &layer::ADDON,
        &upper::ADDON,
    ])
    .require_sql_objects([&erp_addon_account::reconcile::history_guard::MANIFEST])
    .schema_sync_options(erp_testkit::SchemaSyncOptions::inline().lifecycle(true))
    .build()
    .await
    .unwrap()
}

/// An ORM whose `test_wire_layer` layer is `wasm`.
fn with_wasm(h: &Harness, wasm: Vec<WasmMethodImpl>) -> erp_fields::Result<Orm> {
    h.orm()
        .rebuild(Arc::clone(h.registry()))
        .wasm_methods(wasm)
        .build()
}

/// `file:line (addon '…')` of the one native implementation `addon` contributes to a chain, as
/// the dispatch build names it.
fn native_location(h: &Harness, model: &str, method: &str, addon: &str) -> String {
    let id = h.registry().model(model).unwrap().id;
    let imps = h.orm().dispatch().implementations(id, method);
    let imp = imps
        .iter()
        .find(|i| i.addon == addon)
        .unwrap_or_else(|| panic!("{model}.{method} has no native implementation of {addon}"));
    format!("{}:{} (addon '{}')", imp.file, imp.line, imp.addon)
}

/// The gap report row of a method.
fn report(model: &str, addon: &str, method: &str) -> erp_orm::WireReportRow {
    wire_report()
        .into_iter()
        .find(|r| r.model == model && r.addon == addon && r.method == method)
        .unwrap_or_else(|| panic!("{addon}'s {model}.{method} is not in the gap report"))
}

#[tokio::test]
async fn a_wasm_layer_over_a_serde_method_without_wire_builds_and_runs() {
    let h = harness().await;
    assert_eq!(
        report(MODEL, lower::ADDON.name, "plain_total").form,
        "codec"
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let orm = with_wasm(&h, vec![stand_in(MODEL, "plain_total", &log)]).unwrap();
    let mut tx = orm.begin(h.principal()).await.unwrap();
    let records = tx.env().records(MODEL).unwrap();

    let values = vec![1_i64, 2, 3];
    let got: i64 = call_method(&mut tx, &records, "plain_total", (values.clone(),))
        .await
        .unwrap();
    let sum: i64 = values.iter().sum();
    assert_eq!(got, sum + LAYER_ADD + UPPER_ADD);
    assert_eq!(
        *log.lock().unwrap(),
        vec![format!(
            "{MODEL} plain_total args {} super {}",
            serde_json::to_string(&(values,)).unwrap(),
            serde_json::to_string(&sum).unwrap()
        )]
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_wasm_layer_over_a_method_without_serde_is_refused_naming_the_type() {
    let h = harness().await;
    let row = report(MODEL, lower::ADDON.name, "opaque");
    let gap = row.form.strip_prefix("gap: ").expect("opaque is a gap");
    let log = Arc::new(Mutex::new(Vec::new()));
    let err = with_wasm(&h, vec![stand_in(MODEL, "opaque", &log)])
        .err()
        .expect("the build must refuse");
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        format!(
            "method 'opaque' of model '{MODEL}' has a WebAssembly implementation, but no native \
             implementation has a wire form ({} is not serialisable: {gap}): a chain that crosses \
             between native code and a component needs every argument and the result to be \
             Serialize + DeserializeOwned",
            native_location(&h, MODEL, "opaque", lower::ADDON.name)
        )
    );
    // the gap names the argument and its type as the source writes them
    assert_eq!(gap, "`Note` (argument `note`)");
}

#[tokio::test]
async fn a_wasm_layer_over_a_sealed_method_is_refused_naming_the_seal() {
    let h = harness().await;
    let row = report(MODEL, lower::ADDON.name, "guarded");
    assert_eq!(row.form, "sealed");
    let log = Arc::new(Mutex::new(Vec::new()));
    let err = with_wasm(&h, vec![stand_in(MODEL, "guarded", &log)])
        .err()
        .expect("the build must refuse");
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        format!(
            "method 'guarded' of model '{MODEL}' has a WebAssembly implementation, but its native \
             implementation is sealed ({}, sealed at {}): a sealed method never runs in a chain \
             with a package",
            native_location(&h, MODEL, "guarded", lower::ADDON.name),
            row.location
        )
    );
}

#[tokio::test]
async fn a_wasm_layer_over_set_password_is_refused() {
    let h = harness().await;
    let row = report("res.users", "base", "_set_password");
    assert_eq!(row.form, "sealed");
    let log = Arc::new(Mutex::new(Vec::new()));
    let err = with_wasm(&h, vec![stand_in("res.users", "_set_password", &log)])
        .err()
        .expect("the build must refuse");
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        format!(
            "method '_set_password' of model 'res.users' has a WebAssembly implementation, but its \
             native implementation is sealed ({}, sealed at {}): a sealed method never runs in a \
             chain with a package",
            native_location(&h, "res.users", "_set_password", "base"),
            row.location
        )
    );
}

#[tokio::test]
async fn wasm_layers_over_action_archive_and_name_create_reach_the_core() {
    let h = harness().await;
    let log = Arc::new(Mutex::new(Vec::new()));
    let orm = with_wasm(
        &h,
        vec![
            stand_in(MODEL, "name_create", &log),
            stand_in(MODEL, "action_archive", &log),
        ],
    )
    .unwrap();
    let mut tx = orm.begin(h.principal()).await.unwrap();
    let model = tx.env().records(MODEL).unwrap();

    let (id, display) = model.name_create(&mut tx, "Seam one").await.unwrap();
    let record = model.browse([id]).unwrap();
    let read = record.read(&mut tx, &["name", "active"]).await.unwrap();
    assert_eq!(read[0]["name"], Value::str("Seam one"));
    assert_eq!(&*display, "Seam one");
    record.action_archive(&mut tx).await.unwrap();
    let read = record.read(&mut tx, &["active"]).await.unwrap();
    assert_eq!(read[0]["active"], Value::Bool(false));
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            format!(
                "{MODEL} name_create args {} super {}",
                serde_json::to_string(&("Seam one",)).unwrap(),
                serde_json::to_string(&(id, "Seam one")).unwrap()
            ),
            format!("{MODEL} action_archive args null super null"),
        ]
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_wasm_layer_over_account_map_tax_runs() {
    let h = harness().await;
    assert_eq!(
        report("account.fiscal.position", "account", "map_tax").form,
        "codec"
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let orm = with_wasm(
        &h,
        vec![stand_in("account.fiscal.position", "map_tax", &log)],
    )
    .unwrap();
    let mut tx = orm.begin(h.principal()).await.unwrap();
    // an empty position maps every tax to itself (the account definition)
    let none = tx.env().records("account.fiscal.position").unwrap();
    let taxes: Vec<i64> = Vec::new();
    let got: Vec<i64> = call_method(&mut tx, &none, "map_tax", (taxes.clone(),))
        .await
        .unwrap();
    assert_eq!(got, taxes);
    assert_eq!(
        *log.lock().unwrap(),
        vec![format!(
            "account.fiscal.position map_tax args {} super {}",
            serde_json::to_string(&(taxes.clone(),)).unwrap(),
            serde_json::to_string(&taxes).unwrap()
        )]
    );
    tx.rollback().await.unwrap();
}

#[test]
fn the_gap_report_names_seals_and_no_core_gap() {
    for (model, method) in [
        ("res.users", "_set_password"),
        ("res.users", "_check_credentials"),
        ("res.users", "init"),
        ("res.users.apikeys", "generate"),
        ("ir.access", "create"),
        ("res.groups", "write"),
        ("base", "init"),
        ("base", "_register_hook"),
    ] {
        assert_eq!(
            report(model, "base", method).form,
            "sealed",
            "{model}.{method}"
        );
    }
    // the six methods that were named gaps have codecs now (`wire.rs`): like `create` and
    // `write`, a core method with a codec has no row in the report
    let rows = wire_report();
    for method in CORE_CODEC_METHODS {
        assert!(
            !rows
                .iter()
                .any(|r| r.model == "base" && r.addon == "base" && r.method == method),
            "base.{method} is still in the gap report"
        );
    }
}

/// The refusal of a crossing chain over a sealed native entry, as the dispatch build words it:
/// `entries` are the clauses naming each seal.
fn sealed_refusal(model: &str, method: &str, entries: &str) -> String {
    format!(
        "method '{method}' of model '{model}' has a WebAssembly implementation, but its native \
         implementation is sealed ({entries}): a sealed method never runs in a chain with a package"
    )
}

#[tokio::test]
async fn a_wasm_layer_over_a_constraint_is_refused_naming_the_constraint_seal() {
    let h = harness().await;
    let row = report(MODEL, lower::ADDON.name, "_check_seam_name");
    assert_eq!(row.form, "sealed (constraint)");
    let log = Arc::new(Mutex::new(Vec::new()));
    let err = with_wasm(&h, vec![stand_in(MODEL, "_check_seam_name", &log)])
        .err()
        .expect("the build must refuse");
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        sealed_refusal(
            MODEL,
            "_check_seam_name",
            &format!(
                "{}, sealed as a constraint at {} (a package overrides constraints only from \
                 WASM.COMPUTE-REL on)",
                native_location(&h, MODEL, "_check_seam_name", lower::ADDON.name),
                row.location
            )
        )
    );
}

#[tokio::test]
async fn a_wasm_layer_over_an_onchange_is_refused_naming_the_onchange_seal() {
    let h = harness().await;
    let row = report(MODEL, lower::ADDON.name, "_onchange_seam_name");
    assert_eq!(row.form, "sealed (onchange)");
    let log = Arc::new(Mutex::new(Vec::new()));
    let err = with_wasm(&h, vec![stand_in(MODEL, "_onchange_seam_name", &log)])
        .err()
        .expect("the build must refuse");
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        sealed_refusal(
            MODEL,
            "_onchange_seam_name",
            &format!(
                "{}, sealed as an onchange at {} (a package overrides onchange methods only from \
                 WASM.ONCHANGE on)",
                native_location(&h, MODEL, "_onchange_seam_name", lower::ADDON.name),
                row.location
            )
        )
    );
}

#[tokio::test]
async fn a_wasm_layer_over_the_view_check_is_refused() {
    let h = harness().await;
    let row = report("ir.ui.view", "base", "_check_xml");
    assert_eq!(row.form, "sealed");
    let log = Arc::new(Mutex::new(Vec::new()));
    let err = with_wasm(&h, vec![stand_in("ir.ui.view", "_check_xml", &log)])
        .err()
        .expect("the build must refuse");
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        sealed_refusal(
            "ir.ui.view",
            "_check_xml",
            &format!(
                "{}, sealed at {}",
                native_location(&h, "ir.ui.view", "_check_xml", "base"),
                row.location
            )
        )
    );
}

#[tokio::test]
async fn a_wasm_layer_over_action_archive_of_users_is_refused() {
    let h = harness().await;
    let list = model_seal_location("res.users", "action_archive")
        .expect("res.users.action_archive is on the core list");
    let log = Arc::new(Mutex::new(Vec::new()));
    let err = with_wasm(&h, vec![stand_in("res.users", "action_archive", &log)])
        .err()
        .expect("the build must refuse");
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        sealed_refusal(
            "res.users",
            "action_archive",
            &format!("'action_archive' is sealed on model 'res.users' by the core list at {list}")
        )
    );
    // the list covers the access models and leaves an ordinary model open
    for model in [
        "res.users",
        "res.groups",
        "ir.access",
        "res.users.apikeys",
        "ir.config_parameter",
    ] {
        for method in [
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
            "exists",
            "filtered_domain",
        ] {
            assert!(
                model_seal_location(model, method).is_some(),
                "{model}.{method}"
            );
        }
    }
    assert!(model_seal_location(MODEL, "action_archive").is_none());
    // every core method that has a codec since WASM.WIRE-NATIVE.CORE-CODECS is on the list
    for method in CORE_CODEC_METHODS {
        assert!(
            model_seal_location("res.users", method).is_some(),
            "res.users.{method}"
        );
        assert!(model_seal_location(MODEL, method).is_none(), "{method}");
    }
}

#[test]
fn the_review_additions_are_sealed() {
    // mail is linked here only so its wire records are in the report
    let _ = &erp_addon_mail::ADDON;
    for (model, addon, method) in [
        ("ir.ui.view", "base", "_check_xml"),
        ("ir.ui.view", "base", "_validate_module_views"),
        ("ir.ui.view", "base", "_inverse_arch"),
        ("ir.cron", "base", "_notify_admin"),
        ("decimal.precision", "base", "_precision_get"),
        ("res.currency", "base", "_round_monetary"),
        ("ir.config_parameter", "base", "get_str"),
        ("ir.config_parameter", "base", "get_int"),
        ("ir.config_parameter", "base", "get_float"),
        ("ir.config_parameter", "base", "get_bool"),
        ("ir.config_parameter", "base", "set_str"),
        ("ir.config_parameter", "base", "set_int"),
        ("ir.config_parameter", "base", "set_float"),
        ("ir.config_parameter", "base", "set_bool"),
        ("mail.thread", "mail", "_mail_post_access"),
        ("res.users", "mail", "_mail_post_access"),
        ("res.company", "mail", "_mail_post_access"),
        ("res.partner", "mail", "_mail_post_access"),
    ] {
        assert_eq!(
            report(model, addon, method).form,
            "sealed",
            "{addon}'s {model}.{method}"
        );
    }
    // base's constraints and onchange handlers are sealed whether or not they say so
    assert_eq!(
        report("res.users", "base", "_onchange_role").form,
        "sealed (onchange)"
    );
    assert_eq!(
        report("res.groups", "base", "_check_disjoint_groups").form,
        "sealed (constraint)"
    );
}

// ---- the core's record, value-map, domain and upsert methods (WASM.WIRE-NATIVE.CORE-CODECS) -----

/// The six core methods whose codecs came with WASM.WIRE-NATIVE.CORE-CODECS.
const CORE_CODEC_METHODS: [&str; 6] = [
    "copy",
    "copy_data",
    "default_get",
    "exists",
    "filtered_domain",
    "upsert",
];

/// One call the stand-in saw: `model method`, the wire arguments it got and what its `super()`
/// answered, both as JSON documents (so the comparison does not depend on the order of keys).
type Seen = (String, serde_json::Value, serde_json::Value);

fn seen(model: &str, method: &str, args: serde_json::Value, answer: serde_json::Value) -> Seen {
    (format!("{model} {method}"), args, answer)
}

/// Take the stand-in's log, parsed, keeping the calls of `method` on `model`. A core method runs
/// other chains on its way (`copy` runs `copy_data` and `create`, `create` runs `default_get`):
/// those entries are dropped with the rest of the log.
fn logged(log: &Arc<Mutex<Vec<String>>>, model: &str, method: &str) -> Vec<Seen> {
    let lines = std::mem::take(&mut *log.lock().unwrap());
    lines
        .iter()
        .map(|line| {
            let (head, rest) = line.split_once(" args ").expect("a stand-in log line");
            let (args, answer) = rest.split_once(" super ").expect("a stand-in log line");
            (
                head.to_owned(),
                serde_json::from_str(args).expect("wire arguments are JSON"),
                serde_json::from_str(answer).expect("a wire answer is JSON"),
            )
        })
        .filter(|(head, _, _)| *head == format!("{model} {method}"))
        .collect()
}

/// A values object in the wire form, value by value as the core's codecs write it.
fn wire_values(map: &ValueMap) -> serde_json::Value {
    serde_json::Value::Object(
        map.iter()
            .map(|(name, value)| (name.clone(), wasm_abi1_value_to_json(value).unwrap()))
            .collect(),
    )
}

fn ids(records: &Records) -> Vec<i64> {
    records.rec_ids().iter().map(|id| id.get()).collect()
}

/// A WebAssembly layer that does not call `super()` and answers a fixed document.
#[derive(Debug)]
struct Fixed(&'static str);

impl MethodInvoker for Fixed {
    fn addon(&self) -> &str {
        layer::ADDON.name
    }

    fn kind(&self) -> ImplKind {
        ImplKind::Override
    }

    fn invoke<'a>(
        &'a self,
        _tx: &'a mut Transaction,
        _records: &'a Records,
        _args_json: &'a str,
        _sup: DynSuper,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Ok(self.0.to_owned()) })
    }
}

/// A WebAssembly layer that does not call `super()` and answers the id after the first record it
/// ran on: a record that is not in the set.
#[derive(Debug)]
struct Outside;

impl MethodInvoker for Outside {
    fn addon(&self) -> &str {
        layer::ADDON.name
    }

    fn kind(&self) -> ImplKind {
        ImplKind::Override
    }

    fn invoke<'a>(
        &'a self,
        _tx: &'a mut Transaction,
        records: &'a Records,
        _args_json: &'a str,
        _sup: DynSuper,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let first = records.rec_ids().first().expect("called on a record").get();
            Ok(serde_json::to_string(&[first + 1]).unwrap())
        })
    }
}

/// A WebAssembly layer that calls `super()` and then answers what `lie` makes of the records it ran
/// on and of the super answer.
#[derive(Debug)]
struct LiesAfterSuper(fn(&[i64], &str) -> String);

impl MethodInvoker for LiesAfterSuper {
    fn addon(&self) -> &str {
        layer::ADDON.name
    }

    fn kind(&self) -> ImplKind {
        ImplKind::Override
    }

    fn invoke<'a>(
        &'a self,
        tx: &'a mut Transaction,
        records: &'a Records,
        args_json: &'a str,
        sup: DynSuper,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let inner = sup.call_json(tx, records, args_json).await?;
            Ok((self.0)(&ids(records), &inner))
        })
    }
}

fn layer_of(model: &str, method: &str, invoker: Arc<dyn MethodInvoker>) -> WasmMethodImpl {
    WasmMethodImpl {
        model: model.to_owned(),
        method: method.to_owned(),
        invoker,
    }
}

/// A record id for the tests whose layer never reaches the database.
const ANY_ID: i64 = 7;

/// The upsert the tests send: one row by the fixture key, its `hits` accumulated.
fn keyed_call() -> UpsertCall {
    UpsertCall::new(KEYED_KEY)
        .with_rows([RawVals::new()
            .set_cell("code", Cell::Str(KEYED_CODE.into()))
            .set_cell("hits", Cell::Int(KEYED_HITS))])
        .with_accumulate("hits")
}

#[tokio::test]
async fn wasm_layers_over_the_record_and_value_methods_reach_the_core() {
    let h = harness().await;
    let log = Arc::new(Mutex::new(Vec::new()));
    let orm = with_wasm(
        &h,
        [
            "default_get",
            "exists",
            "filtered_domain",
            "copy_data",
            "copy",
        ]
        .into_iter()
        .map(|method| stand_in(MODEL, method, &log))
        .collect(),
    )
    .unwrap();
    let mut tx = orm.begin(h.principal()).await.unwrap();
    let model = tx.env().records(MODEL).unwrap();

    // default_get: the field names go down, a values object comes up
    let names = vec!["name".to_owned(), "active".to_owned()];
    let defaults: ValueMap = call_method(&mut tx, &model, "default_get", (names.clone(),))
        .await
        .unwrap();
    assert!(
        defaults.contains_key("active"),
        "the fixture declares a default for `active`"
    );
    assert_eq!(
        logged(&log, MODEL, "default_get"),
        vec![seen(
            MODEL,
            "default_get",
            serde_json::json!([names]),
            wire_values(&defaults)
        )]
    );

    let (id, _) = model.name_create(&mut tx, SEAM_NAME).await.unwrap();
    let record = model.browse([id]).unwrap();
    // whatever chains the creation ran through the layer is not what the next steps look at
    log.lock().unwrap().clear();

    // exists: nothing goes down, the ids that exist come up
    let existing: Records = call_method(&mut tx, &record, "exists", ()).await.unwrap();
    assert_eq!(ids(&existing), vec![id]);
    assert_eq!(
        logged(&log, MODEL, "exists"),
        vec![seen(
            MODEL,
            "exists",
            serde_json::Value::Null,
            serde_json::json!([id])
        )]
    );

    // filtered_domain: the domain goes down in list syntax, the matching ids come up
    let matching = Domain::from_json(&serde_json::json!([["name", "=", SEAM_NAME]])).unwrap();
    let other = Domain::from_json(&serde_json::json!([["name", "=", COPY_NAME]])).unwrap();
    let kept: Records = call_method(&mut tx, &record, "filtered_domain", (matching.clone(),))
        .await
        .unwrap();
    assert_eq!(ids(&kept), vec![id]);
    let dropped: Records = call_method(&mut tx, &record, "filtered_domain", (other.clone(),))
        .await
        .unwrap();
    assert!(dropped.rec_ids().is_empty());
    assert_eq!(
        logged(&log, MODEL, "filtered_domain"),
        vec![
            seen(
                MODEL,
                "filtered_domain",
                serde_json::json!([matching.to_json().unwrap()]),
                serde_json::json!([id])
            ),
            seen(
                MODEL,
                "filtered_domain",
                serde_json::json!([other.to_json().unwrap()]),
                serde_json::json!([])
            ),
        ]
    );

    // copy_data: no default goes down as null, one values object per record comes up
    let data: Vec<Option<ValueMap>> =
        call_method(&mut tx, &record, "copy_data", (None::<ValueMap>,))
            .await
            .unwrap();
    assert_eq!(data.len(), 1);
    let data_wire: Vec<serde_json::Value> = data
        .iter()
        .map(|entry| entry.as_ref().map_or(serde_json::Value::Null, wire_values))
        .collect();
    assert!(data_wire[0].is_object(), "the record has values to copy");
    assert_eq!(
        logged(&log, MODEL, "copy_data"),
        vec![seen(
            MODEL,
            "copy_data",
            serde_json::json!([null]),
            serde_json::Value::Array(data_wire)
        )]
    );

    // copy: the default goes down as a values object, the ids of the copies come up
    let default: ValueMap = [("name".to_owned(), Value::str(COPY_NAME))]
        .into_iter()
        .collect();
    let copies: Records = call_method(&mut tx, &record, "copy", (Some(default.clone()),))
        .await
        .unwrap();
    let copy_ids = ids(&copies);
    assert_eq!(copy_ids.len(), 1);
    assert_ne!(copy_ids[0], id);
    let read = copies.read(&mut tx, &["name"]).await.unwrap();
    assert_eq!(read[0]["name"], Value::str(COPY_NAME));
    assert_eq!(
        logged(&log, MODEL, "copy"),
        vec![seen(
            MODEL,
            "copy",
            serde_json::json!([wire_values(&default)]),
            serde_json::json!(copy_ids)
        )]
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_wasm_layer_over_upsert_reaches_the_core() {
    let h = harness().await;
    let log = Arc::new(Mutex::new(Vec::new()));
    let orm = with_wasm(&h, vec![stand_in(KEYED, "upsert", &log)]).unwrap();
    let mut tx = orm.begin(h.principal()).await.unwrap();
    let keyed = tx.env().records(KEYED).unwrap();

    let first = keyed.upsert(&mut tx, keyed_call()).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first.rows()[0].outcome(), UpsertOutcome::Inserted);
    let id = first.rows()[0].id();
    let second = keyed.upsert(&mut tx, keyed_call()).await.unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second.rows()[0].outcome(), UpsertOutcome::Updated);
    assert_eq!(second.rows()[0].id(), id);
    let read = keyed
        .browse_ids([id])
        .read(&mut tx, &["hits"])
        .await
        .unwrap();
    assert_eq!(read[0]["hits"], Value::Int(i64::from(KEYED_HITS) * 2));

    // the documented call object, and one `[id, outcome, previous_version, version]` per row
    let call = serde_json::json!([{
        "key": KEYED_KEY,
        "rows": [{"code": KEYED_CODE, "hits": KEYED_HITS}],
        "update": [],
        "accumulate": [["hits", null]],
    }]);
    let answer = |outcome: &str, row: &erp_orm::UpsertedRow| {
        serde_json::json!([[id.get(), outcome, row.previous_version(), row.version()]])
    };
    assert_eq!(
        logged(&log, KEYED, "upsert"),
        vec![
            seen(
                KEYED,
                "upsert",
                call.clone(),
                answer("inserted", &first.rows()[0])
            ),
            seen(KEYED, "upsert", call, answer("updated", &second.rows()[0])),
        ]
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_wasm_filter_answer_outside_its_records_is_refused() {
    let h = harness().await;
    for method in ["exists", "filtered_domain"] {
        let orm = with_wasm(&h, vec![layer_of(MODEL, method, Arc::new(Outside))]).unwrap();
        let mut tx = orm.begin(h.principal()).await.unwrap();
        // the layer answers without calling super(), so the record need not exist
        let id = ANY_ID;
        let record = tx.env().records(MODEL).unwrap().browse([id]).unwrap();
        let result: Result<Records> = if method == "exists" {
            call_method(&mut tx, &record, "exists", ()).await
        } else {
            let domain = Domain::from_json(&serde_json::json!([["name", "=", SEAM_NAME]])).unwrap();
            call_method(&mut tx, &record, "filtered_domain", (domain,)).await
        };
        let err = result.expect_err("an id outside the records is refused");
        assert_eq!(err.kind(), ErrorKind::Validation, "{method}");
        assert_eq!(
            err.message(),
            format!(
                "a WASM {method} returned the record id {}, which is not one of the records it \
                 ran on",
                id + 1
            )
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn an_upsert_answer_with_another_row_count_is_refused() {
    let h = harness().await;
    let orm = with_wasm(&h, vec![layer_of(KEYED, "upsert", Arc::new(Fixed("[]")))]).unwrap();
    let mut tx = orm.begin(h.principal()).await.unwrap();
    let keyed = tx.env().records(KEYED).unwrap();
    let call = keyed_call();
    let rows = call.rows().len();
    let err = keyed
        .upsert(&mut tx, call)
        .await
        .expect_err("a short answer is refused");
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        format!("upsert on '{KEYED}': an implementation answered 0 results for {rows} input rows")
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_field_without_an_abi_form_is_refused_in_copy_and_upsert() {
    let h = harness().await;
    let log = Arc::new(Mutex::new(Vec::new()));
    let orm = with_wasm(
        &h,
        vec![
            stand_in(KEYED, "copy", &log),
            stand_in(KEYED, "upsert", &log),
        ],
    )
    .unwrap();
    let expected = format!(
        "field '{KEYED}.big' (bigint) is not supported across frozen WASM ABI1; requires a future \
         ABI major with typed native fields"
    );

    // typed call into the crossing chain: the names of the default are checked
    let mut tx = orm.begin(h.principal()).await.unwrap();
    let keyed = tx.env().records(KEYED).unwrap();
    let default: ValueMap = [("big".to_owned(), Value::Int(1))].into_iter().collect();
    let result: Result<Records> = call_method(&mut tx, &keyed, "copy", (Some(default),)).await;
    let err = result.expect_err("the bigint field is refused");
    assert!(is_wasm_abi1_native_refusal(&err));
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(err.message(), expected);
    tx.rollback().await.unwrap();

    // wire-form call: the rows of the call object are checked
    let mut tx = orm.begin(h.principal()).await.unwrap();
    let keyed = tx.env().records(KEYED).unwrap();
    let args = serde_json::json!([{
        "key": KEYED_KEY,
        "rows": [{"code": KEYED_CODE, "big": 1}],
    }])
    .to_string();
    let err = call_method_json(&mut tx, &keyed, "upsert", &args)
        .await
        .expect_err("the bigint field is refused");
    assert!(is_wasm_abi1_native_refusal(&err));
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(err.message(), expected);
    tx.rollback().await.unwrap();

    assert!(log.lock().unwrap().is_empty(), "no layer ran");
}

#[tokio::test]
async fn malformed_wire_documents_of_the_core_methods_are_refused() {
    let h = harness().await;
    let mut tx = h.orm().begin(h.principal()).await.unwrap();
    let model = tx.env().records(MODEL).unwrap();
    let keyed = tx.env().records(KEYED).unwrap();
    // `exists` takes nothing: the parser's own words for a document that is not `null`
    let not_unit = serde_json::from_str::<()>("[1]").expect_err("a list is not null");
    let unknown_entry = serde_json::json!([{"key": KEYED_KEY, "mode": "x"}]).to_string();
    let bad_row = serde_json::json!([{"key": KEYED_KEY, "rows": [1]}]).to_string();
    for (records, method, args, message) in [
        (
            &model,
            "copy",
            "[5]",
            "the default of 'copy' must be a JSON object, got a number".to_owned(),
        ),
        (
            &model,
            "copy_data",
            "[[]]",
            "the default of 'copy_data' must be a JSON object, got a list".to_owned(),
        ),
        (
            &model,
            "copy",
            "[]",
            "arguments of 'copy' do not match its wire form: expected a list of one argument, \
             got a list of 0"
                .to_owned(),
        ),
        (
            &model,
            "exists",
            "[1]",
            format!("arguments of 'exists' do not match its wire form: {not_unit}"),
        ),
        (
            &keyed,
            "upsert",
            r#"[{"rows":[]}]"#,
            "the call of 'upsert' names no key (the name of a declared key)".to_owned(),
        ),
        (
            &keyed,
            "upsert",
            unknown_entry.as_str(),
            "the call of 'upsert' has the unknown entry 'mode' (it takes key, rows, update and \
             accumulate)"
                .to_owned(),
        ),
        (
            &keyed,
            "upsert",
            bad_row.as_str(),
            "a row of 'upsert' must be a JSON object, got a number".to_owned(),
        ),
    ] {
        let err = call_method_json(&mut tx, records, method, args)
            .await
            .err()
            .unwrap_or_else(|| panic!("{method} {args} must be refused"));
        assert_eq!(err.kind(), ErrorKind::Validation, "{method} {args}");
        assert_eq!(err.message(), message, "{method} {args}");
    }
    tx.rollback().await.unwrap();
}

/// A WebAssembly layer that does not call `super()` and answers the first record it ran on twice.
#[derive(Debug)]
struct Twice;

impl MethodInvoker for Twice {
    fn addon(&self) -> &str {
        layer::ADDON.name
    }

    fn kind(&self) -> ImplKind {
        ImplKind::Override
    }

    fn invoke<'a>(
        &'a self,
        _tx: &'a mut Transaction,
        records: &'a Records,
        _args_json: &'a str,
        _sup: DynSuper,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let first = records.rec_ids().first().expect("called on a record").get();
            Ok(serde_json::to_string(&[first, first]).unwrap())
        })
    }
}

#[tokio::test]
async fn a_wasm_filter_answer_with_a_repeated_id_is_refused() {
    let h = harness().await;
    let orm = with_wasm(&h, vec![layer_of(MODEL, "exists", Arc::new(Twice))]).unwrap();
    for (ran_on, message) in [
        (
            vec![ANY_ID, ANY_ID + 1],
            format!("a WASM exists returned the record id {ANY_ID} more than once"),
        ),
        (
            vec![ANY_ID],
            "a WASM exists returned 2 record ids, more than the 1 records it ran on".to_owned(),
        ),
    ] {
        let mut tx = orm.begin(h.principal()).await.unwrap();
        let records = tx.env().records(MODEL).unwrap().browse(ran_on).unwrap();
        let result: Result<Records> = call_method(&mut tx, &records, "exists", ()).await;
        let err = result.expect_err("a repeated id is refused");
        assert_eq!(err.kind(), ErrorKind::Validation);
        assert_eq!(err.message(), message);
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn wasm_answers_that_do_not_fit_the_call_are_refused() {
    /// A field the layer answers a default for, and the one the test asks for.
    const ANSWERED: &str = "name";
    const ASKED: &str = "active";
    let h = harness().await;
    let orm = with_wasm(
        &h,
        vec![
            layer_of(MODEL, "copy", Arc::new(Fixed("[]"))),
            layer_of(MODEL, "copy_data", Arc::new(Fixed("[]"))),
            layer_of(MODEL, "default_get", Arc::new(Fixed(r#"{"name":"x"}"#))),
        ],
    )
    .unwrap();

    let mut tx = orm.begin(h.principal()).await.unwrap();
    let record = tx.env().records(MODEL).unwrap().browse([ANY_ID]).unwrap();
    let result: Result<Records> = call_method(&mut tx, &record, "copy", (None::<ValueMap>,)).await;
    let err = result.expect_err("no copy for one record is refused");
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        "a WASM copy returned 0 records for the 1 records it ran on"
    );
    tx.rollback().await.unwrap();

    let mut tx = orm.begin(h.principal()).await.unwrap();
    let record = tx.env().records(MODEL).unwrap().browse([ANY_ID]).unwrap();
    let result: Result<Vec<Option<ValueMap>>> =
        call_method(&mut tx, &record, "copy_data", (None::<ValueMap>,)).await;
    let err = result.expect_err("no entry for one record is refused");
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        "a WASM copy_data returned 0 entries for the 1 records it ran on"
    );
    tx.rollback().await.unwrap();

    let mut tx = orm.begin(h.principal()).await.unwrap();
    let model = tx.env().records(MODEL).unwrap();
    let result: Result<ValueMap> =
        call_method(&mut tx, &model, "default_get", (vec![ASKED.to_owned()],)).await;
    let err = result.expect_err("a field that was not asked for is refused");
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        format!(
            "a WASM default_get answered a value for '{ANSWERED}', which is not one of the \
             fields it was asked for"
        )
    );
    tx.rollback().await.unwrap();
}

/// A lying answer: the ids the layer ran on and the super's answer in, the layer's answer out.
type Lie = fn(&[i64], &str) -> String;

#[tokio::test]
async fn a_wasm_copy_answering_records_its_super_did_not_make_is_refused() {
    let h = harness().await;
    let lies: [(&str, Lie); 3] = [
        ("the source", |ran_on, _| {
            serde_json::json!(ran_on).to_string()
        }),
        ("a stranger", |_, sup| {
            let made: Vec<i64> = serde_json::from_str(sup).unwrap();
            serde_json::json!([made[0] + 1000]).to_string()
        }),
        ("an honest answer", |_, sup| sup.to_owned()),
    ];
    for (what, lie) in lies {
        let orm = with_wasm(
            &h,
            vec![layer_of(MODEL, "copy", Arc::new(LiesAfterSuper(lie)))],
        )
        .unwrap();
        let mut tx = orm.begin(h.principal()).await.unwrap();
        let model = tx.env().records(MODEL).unwrap();
        let (id, _) = model.name_create(&mut tx, SEAM_NAME).await.unwrap();
        let record = model.browse([id]).unwrap();
        let result: Result<Records> =
            call_method(&mut tx, &record, "copy", (None::<ValueMap>,)).await;
        if what == "an honest answer" {
            assert_eq!(ids(&result.expect("an honest copy passes")).len(), 1);
            tx.rollback().await.unwrap();
            continue;
        }
        let err = result.expect_err(what);
        assert_eq!(err.kind(), ErrorKind::Validation, "{what}: {err}");
        assert!(
            err.message().contains("must answer exactly those"),
            "{what}: {err}"
        );
        assert!(
            tx.commit().await.is_err(),
            "{what}: the transaction must not commit"
        );
    }
}

#[tokio::test]
async fn a_wasm_upsert_answering_a_foreign_record_is_refused() {
    let h = harness().await;
    let foreign: fn(&[i64], &str) -> String = |_, sup| {
        let mut rows: Vec<serde_json::Value> = serde_json::from_str(sup).unwrap();
        let id = rows[0][0].as_i64().unwrap();
        rows[0][0] = serde_json::json!(id + 1000);
        serde_json::Value::Array(rows).to_string()
    };
    let orm = with_wasm(
        &h,
        vec![layer_of(KEYED, "upsert", Arc::new(LiesAfterSuper(foreign)))],
    )
    .unwrap();
    let mut tx = orm.begin(h.principal()).await.unwrap();
    let keyed = tx.env().records(KEYED).unwrap();
    let err = keyed
        .upsert(&mut tx, keyed_call())
        .await
        .expect_err("a foreign id is refused");
    assert_eq!(err.kind(), ErrorKind::Validation, "{err}");
    assert!(err.message().contains("must answer exactly those"), "{err}");
    assert!(
        tx.commit().await.is_err(),
        "the transaction must not commit"
    );
}
