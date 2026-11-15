// SPDX-License-Identifier: MIT OR Apache-2.0
//! The automatic wire form of native methods (WASM.WIRE-NATIVE,
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
    call_method, model_seal_location, wire_report,
};
use erp_sdk::ErrorKind;
use erp_sdk::prelude::*;
use erp_testkit::Harness;

const MODEL: &str = "test.wire.seam";

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
    use super::lower::Seam;
    use super::*;

    erp_sdk::addon!(
        name = "test_wire_layer",
        depends = ["test_wire_lower", "account"]
    );

    #[extend(Seam, addon = "test_wire_layer")]
    pub struct SeamLayer {}

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
        &erp_addon_uom::ADDON,
        &erp_addon_product::ADDON,
        &erp_addon_analytic::ADDON,
        &erp_addon_account::ADDON,
        &lower::ADDON,
        &layer::ADDON,
        &upper::ADDON,
    ])
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
fn the_gap_report_names_seals_and_gaps() {
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
    assert_eq!(
        report("base", "base", "default_get").form,
        "gap: `ValueMap` (result)"
    );
    assert_eq!(
        report("base", "base", "upsert").form,
        "gap: `UpsertCall` (argument 1), `UpsertResult` (result)"
    );
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
        ] {
            assert!(
                model_seal_location(model, method).is_some(),
                "{model}.{method}"
            );
        }
    }
    assert!(model_seal_location(MODEL, "action_archive").is_none());
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
