// SPDX-License-Identifier: MIT OR Apache-2.0
//! Batch helper execution and its caller/dependency boundary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
use erp_expr::EvalContext;
use erp_fields::{ErrorKind, Value};
use erp_orm::{
    Records, Transaction,
    report_helpers::{REPORT_HELPERS, ReportHelperDecl},
    rpc::{BindError, ParamSpec, RPC_IMPLS, Receiver, RpcArgs, RpcDecl, RpcImpl},
};
use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl};
use erp_report::{
    DocumentOptions, EvalOptions, Language, Limits, Node, PageSetup, PlanContext, ReportLimits,
    ReportProgram, TemplateSource,
};
use erp_report_data::{DataError, field_info, load, report_helpers};
use erp_test_models::base_users;
use erp_testkit::Harness;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

static ADDON: AddonDescriptor = AddonDescriptor {
    name: "x_report_helpers",
    depends: &["base"],
    exact_only: false,
};
const MODEL: &str = "x_report_helpers.doc";
static FIELDS: &[FieldDecl] = &[FieldDecl::char("name"), FieldDecl::char("secret")];
static CALLS: AtomicUsize = AtomicUsize::new(0);
#[linkme::distributed_slice(REPORT_HELPERS)]
static HELPER: ReportHelperDecl =
    ReportHelperDecl::new("x_report_helpers", MODEL, "_label", &["name"]);
#[linkme::distributed_slice(RPC_IMPLS)]
static CALLABLE: RpcImpl = RpcImpl {
    addon: "x_report_helpers",
    model: MODEL,
    method: "_label",
    decl: RpcDecl::DataCallable {
        receiver: Receiver::Records,
        params: &[
            ParamSpec::new("field", true, "a string"),
            ParamSpec::new("mode", true, "a string"),
        ],
        bind,
    },
    file: file!(),
    line: line!(),
};
fn bind<'a>(
    tx: &'a mut Transaction,
    records: Records,
    mut args: RpcArgs,
) -> Pin<Box<dyn Future<Output = Result<Value, BindError>> + Send + 'a>> {
    Box::pin(async move {
        let field: String = args.take("field")?;
        let mode: String = args.take("mode")?;
        if mode == "count" {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
        if mode == "raw" {
            tx.query("SELECT 1", &[]).await.map_err(BindError::Call)?;
        }
        if mode == "swallow" {
            let _caught = records.values(tx, "secret").await;
        }
        if mode == "context" {
            records
                .with_context([("lang", Value::str("fr_FR"))])
                .values(tx, "name")
                .await
                .map_err(BindError::Call)?;
        }
        if mode == "sudo" {
            records
                .sudo(!records.env().su())
                .values(tx, "name")
                .await
                .map_err(BindError::Call)?;
        }
        if mode == "write" {
            records
                .write_values(
                    tx,
                    &BTreeMap::from([("name".into(), Value::str("changed"))]),
                )
                .await
                .map_err(BindError::Call)?;
        }
        if mode == "commit" {
            tx.commit().await.map_err(BindError::Call)?;
        }
        if mode == "nested" {
            tx.report_helper_scope(&records, "_label", Vec::new())
                .map_err(BindError::Call)?
                .finish()
                .map_err(BindError::Call)?;
        }
        let values = records.values(tx, &field).await.map_err(BindError::Call)?;
        if mode == "outside" {
            return Ok(Value::list(
                records
                    .browse([records.ids()[0] + 1])
                    .map_err(BindError::Call)?
                    .values(tx, "name")
                    .await
                    .map_err(BindError::Call)?,
            ));
        }
        if mode == "short" {
            return Ok(Value::list([]));
        }
        Ok(Value::list(values))
    })
}
async fn fixture() -> (Harness, Vec<i64>) {
    fixture_with_policy(false).await
}
async fn fixture_with_policy(denied: bool) -> (Harness, Vec<i64>) {
    let mut policy = erp_testkit::DomainPolicy::builder(erp_testkit::Otherwise::Allow);
    if denied {
        policy = policy.deny(MODEL, erp_orm::Operation::Read);
    }
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls([ModelDecl::new("x_report_helpers", MODEL)
            .description("Helper Document")
            .fields(FIELDS)])
        .policy(policy.build().unwrap())
        .build()
        .await
        .unwrap();
    let mut tx = h.begin().await.unwrap();
    let rows: Vec<_> = (0..32)
        .map(|i| {
            BTreeMap::from([
                ("name".into(), Value::str(format!("Name{i}"))),
                ("secret".into(), Value::str("hidden")),
            ])
        })
        .collect();
    let ids = tx
        .env()
        .records(MODEL)
        .unwrap()
        .create_values(&mut tx, &rows)
        .await
        .unwrap()
        .ids();
    tx.commit().await.unwrap();
    (h, ids)
}
fn program(field: &str, mode: &str) -> ReportProgram {
    let arch = format!(
        "<div><t t-foreach='docs' t-as='o'><span t-out=\"o._label('{field}', mode='{mode}')\"/></t></div>"
    );
    ReportProgram::compile(
        "test.helpers",
        &[TemplateSource::new("test.helpers", &arch)],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap()
}
#[tokio::test]
async fn helpers_run_once_per_batch_and_evaluation_reads_only_results() {
    let (h, ids) = fixture().await;
    let mut counts = Vec::new();
    for count in [1, 32] {
        let mut tx = h.begin().await.unwrap();
        let env = tx.env();
        let program = program("name", "count");
        let helpers = report_helpers(&tx);
        let plan = program
            .prefetch_plan(&PlanContext::new(MODEL).with_helpers(&helpers), &|m, f| {
                field_info(h.registry(), m, f)
            })
            .unwrap();
        let calls = CALLS.load(Ordering::SeqCst);
        let before = tx.query_count();
        let store = Arc::new(
            load(
                &mut tx,
                &env,
                &plan,
                &[("docs", &ids[..count])],
                ReportLimits::default(),
                Limits::default(),
            )
            .await
            .unwrap(),
        );
        counts.push(tx.query_count() - before);
        assert_eq!(CALLS.load(Ordering::SeqCst) - calls, 1);
        let context =
            EvalContext::standard().with_var("docs", store.recordset(MODEL, &ids[..count]));
        let page = PageSetup::new(595.0, 842.0, 0.0).unwrap();
        let doc = program
            .evaluate(
                &context,
                &store,
                &EvalOptions::default(),
                DocumentOptions::new("test", "test", page),
            )
            .unwrap();
        let text: String = doc
            .nodes()
            .iter()
            .filter_map(|n| {
                if let Node::Text(s) = n {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            text,
            (0..count).map(|i| format!("Name{i}")).collect::<String>()
        );
        assert_eq!(tx.query_count() - before, *counts.last().unwrap());
        assert_eq!(CALLS.load(Ordering::SeqCst) - calls, 1);
    }
    assert_eq!(counts[0], counts[1]);
    h.cleanup().await.unwrap();
}
#[tokio::test]
async fn helpers_refuse_undeclared_reads_even_on_a_warm_cache_and_swallowed_errors() {
    let (h, ids) = fixture().await;
    for warm in [false, true] {
        for mode in ["normal", "swallow"] {
            let mut tx = h.begin().await.unwrap();
            let env = tx.env();
            if warm {
                env.records(MODEL)
                    .unwrap()
                    .browse(ids.clone())
                    .unwrap()
                    .read(&mut tx, &["secret"])
                    .await
                    .unwrap();
            }
            let program = program(if mode == "swallow" { "name" } else { "secret" }, mode);
            let helpers = report_helpers(&tx);
            let plan = program
                .prefetch_plan(&PlanContext::new(MODEL).with_helpers(&helpers), &|m, f| {
                    field_info(h.registry(), m, f)
                })
                .unwrap();
            let err = load(
                &mut tx,
                &env,
                &plan,
                &[("docs", &ids)],
                ReportLimits::default(),
                Limits::default(),
            )
            .await
            .unwrap_err();
            match err {
                DataError::Orm(e) => {
                    assert_eq!(e.kind(), ErrorKind::Programming);
                    assert_eq!(
                        e.message(),
                        "Report helper x_report_helpers.doc._label: undeclared read of x_report_helpers.doc.secret"
                    );
                }
                e => panic!("{e}"),
            }
            assert!(
                env.records(MODEL)
                    .unwrap()
                    .browse(ids.clone())
                    .unwrap()
                    .values(&mut tx, "secret")
                    .await
                    .is_ok(),
                "scope restored"
            );
        }
    }
    h.cleanup().await.unwrap();
}
#[tokio::test]
async fn helpers_refuse_raw_sql_and_wrong_result_count() {
    let (h, ids) = fixture().await;
    for mode in [
        "raw", "short", "context", "sudo", "write", "commit", "nested",
    ] {
        let principal = erp_orm::PrincipalBuilder::user(erp_orm::UserId::new(2).unwrap())
            .resolution(
                erp_orm::Resolution::in_id_order(
                    erp_orm::GroupSet::default(),
                    [],
                    std::num::NonZeroU64::MIN,
                )
                .unwrap(),
            )
            .build()
            .unwrap();
        let mut tx = h
            .orm()
            .begin_opts(erp_orm::BeginOptions::for_principal(principal))
            .await
            .unwrap();
        let env = tx.env();
        assert!(!env.su());
        let program = program("name", mode);
        let helpers = report_helpers(&tx);
        let plan = program
            .prefetch_plan(&PlanContext::new(MODEL).with_helpers(&helpers), &|m, f| {
                field_info(h.registry(), m, f)
            })
            .unwrap();
        let err = load(
            &mut tx,
            &env,
            &plan,
            &[("docs", &ids)],
            ReportLimits::default(),
            Limits::default(),
        )
        .await
        .unwrap_err();
        if mode != "short" {
            match err {
                DataError::Orm(e) => {
                    assert_eq!(e.kind(), ErrorKind::Programming);
                    assert_eq!(
                        e.message(),
                        "Report helper x_report_helpers.doc._label may only read its declared, prefetched fields in the caller environment"
                    );
                }
                e => panic!("{e}"),
            }
        } else {
            assert!(
                matches!(err, DataError::Report(erp_report::ReportError::Data { .. })),
                "{err}"
            );
            assert!(err.to_string().contains("_label"), "{err}");
        }
    }
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn helper_reads_preserve_caller_access_and_scope_cancellation() {
    use erp_orm::{BeginOptions, GroupSet, PrincipalBuilder, Resolution, UserId};
    use std::num::NonZeroU64;
    for denied in [false, true] {
        let (h, ids) = fixture_with_policy(denied).await;
        let principal = PrincipalBuilder::user(UserId::new(2).unwrap())
            .resolution(Resolution::in_id_order(GroupSet::default(), [], NonZeroU64::MIN).unwrap())
            .build()
            .unwrap();
        let mut tx = h
            .orm()
            .begin_opts(BeginOptions::for_principal(principal))
            .await
            .unwrap();
        let env = tx.env();
        assert!(!env.su());
        let program = program("name", "normal");
        let helpers = report_helpers(&tx);
        let plan = program
            .prefetch_plan(&PlanContext::new(MODEL).with_helpers(&helpers), &|m, f| {
                field_info(h.registry(), m, f)
            })
            .unwrap();
        let result = load(
            &mut tx,
            &env,
            &plan,
            &[("docs", &ids)],
            ReportLimits::default(),
            Limits::default(),
        )
        .await;
        if denied {
            match result.unwrap_err() {
                DataError::Orm(e) => {
                    assert_eq!(e.kind(), ErrorKind::Access);
                    assert!(e.message().contains("Helper Document"), "{e}");
                }
                e => panic!("{e}"),
            }
        } else {
            assert_eq!(result.unwrap().len(), ids.len());
            let records = env.records(MODEL).unwrap().browse(ids.clone()).unwrap();
            {
                let pending = async {
                    let _scope = tx
                        .report_helper_scope(&records, "_label", Vec::new())
                        .unwrap();
                    std::future::pending::<()>().await;
                };
                tokio::pin!(pending);
                tokio::select! {biased; _=&mut pending=>panic!("unexpected completion"), _=std::future::ready(())=>{}}
            }
            assert!(
                records.values(&mut tx, "secret").await.is_ok(),
                "cancellation restores scope"
            );
        }
        drop(tx);
        h.cleanup().await.unwrap();
    }
}

#[tokio::test]
async fn helper_cannot_read_another_warm_record_or_use_forged_dependencies() {
    let (h, ids) = fixture().await;
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    env.records(MODEL)
        .unwrap()
        .browse(ids.clone())
        .unwrap()
        .values(&mut tx, "name")
        .await
        .unwrap();
    let program = program("name", "outside");
    let helpers = report_helpers(&tx);
    let plan = program
        .prefetch_plan(&PlanContext::new(MODEL).with_helpers(&helpers), &|m, f| {
            field_info(h.registry(), m, f)
        })
        .unwrap();
    let error = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", &ids[..1])],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap_err();
    match error {
        DataError::Orm(e) => {
            assert_eq!(e.kind(), ErrorKind::Programming);
            assert!(
                e.message().contains("_label") && e.message().contains("undeclared read"),
                "{e}"
            );
        }
        e => panic!("{e}"),
    }
    let helpers = [erp_report::ReportHelper::new(MODEL, "_label", &["secret"])
        .with_parameters(&["field", "mode"])];
    let plan = program
        .prefetch_plan(&PlanContext::new(MODEL).with_helpers(&helpers), &|m, f| {
            field_info(h.registry(), m, f)
        })
        .unwrap();
    let error = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", &ids[..1])],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            DataError::Report(erp_report::ReportError::Data { .. })
        ),
        "{error}"
    );
    assert!(
        error.to_string().contains("_label")
            && error.to_string().contains("differs from the registry"),
        "{error}"
    );
    drop(tx);
    h.cleanup().await.unwrap();
}
