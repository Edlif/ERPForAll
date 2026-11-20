// SPDX-License-Identifier: MIT OR Apache-2.0
//! Installed base printing consumes the authoritative report-helper catalog.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]
use erp_modules::{
    AddonIndex,
    system::{InitOptions, ModuleSystem},
};
use erp_orm::rpc::{BindError, RPC_IMPLS, Receiver, RpcArgs, RpcDecl, RpcImpl};
use erp_sdk::ErrorKind;
use erp_sdk::prelude::*;
use erp_sdk::report::{REPORT_HELPERS, ReportHelperDecl};
use erp_testkit::TestDb;
use std::path::{Path, PathBuf};
#[path = "support/report_frame.rs"]
mod report_frame;

#[erp_sdk::__private::linkme::distributed_slice(REPORT_HELPERS)]
#[linkme(crate = erp_sdk::__private::linkme)]
static HELPER: ReportHelperDecl =
    ReportHelperDecl::new("base", "res.partner", "_test_report_label", &["name"]);
#[erp_sdk::__private::linkme::distributed_slice(RPC_IMPLS)]
#[linkme(crate = erp_sdk::__private::linkme)]
static CALLABLE: RpcImpl = RpcImpl {
    addon: "base",
    model: "res.partner",
    method: "_test_report_label",
    decl: RpcDecl::DataCallable {
        receiver: Receiver::Records,
        params: &[],
        bind,
    },
    file: file!(),
    line: line!(),
};
fn bind<'a>(
    tx: &'a mut Transaction,
    records: Records,
    _: RpcArgs,
) -> erp_orm::BoxFuture<'a, std::result::Result<Value, BindError>> {
    Box::pin(async move {
        Ok(Value::list(
            records.values(tx, "name").await.map_err(BindError::Call)?,
        ))
    })
}
fn addons_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("addons")
}
async fn create(tx: &mut Transaction, model: &str, values: &[(&'static str, &str)]) -> Records {
    let values = values.iter().fold(RawVals::new(), |v, (k, s)| {
        v.set_cell(*k, Cell::Str((*s).into()))
    });
    tx.env()
        .records(model)
        .unwrap()
        .create(tx, vec![values])
        .await
        .unwrap()
}
#[tokio::test(flavor = "multi_thread")]
async fn installed_base_print_runs_registered_helpers_and_rejects_unknown_calls() {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let mut index = AddonIndex::new(true);
    index.add_dev_path(addons_dir());
    let identities = index
        .discover_entries()
        .unwrap()
        .into_iter()
        .map(|(name, entry)| {
            let snapshot = erp_modules::artifact_identity::ArtifactSnapshot::capture(
                name.as_str(),
                Path::new(entry.source_location()),
            )
            .unwrap();
            (name, snapshot.digest().to_owned())
        })
        .collect();
    let system = ModuleSystem::builder(db.pool().clone(), index)
        .view_types(erp_addon_web::view_type_contributions())
        .native_artifact_identities(identities)
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_uom::ADDON,
        ])
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .require_core_tables(erp_security::auth::core_tables())
        .build()
        .unwrap();
    let init = Box::pin(system.initialize(InitOptions::new().with_skip_auto_install(true)))
        .await
        .unwrap();
    let mut tx = init.orm.begin(erp_orm::SUPERUSER_ID).await.unwrap();
    let company: i64 = tx
        .query(
            "SELECT res_id::int8 FROM ir_model_data WHERE module='base' AND name='main_company'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.rollback().await.unwrap();
    let mut tx = init
        .orm
        .begin_with(
            erp_orm::SUPERUSER_ID,
            erp_orm::Context::new().with_company_id(company),
        )
        .await
        .unwrap();
    let partner = create(&mut tx, "res.partner", &[("name", "Helper <served>")]).await;
    for (key, method) in [
        ("test.helper", "_test_report_label"),
        ("test.unknown", "_unknown_report_helper"),
    ] {
        let arch = format!(
            "<t t-name='{key}'><div><p t-foreach='docs' t-as='o' t-out='o.{method}()'/></div></t>"
        );
        create(
            &mut tx,
            "ir.ui.view",
            &[
                ("name", key),
                ("key", key),
                ("type", "qweb"),
                ("arch_db", &arch),
            ],
        )
        .await;
        create(
            &mut tx,
            "ir.actions.report",
            &[
                ("name", key),
                ("model", "res.partner"),
                ("report_name", key),
                ("report_type", "qweb-html"),
            ],
        )
        .await;
        let action = erp_addon_base::report_print::find(&mut tx, key)
            .await
            .unwrap();
        let result = erp_addon_base::report_print::prepare(&mut tx, &action, &partner.ids()).await;
        if key == "test.helper" {
            let document = report_frame::through_frame(&result.unwrap());
            let texts: Vec<_> = document
                .nodes()
                .iter()
                .filter_map(|node| {
                    if let erp_report::Node::Text(text) = node {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            assert!(
                texts.iter().any(|text| text.contains("Helper <served>")),
                "{texts:?}"
            );
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation);
            assert!(
                error.message().contains(method)
                    && error.message().contains("not a registered report helper"),
                "{error}"
            );
        }
    }
    tx.rollback().await.unwrap();
    drop(tx);
    drop(init);
    drop(system);
    db.cleanup().await.unwrap();
}
