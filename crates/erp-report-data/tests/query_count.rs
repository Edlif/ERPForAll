// SPDX-License-Identifier: MIT OR Apache-2.0
//! The report data stage against the database: the number of statements follows the prefetch
//! plan and not the number of records, reads run with the caller's rights, and a field of a type
//! reports cannot print is refused by name. The models are declared here (a document with a
//! partner and lines, lines with a product), so every expected count comes from the rows the
//! test created and from the plan the report library built.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::sync::Arc;

use erp_expr::EvalContext;
use erp_fields::{ErrorKind, FieldType, Value};
use erp_orm::{
    BeginOptions, GroupSet, Operation, PrincipalBuilder, Resolution, Transaction, UserId,
};
use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl, Registry};
use erp_report::{
    DocumentOptions, EvalOptions, Language, Limits, Node, PageSetup, PlanContext, PrefetchPlan,
    ReportLimits, ReportProgram, TemplateSource,
};
use erp_report_data::{DataError, field_info, load};
use erp_test_models::base_users;
use erp_testkit::{DomainPolicy, Harness, Otherwise, assert_query_count};

static ADDON: AddonDescriptor = AddonDescriptor {
    name: "x_report_data",
    depends: &["base"],
    exact_only: false,
};

const DOC: &str = "x_report_data.doc";
const LINE: &str = "x_report_data.line";
const PARTNER: &str = "x_report_data.partner";
const PRODUCT: &str = "x_report_data.product";
const DEMO: i64 = 2;

const NAME_ONLY: &[FieldDecl] = &[FieldDecl::char("name")];
const DOC_FIELDS: &[FieldDecl] = &[
    FieldDecl::char("name"),
    FieldDecl::many2one("partner_id", PARTNER),
    FieldDecl::one2many("line_ids", LINE, "doc_id"),
    FieldDecl::json("meta"),
];
const LINE_FIELDS: &[FieldDecl] = &[
    FieldDecl::many2one("doc_id", DOC),
    FieldDecl::many2one("product_id", PRODUCT),
];

/// The document of the batch test: its name, its partner's name, its lines' product names.
const DOCUMENT: &str = "<div><t t-foreach=\"docs\" t-as=\"o\"><p t-field=\"o.name\"/>\
     <span t-out=\"o.partner_id.name\"/><t t-foreach=\"o.line_ids\" t-as=\"l\">\
     <span t-out=\"l.product_id.name\"/></t></t></div>";

fn decls() -> [ModelDecl; 4] {
    [
        ModelDecl::new("x_report_data", PARTNER)
            .description("Report Partner")
            .fields(NAME_ONLY),
        ModelDecl::new("x_report_data", PRODUCT)
            .description("Report Product")
            .fields(NAME_ONLY),
        ModelDecl::new("x_report_data", DOC)
            .description("Report Document")
            .fields(DOC_FIELDS),
        ModelDecl::new("x_report_data", LINE)
            .description("Report Line")
            .fields(LINE_FIELDS),
    ]
}

fn program_of(arch: &str) -> ReportProgram {
    let sources = [TemplateSource::new("test.report", arch)];
    ReportProgram::compile(
        "test.report",
        &sources,
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap()
}

fn plan_of(registry: &Registry, arch: &str) -> PrefetchPlan {
    program_of(arch)
        .prefetch_plan(&PlanContext::new(DOC), &|model: &str, field: &str| {
            field_info(registry, model, field)
        })
        .unwrap()
}

/// The text `program` prints for the documents `docs` of `store` (adjacent text is one node).
fn printed(program: &ReportProgram, store: erp_report::RecordStore, docs: &[i64]) -> String {
    let store = Arc::new(store);
    let context = EvalContext::standard().with_var("docs", store.recordset(DOC, docs));
    let page = PageSetup::new(595.0, 842.0, 0.0).unwrap();
    let document = program
        .evaluate(
            &context,
            &store,
            &EvalOptions::default(),
            DocumentOptions::new("test", "test", page),
        )
        .unwrap();
    document
        .nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn user_principal() -> erp_orm::Principal {
    PrincipalBuilder::user(UserId::new(DEMO).unwrap())
        .resolution(Resolution::in_id_order(GroupSet::default(), [], NonZeroU64::MIN).unwrap())
        .build()
        .unwrap()
}

fn row(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

async fn create(tx: &mut Transaction, model: &str, rows: &[BTreeMap<String, Value>]) -> Vec<i64> {
    tx.env()
        .records(model)
        .unwrap()
        .create_values(tx, rows)
        .await
        .unwrap()
        .ids()
}

/// What the batch test created: per document its id, its partner and the products of its lines.
struct Batch {
    docs: Vec<i64>,
    partner_of: Vec<i64>,
    products_of: Vec<Vec<i64>>,
}

impl Batch {
    /// The records a report of the first `count` documents reads: the documents, their lines,
    /// and each partner and product they point to, once.
    fn records(&self, count: usize) -> usize {
        let partners: BTreeSet<i64> = self.partner_of[..count].iter().copied().collect();
        let lines: usize = self.products_of[..count].iter().map(Vec::len).sum();
        let products: BTreeSet<i64> = self.products_of[..count]
            .iter()
            .flatten()
            .copied()
            .collect();
        count + lines + partners.len() + products.len()
    }
}

/// `documents` documents over a pool of 50 partners, each with 2 lines over 20 products.
async fn batch(h: &Harness, documents: usize) -> Batch {
    let mut tx = h.begin().await.unwrap();
    let named = |prefix: &str, count: usize| -> Vec<BTreeMap<String, Value>> {
        (0..count)
            .map(|n| row(&[("name", Value::str(format!("{prefix}{n}")))]))
            .collect()
    };
    let partners = create(&mut tx, PARTNER, &named("Partner ", 50)).await;
    let products = create(&mut tx, PRODUCT, &named("Product ", 20)).await;
    let partner_of: Vec<i64> = (0..documents)
        .map(|n| partners[n % partners.len()])
        .collect();
    let doc_rows: Vec<_> = partner_of
        .iter()
        .enumerate()
        .map(|(n, partner)| {
            row(&[
                ("name", Value::str(format!("D{n:04}"))),
                ("partner_id", Value::Int(*partner)),
            ])
        })
        .collect();
    let docs = create(&mut tx, DOC, &doc_rows).await;
    let products_of: Vec<Vec<i64>> = (0..documents)
        .map(|n| {
            (0..2)
                .map(|line| products[(n * 2 + line) % products.len()])
                .collect()
        })
        .collect();
    let line_rows: Vec<_> = docs
        .iter()
        .zip(&products_of)
        .flat_map(|(doc, products)| {
            products.iter().map(|product| {
                row(&[
                    ("doc_id", Value::Int(*doc)),
                    ("product_id", Value::Int(*product)),
                ])
            })
        })
        .collect();
    create(&mut tx, LINE, &line_rows).await;
    tx.commit().await.unwrap();
    Batch {
        docs,
        partner_of,
        products_of,
    }
}

/// The statement ceiling of one load of `plan` in superuser mode on a cold cache, derived from
/// `Records::read_with` (`crates/erp-orm/src/read.rs`, `search.rs`, `relational.rs`). Per level:
///
/// - 1: the stored columns of the level's records, `SELECT id, columns… WHERE id = ANY($1)`
///   (one statement while the ids fit one prefetch chunk; the display name reads the record
///   name column in the same statement);
/// - 1 per one2many or many2many field of the level: the lines or the relation rows of all the
///   level's records in one statement.
///
/// Superuser mode prepares no access and adds no rule statement. A level whose columns an
/// earlier one2many read already cached issues fewer, so this is a ceiling.
fn ceiling(registry: &Registry, plan: &PrefetchPlan) -> u64 {
    plan.levels()
        .iter()
        .map(|level| {
            let spec = registry.model(level.model()).unwrap();
            let x2many = level
                .fields()
                .iter()
                .filter(|field| {
                    matches!(
                        spec.field(field).unwrap().ty,
                        FieldType::One2many { .. } | FieldType::Many2many { .. }
                    )
                })
                .count();
            1 + u64::try_from(x2many).unwrap()
        })
        .sum()
}

#[tokio::test]
async fn the_statements_of_a_load_follow_the_plan_not_the_batch_size() {
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .build()
        .await
        .expect("harness");
    let made = batch(&h, 1_000).await;
    let plan = plan_of(h.registry(), DOCUMENT);
    // documents, partners, lines and products are each read
    let models: BTreeSet<String> = plan
        .levels()
        .iter()
        .map(|level| level.model().to_owned())
        .collect();
    assert_eq!(
        models,
        [DOC, LINE, PARTNER, PRODUCT]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    );
    let ceiling = ceiling(h.registry(), &plan);

    // ten documents, in a transaction of their own (a cold cache)
    let few = 10;
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    let before = tx.query_count();
    let store = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", &made.docs[..few])],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap();
    let statements_few = tx.query_count() - before;
    assert_eq!(store.len(), made.records(few));
    drop(tx);

    // the whole batch: within the plan's ceiling, and the same count as for ten
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    let before = tx.query_count();
    let store = assert_query_count!(tx, >= 1, <= ceiling, {
        load(
            &mut tx,
            &env,
            &plan,
            &[("docs", made.docs.as_slice())],
            ReportLimits::default(),
            Limits::default(),
        )
        .await
        .unwrap()
    });
    let statements_all = tx.query_count() - before;
    assert_eq!(statements_all, statements_few);
    assert_eq!(store.len(), made.records(made.docs.len()));
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_user_without_read_access_gets_the_access_error_of_the_orm() {
    let policy = DomainPolicy::builder(Otherwise::Allow)
        .deny(DOC, Operation::Read)
        .build()
        .unwrap();
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .policy(policy)
        .build()
        .await
        .expect("harness");
    let made = batch(&h, 3).await;
    let plan = plan_of(h.registry(), DOCUMENT);
    let principal = PrincipalBuilder::user(UserId::new(DEMO).unwrap())
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
    let error = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", made.docs.as_slice())],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap_err();
    let DataError::Orm(error) = error else {
        panic!("expected the ORM's error, got {error}");
    };
    assert_eq!(error.kind(), ErrorKind::Access, "{error}");
    // the model-level refusal of the test policy, worded from the model the registry holds
    let spec = h.registry().model(DOC).unwrap();
    assert_eq!(
        error.message(),
        format!(
            "Your access rights do not let you {} '{}' ({}) records.",
            Operation::Read.as_str(),
            spec.description,
            spec.name
        )
    );
    drop(tx);

    // the same load as superuser reads them: the refusal came from the caller's rights
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    let store = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", made.docs.as_slice())],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap();
    assert_eq!(store.len(), made.records(made.docs.len()));
    drop(tx);
    h.cleanup().await.unwrap();
}

/// A field of a type reports cannot print is refused by model, field and type, before the model
/// is read.
#[tokio::test]
async fn an_unprintable_field_type_is_refused_by_name() {
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .build()
        .await
        .expect("harness");
    let made = batch(&h, 1).await;
    let plan = plan_of(
        h.registry(),
        "<div><t t-foreach=\"docs\" t-as=\"o\"><span t-out=\"o.meta\"/></t></div>",
    );
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    let error = assert_query_count!(tx, <= 0, {
        load(
            &mut tx,
            &env,
            &plan,
            &[("docs", made.docs.as_slice())],
            ReportLimits::default(),
            Limits::default(),
        )
        .await
        .unwrap_err()
    });
    let declared = h
        .registry()
        .model(DOC)
        .unwrap()
        .field("meta")
        .unwrap()
        .ty
        .type_name();
    let DataError::UnsupportedField { model, field, kind } = &error else {
        panic!("expected an unsupported field, got {error}");
    };
    assert_eq!((model.as_str(), field.as_str()), (DOC, "meta"));
    assert_eq!(kind, declared);
    assert_eq!(
        error.to_string(),
        format!("report data: {DOC}.meta is a {declared} field, which reports cannot print")
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

/// A relation printed as itself shows the display name of its target to a user who may read the
/// record holding it but not the target's model, as a form does (the ORM's many2one read). Any
/// other field of that target keeps the user's rights and is refused.
#[tokio::test]
async fn a_printed_relation_shows_the_name_of_a_target_the_user_may_not_read() {
    let policy = DomainPolicy::builder(Otherwise::Allow)
        .deny(PARTNER, Operation::Read)
        .build()
        .unwrap();
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls(decls())
        .policy(policy)
        .build()
        .await
        .expect("harness");
    let made = batch(&h, 3).await;
    let arch = "<div><t t-foreach=\"docs\" t-as=\"o\"><span t-field=\"o.partner_id\"/></t></div>";
    let program = program_of(arch);
    let plan = plan_of(h.registry(), arch);

    let mut tx = h
        .orm()
        .begin_opts(BeginOptions::for_principal(user_principal()))
        .await
        .unwrap();
    let env = tx.env();
    assert!(!env.su());
    let store = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", made.docs.as_slice())],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap();
    drop(tx);
    // the batch gives document n the n-th partner of its pool, named "Partner <n>"
    let expected: String = (0..made.docs.len())
        .map(|n| format!("Partner {n}"))
        .collect();
    assert_eq!(printed(&program, store, &made.docs), expected);

    // a field of the partner itself is still read with the user's rights
    let plan = plan_of(
        h.registry(),
        "<div><t t-foreach=\"docs\" t-as=\"o\"><span t-out=\"o.partner_id.name\"/></t></div>",
    );
    let mut tx = h
        .orm()
        .begin_opts(BeginOptions::for_principal(user_principal()))
        .await
        .unwrap();
    let env = tx.env();
    let error = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", made.docs.as_slice())],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap_err();
    let DataError::Orm(error) = error else {
        panic!("expected the ORM's error, got {error}");
    };
    assert_eq!(error.kind(), ErrorKind::Access, "{error}");
    let spec = h.registry().model(PARTNER).unwrap();
    assert_eq!(
        error.message(),
        format!(
            "Your access rights do not let you {} '{}' ({}) records.",
            Operation::Read.as_str(),
            spec.description,
            spec.name
        )
    );
    drop(tx);
    h.cleanup().await.unwrap();
}
