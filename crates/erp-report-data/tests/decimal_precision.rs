// SPDX-License-Identifier: MIT OR Apache-2.0
//! Named float precision is loaded once per report batch, under the printing user's rights.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use erp_domain::{Domain, Operator, Value as DomainValue};
use erp_expr::EvalContext;
use erp_fields::{ErrorKind, Value};
use erp_orm::{BeginOptions, FieldOp, GroupSet, Operation, PrincipalBuilder, Resolution, UserId};
use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl, Registry};
use erp_report::{
    DocumentOptions, EvalOptions, Language, Limits, Node, PageSetup, PlanContext, PrefetchPlan,
    RecordStore, ReportError, ReportLimits, ReportProgram, TemplateSource,
};
use erp_report_data::{DataError, field_info, load};
use erp_test_models::base_users;
use erp_testkit::{DomainPolicy, FieldPolicy, Harness, Otherwise, assert_query_count};

static ADDON: AddonDescriptor = AddonDescriptor {
    name: "x_report_precision",
    depends: &["base"],
    exact_only: false,
};
const DOC: &str = "x_report_precision.doc";
const PRECISION: &str = "decimal.precision";
const DEMO: i64 = 2;
const NAMED: &str = "<div><t t-foreach=\"docs\" t-as=\"o\"><span t-field=\"o.amount\" \
    t-options=\"{'widget': 'float', 'decimal_precision': 'Product Unit', 'precision': 1}\"/>\
    <span t-out=\"o.amount\" t-options=\"{'widget': 'float', 'decimal_precision': 'Package'}\"/>\
    <span t-out=\"o.amount\" t-options=\"{'widget': 'float', 'decimal_precision': 'Product Unit'}\"/>\
    </t></div>";
const SINGLE: &str = "<div><t t-foreach=\"docs\" t-as=\"o\"><span t-field=\"o.amount\" \
    t-options=\"{'widget': 'float', 'decimal_precision': 'Product Unit'}\"/></t></div>";

static DOC_FIELDS: &[FieldDecl] = &[FieldDecl::char("name"), FieldDecl::float("amount")];
static PRECISION_FIELDS: &[FieldDecl] = &[FieldDecl::char("name"), FieldDecl::integer("digits")];

fn decls() -> [ModelDecl; 2] {
    [
        ModelDecl::new("x_report_precision", DOC)
            .description("Precision Report Document")
            .fields(DOC_FIELDS),
        ModelDecl::new("x_report_precision", PRECISION)
            .description("Decimal Precision")
            .fields(PRECISION_FIELDS),
    ]
}

fn report(registry: &Registry, arch: &str) -> (ReportProgram, PrefetchPlan) {
    let program = ReportProgram::compile(
        "test.precision",
        &[TemplateSource::new("test.precision", arch)],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap();
    let plan = program
        .prefetch_plan(&PlanContext::new(DOC), &|model: &str, field: &str| {
            field_info(registry, model, field)
        })
        .unwrap();
    (program, plan)
}

fn printed(program: &ReportProgram, store: RecordStore, docs: &[i64]) -> String {
    let store = Arc::new(store);
    let context = EvalContext::standard().with_var("docs", store.recordset(DOC, docs));
    program
        .evaluate(
            &context,
            &store,
            &EvalOptions::default(),
            DocumentOptions::new(
                "precision",
                "precision",
                PageSetup::new(595.0, 842.0, 0.0).unwrap(),
            ),
        )
        .unwrap()
        .nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Text(text) => Some(text.as_str()),
            // Document layout nodes carry no printed text.
            _ => None,
        })
        .collect()
}

fn principal() -> erp_orm::Principal {
    PrincipalBuilder::user(UserId::new(DEMO).unwrap())
        .resolution(Resolution::in_id_order(GroupSet::default(), [], NonZeroU64::MIN).unwrap())
        .build()
        .unwrap()
}

async fn seed(h: &Harness, count: usize) -> (Vec<i64>, i64) {
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    let precisions = env
        .records(PRECISION)
        .unwrap()
        .create_values(
            &mut tx,
            &[
                BTreeMap::from([
                    ("name".into(), Value::str("Product Unit")),
                    ("digits".into(), Value::Int(3)),
                ]),
                BTreeMap::from([
                    ("name".into(), Value::str("Package")),
                    ("digits".into(), Value::Int(1)),
                ]),
            ],
        )
        .await
        .unwrap()
        .ids();
    let rows: Vec<_> = (0..count)
        .map(|n| {
            BTreeMap::from([
                ("name".into(), Value::str(format!("Document {n}"))),
                ("amount".into(), Value::Float(12.375)),
            ])
        })
        .collect();
    let docs = env
        .records(DOC)
        .unwrap()
        .create_values(&mut tx, &rows)
        .await
        .unwrap()
        .ids();
    tx.commit().await.unwrap();
    (docs, precisions[0])
}

async fn loaded(
    h: &Harness,
    plan: &PrefetchPlan,
    docs: &[i64],
    user: bool,
) -> Result<RecordStore, DataError> {
    let mut tx = if user {
        h.orm()
            .begin_opts(BeginOptions::for_principal(principal()))
            .await
            .unwrap()
    } else {
        h.begin().await.unwrap()
    };
    let env = tx.env();
    assert_eq!(env.su(), !user);
    load(
        &mut tx,
        &env,
        plan,
        &[("docs", docs)],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
}

fn assert_missing(error: DataError, name: &str) {
    let reason = format!("decimal precision `{name}` was not loaded for this report");
    assert_eq!(error.to_string(), format!("report data: {reason}"));
    let DataError::Report(ReportError::Data { reason: actual }) = error else {
        panic!("expected a missing-precision data error, got {error}");
    };
    assert_eq!(actual, reason);
}

#[tokio::test]
async fn repeated_and_distinct_names_share_one_lookup_for_small_and_larger_batches() {
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .build()
        .await
        .unwrap();
    let (docs, _) = seed(&h, 64).await;
    let (program, plan) = report(h.registry(), NAMED);
    let mut statement_counts = Vec::new();
    for count in [1, docs.len()] {
        let mut tx = h.begin().await.unwrap();
        let env = tx.env();
        let before = tx.query_count();
        // One stored document read plus one set-based precision search_read, on cold caches.
        let store = assert_query_count!(tx, >= 2, <= 2, {
            load(&mut tx, &env, &plan, &[("docs", &docs[..count])],
                ReportLimits::default(), Limits::default()).await.unwrap()
        });
        statement_counts.push(tx.query_count() - before);
        assert_eq!(
            store.len(),
            count,
            "precision metadata is not a document record"
        );
        assert_eq!(
            printed(&program, store, &docs[..count]),
            "12.37512.412.375".repeat(count)
        );
    }
    assert_eq!(statement_counts[0], statement_counts[1]);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn no_precision_option_adds_no_lookup_even_without_precision_read_access() {
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .policy(
            DomainPolicy::builder(Otherwise::Allow)
                .deny(PRECISION, Operation::Read)
                .build()
                .unwrap(),
        )
        .build()
        .await
        .unwrap();
    let (program, plan) = report(
        h.registry(),
        "<div><span t-out=\"12.375\" t-options=\"{'widget': 'float', 'precision': 1}\"/></div>",
    );
    let mut tx = h
        .orm()
        .begin_opts(BeginOptions::for_principal(principal()))
        .await
        .unwrap();
    let env = tx.env();
    assert!(!env.su());
    let store = assert_query_count!(tx, <= 0, {
        load(&mut tx, &env, &plan, &[("docs", &[])], ReportLimits::default(), Limits::default()).await.unwrap()
    });
    assert_eq!(store.len(), 0);
    assert_eq!(printed(&program, store, &[]), "12.4");
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn an_unknown_name_fails_with_its_full_data_error() {
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .build()
        .await
        .unwrap();
    let (docs, _) = seed(&h, 1).await;
    let (_, plan) = report(
        h.registry(),
        &SINGLE.replace("Product Unit", "Unknown Unit"),
    );
    assert_missing(
        loaded(&h, &plan, &docs, false).await.unwrap_err(),
        "Unknown Unit",
    );
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_permitted_load_does_not_authorize_a_later_model_denied_principal() {
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .policy(
            DomainPolicy::builder(Otherwise::Allow)
                .deny(PRECISION, Operation::Read)
                .build()
                .unwrap(),
        )
        .build()
        .await
        .unwrap();
    let (docs, _) = seed(&h, 1).await;
    let (program, plan) = report(h.registry(), SINGLE);
    assert_eq!(
        printed(
            &program,
            loaded(&h, &plan, &docs, false).await.unwrap(),
            &docs
        ),
        "12.375"
    );
    let error = loaded(&h, &plan, &docs, true).await.unwrap_err();
    let DataError::Orm(error) = error else {
        panic!("expected ORM refusal, got {error}")
    };
    assert_eq!(error.kind(), ErrorKind::Access);
    assert_eq!(
        error.message(),
        "Your access rights do not let you read 'Decimal Precision' (decimal.precision) records."
    );
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn precision_name_and_digits_field_denials_preserve_the_orm_error() {
    for field in ["name", "digits"] {
        let h = Harness::builder([&base_users::ADDON, &ADDON])
            .with_decls(decls())
            .policy(FieldPolicy::new().deny(PRECISION, field, FieldOp::Read))
            .build()
            .await
            .unwrap();
        let (docs, _) = seed(&h, 1).await;
        let (program, plan) = report(h.registry(), SINGLE);
        assert_eq!(
            printed(
                &program,
                loaded(&h, &plan, &docs, false).await.unwrap(),
                &docs
            ),
            "12.375"
        );
        let error = loaded(&h, &plan, &docs, true).await.unwrap_err();
        let DataError::Orm(error) = error else {
            panic!("expected ORM refusal, got {error}")
        };
        assert_eq!(error.kind(), ErrorKind::Access);
        assert_eq!(
            error.message(),
            format!(
                "You do not have enough rights to access the field \"{field}\" on Decimal Precision (decimal.precision). Please contact your system administrator.\n\nOperation: read"
            )
        );
        h.cleanup().await.unwrap();
    }
}

#[tokio::test]
async fn a_rule_hidden_existing_name_is_missing_even_after_a_permitted_load() {
    let visible = Domain::condition("name", Operator::Eq, DomainValue::from("Package")).unwrap();
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .policy(
            DomainPolicy::builder(Otherwise::Allow)
                .rule(PRECISION, Operation::Read, visible)
                .build()
                .unwrap(),
        )
        .build()
        .await
        .unwrap();
    let (docs, _) = seed(&h, 1).await;
    let (program, plan) = report(h.registry(), SINGLE);
    assert_eq!(
        printed(
            &program,
            loaded(&h, &plan, &docs, false).await.unwrap(),
            &docs
        ),
        "12.375"
    );
    assert_missing(
        loaded(&h, &plan, &docs, true).await.unwrap_err(),
        "Product Unit",
    );
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_fresh_load_observes_changed_digits_while_the_old_store_keeps_its_snapshot() {
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .build()
        .await
        .unwrap();
    let (docs, precision) = seed(&h, 1).await;
    let (program, plan) = report(h.registry(), SINGLE);
    let old = loaded(&h, &plan, &docs, false).await.unwrap();
    let mut tx = h.begin().await.unwrap();
    tx.env()
        .records(PRECISION)
        .unwrap()
        .browse([precision])
        .unwrap()
        .write_values(&mut tx, &BTreeMap::from([("digits".into(), Value::Int(0))]))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        printed(
            &program,
            loaded(&h, &plan, &docs, false).await.unwrap(),
            &docs
        ),
        "12"
    );
    assert_eq!(printed(&program, old, &docs), "12.375");
    h.cleanup().await.unwrap();
}
