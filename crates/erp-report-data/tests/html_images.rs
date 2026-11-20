// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
use erp_expr::EvalContext;
use erp_fields::Value;
use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl};
use erp_report::{
    DocumentOptions, EvalOptions, Language, Limits, Node, PageSetup, PlanContext, ReportLimits,
    ReportProgram, TemplateSource,
};
use erp_report_data::{field_info, load};
use erp_test_models::base_users;
use erp_testkit::Harness;
use std::{collections::BTreeMap, sync::Arc};
static ADDON: AddonDescriptor = AddonDescriptor {
    name: "x_html_images",
    depends: &["base"],
    exact_only: false,
};
const DOC: &str = "x_html_images.doc";
const PHOTO: &str = "x_html_images.photo";
const DOC_FIELDS: &[FieldDecl] = &[FieldDecl::char("name"), FieldDecl::html("note")];
const PHOTO_FIELDS: &[FieldDecl] = &[
    FieldDecl::char("name"),
    FieldDecl::binary("picture"),
    FieldDecl::binary("restricted").groups("base.group_system"),
];
fn row(values: &[(&str, Value)]) -> BTreeMap<String, Value> {
    values
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}
fn program() -> ReportProgram {
    ReportProgram::compile(
        "test.images",
        &[TemplateSource::new(
            "test.images",
            "<div><p t-foreach='docs' t-as='o' t-field='o.note'/></div>",
        )],
        &Language::new("en_US").unwrap(),
        ReportLimits::default(),
    )
    .unwrap()
}
async fn harness() -> Harness {
    Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls([
            ModelDecl::new("x_html_images", DOC)
                .description("Image document")
                .fields(DOC_FIELDS),
            ModelDecl::new("x_html_images", PHOTO)
                .description("Image target")
                .fields(PHOTO_FIELDS),
        ])
        .build()
        .await
        .unwrap()
}
#[tokio::test]
async fn local_html_images_are_read_in_batches() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let photo = tx
        .env()
        .records(PHOTO)
        .unwrap()
        .create_values(
            &mut tx,
            &[row(&[
                ("name", Value::str("Seal")),
                (
                    "picture",
                    Value::Bytes(Arc::from(
                        &b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x01\0\0\0\x01"[..],
                    )),
                ),
            ])],
        )
        .await
        .unwrap()
        .ids()[0];
    let input = format!("<img src='/web/image/{PHOTO}/{photo}/picture' alt='Seal'>");
    let rows: Vec<_> = (0..12)
        .map(|n| {
            row(&[
                ("name", Value::str(format!("Doc {n}"))),
                ("note", Value::str(&input)),
            ])
        })
        .collect();
    let ids = tx
        .env()
        .records(DOC)
        .unwrap()
        .create_values(&mut tx, &rows)
        .await
        .unwrap()
        .ids()
        .to_vec();
    tx.commit().await.unwrap();
    drop(tx);
    let program = program();
    let plan = program
        .prefetch_plan(&PlanContext::new(DOC), &|m, f| {
            field_info(h.registry(), m, f)
        })
        .unwrap();
    let mut counts = Vec::new();
    for batch in [&ids[..1], ids.as_slice()] {
        let mut tx = h.begin().await.unwrap();
        let env = tx.env();
        let before = tx.query_count();
        let store = Arc::new(
            load(
                &mut tx,
                &env,
                &plan,
                &[("docs", batch)],
                ReportLimits::default(),
                Limits::default(),
            )
            .await
            .unwrap(),
        );
        counts.push(tx.query_count() - before);
        let context = EvalContext::standard().with_var("docs", store.recordset(DOC, batch));
        let document = program
            .evaluate(
                &context,
                &store,
                &EvalOptions::default(),
                DocumentOptions::new(
                    "images",
                    "Images",
                    PageSetup::new(400.0, 500.0, 30.0).unwrap(),
                ),
            )
            .unwrap();
        assert_eq!(
            document
                .nodes()
                .iter()
                .filter(|n| matches!(n, Node::Image { .. }))
                .count(),
            batch.len()
        );
        assert!(
            document.content_warnings().is_empty(),
            "{:?}",
            document.content_warnings()
        );
        tx.rollback().await.unwrap();
        drop(tx);
    }
    assert_eq!(counts[0], counts[1]);
    assert!(counts[0] > 0);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn image_target_model_denial_is_not_degraded() {
    use erp_orm::{BeginOptions, GroupSet, Operation, PrincipalBuilder, Resolution, UserId};
    use erp_testkit::{DomainPolicy, Otherwise};
    use std::num::NonZeroU64;
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls([
            ModelDecl::new("x_html_images", DOC)
                .description("Image document")
                .fields(DOC_FIELDS),
            ModelDecl::new("x_html_images", PHOTO)
                .description("Image target")
                .fields(PHOTO_FIELDS),
        ])
        .policy(
            DomainPolicy::builder(Otherwise::Allow)
                .deny(PHOTO, Operation::Read)
                .build()
                .unwrap(),
        )
        .build()
        .await
        .unwrap();
    let mut tx = h.begin().await.unwrap();
    let photo = tx
        .env()
        .records(PHOTO)
        .unwrap()
        .create_values(&mut tx, &[row(&[("name", Value::str("Seal"))])])
        .await
        .unwrap()
        .ids()[0];
    let id = tx
        .env()
        .records(DOC)
        .unwrap()
        .create_values(
            &mut tx,
            &[row(&[
                ("name", Value::str("Doc")),
                (
                    "note",
                    Value::str(format!("<img src='/web/image/{PHOTO}/{photo}/picture'>")),
                ),
            ])],
        )
        .await
        .unwrap()
        .ids()[0];
    tx.commit().await.unwrap();
    drop(tx);
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
    let plan = program()
        .prefetch_plan(&PlanContext::new(DOC), &|m, f| {
            field_info(h.registry(), m, f)
        })
        .unwrap();
    let error = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", &[id])],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap_err();
    let erp_report_data::DataError::Orm(error) = error else {
        panic!("expected access error: {error}")
    };
    assert_eq!(error.kind(), erp_fields::ErrorKind::Access);
    assert_eq!(
        error.message(),
        "Your access rights do not let you read 'Image target' (x_html_images.photo) records."
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn restricted_image_requires_archive_acknowledgement_even_for_superuser() {
    use erp_report_data::{DataError, HtmlImagePolicy, LoadOptions, load_with_options};
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let photo = tx
        .env()
        .records(PHOTO)
        .unwrap()
        .create_values(&mut tx, &[row(&[("name", Value::str("Seal"))])])
        .await
        .unwrap()
        .ids()[0];
    let id = tx
        .env()
        .records(DOC)
        .unwrap()
        .create_values(
            &mut tx,
            &[row(&[
                ("name", Value::str("Doc")),
                (
                    "note",
                    Value::str(format!("<img src='/web/image/{PHOTO}/{photo}/restricted'>")),
                ),
            ])],
        )
        .await
        .unwrap()
        .ids()[0];
    let env = tx.env();
    let plan = program()
        .prefetch_plan(&PlanContext::new(DOC), &|m, f| {
            field_info(h.registry(), m, f)
        })
        .unwrap();
    let error = load(
        &mut tx,
        &env,
        &plan,
        &[("docs", &[id])],
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error,DataError::Value{model,field,reason} if model==PHOTO && field=="restricted" && reason=="a dynamic group-restricted image requires explicit report attachment acknowledgement")
    );
    let store = load_with_options(
        &mut tx,
        &env,
        &plan,
        &[("docs", &[id])],
        LoadOptions::new(ReportLimits::default(), Limits::default())
            .with_html_image_policy(HtmlImagePolicy::CallerRights),
    )
    .await
    .unwrap();
    assert_eq!(store.len(), 1);
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn bounded_binary_projection_never_returns_excess_payloads() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let ids = tx
        .env()
        .records(PHOTO)
        .unwrap()
        .create_values(
            &mut tx,
            &[
                row(&[
                    ("name", Value::str("A")),
                    ("picture", Value::Bytes(Arc::from([1u8; 20]))),
                ]),
                row(&[
                    ("name", Value::str("B")),
                    ("picture", Value::Bytes(Arc::from([2u8; 20]))),
                ]),
            ],
        )
        .await
        .unwrap()
        .ids();
    tx.commit().await.unwrap();
    drop(tx);
    let mut tx = h.begin().await.unwrap();
    let records = tx.env().records(PHOTO).unwrap().browse(ids).unwrap();
    let rows = records
        .read_binary_bounded(&mut tx, "picture", 10, 40)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.size() == 20 && r.bytes().is_none()));
    let rows = records
        .read_binary_bounded(&mut tx, "picture", 20, 39)
        .await
        .unwrap();
    assert!(rows.iter().all(|r| r.bytes().is_none()));
    let rows = records
        .read_binary_bounded(&mut tx, "picture", 20, 40)
        .await
        .unwrap();
    assert!(
        rows.iter()
            .all(|r| r.bytes().is_some_and(|b| b.len() == 20))
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn archive_acknowledgement_does_not_grant_field_access() {
    use erp_orm::{BeginOptions, GroupSet, PrincipalBuilder, Resolution, UserId};
    use erp_report_data::{DataError, HtmlImagePolicy, LoadOptions, load_with_options};
    use erp_testkit::FieldPolicy;
    use std::num::NonZeroU64;
    let h = Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls([
            ModelDecl::new("x_html_images", DOC)
                .description("Image document")
                .fields(DOC_FIELDS),
            ModelDecl::new("x_html_images", PHOTO)
                .description("Image target")
                .fields(PHOTO_FIELDS),
        ])
        .policy(FieldPolicy::new().enforce_groups())
        .build()
        .await
        .unwrap();
    let mut tx = h.begin().await.unwrap();
    let photo = tx
        .env()
        .records(PHOTO)
        .unwrap()
        .create_values(
            &mut tx,
            &[row(&[
                ("name", Value::str("Seal")),
                ("restricted", Value::Bytes(Arc::from([1u8; 20]))),
            ])],
        )
        .await
        .unwrap()
        .ids()[0];
    let id = tx
        .env()
        .records(DOC)
        .unwrap()
        .create_values(
            &mut tx,
            &[row(&[
                ("name", Value::str("Doc")),
                (
                    "note",
                    Value::str(format!("<img src='/web/image/{PHOTO}/{photo}/restricted'>")),
                ),
            ])],
        )
        .await
        .unwrap()
        .ids()[0];
    tx.commit().await.unwrap();
    drop(tx);
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
    let plan = program()
        .prefetch_plan(&PlanContext::new(DOC), &|m, f| {
            field_info(h.registry(), m, f)
        })
        .unwrap();
    let error = load_with_options(
        &mut tx,
        &env,
        &plan,
        &[("docs", &[id])],
        LoadOptions::new(ReportLimits::default(), Limits::default())
            .with_html_image_policy(HtmlImagePolicy::CallerRights),
    )
    .await
    .unwrap_err();
    let DataError::Orm(error) = error else {
        panic!("expected ORM access error: {error}")
    };
    assert_eq!(error.kind(), erp_fields::ErrorKind::Access);
    assert_eq!(
        error.message(),
        "You do not have enough rights to access the field \"restricted\" on Image target (x_html_images.photo). Please contact your system administrator.\n\nOperation: read"
    );
    drop(tx);
    h.cleanup().await.unwrap();
}
