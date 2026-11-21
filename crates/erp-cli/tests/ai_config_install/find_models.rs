// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use erp_sdk::ai::{ToolCall, ToolCallId};
use erp_sdk::prelude::*;
use serde_json::json;

fn call(query: &str) -> ToolCall {
    ToolCall::new(
        ToolCallId::new("catalog-1").unwrap(),
        "find_models".into(),
        json!({"query":query}),
    )
    .unwrap()
}

async fn names(tx: &mut Transaction, query: &str) -> Vec<String> {
    let call = call(query);
    let result = erp_addon_ai_base::catalog::find_models(tx, &call)
        .await
        .unwrap();
    assert_eq!(result.id(), call.id());
    assert_eq!(result.data().source().tool().as_str(), "find_models");
    result.data().value()["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn retail_model_discovery_follows_the_callers_installed_access_policy() {
    big(async {
        let db = TestDb::create().await.unwrap();
        let super::retail::Retail {
            installed,
            orm,
            users,
            ..
        } = super::retail::install(&db).await;
        let mut setup = installed.begin(SUPERUSER_ID).await.unwrap();
        let denied = erp_addon_ai_base::catalog::find_models(&mut setup, &call("purchase"))
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), erp_sdk::ErrorKind::Access);
        assert_eq!(
            denied.message(),
            "Assistant tools require an unprivileged interactive user."
        );
        setup.rollback().await.unwrap();
        for (uid, model) in users.iter().zip([
            "purchase.order",
            "sale.order",
            "stock.picking",
            "account.move",
        ]) {
            let mut tx = orm.begin(*uid).await.unwrap();
            let visible = names(&mut tx, &model.to_ascii_uppercase()).await;
            assert!(
                visible.iter().any(|candidate| candidate == model),
                "{model}: {visible:?}"
            );
            assert!(visible.is_sorted());
            assert!(names(&mut tx, "nonexistent.catalog.model").await.is_empty());
            if model == "purchase.order" {
                assert!(names(&mut tx, "sale.order").await.is_empty());
            }
            let invalid = ToolCall::new(
                ToolCallId::new("bad-1").unwrap(),
                "find_models".into(),
                json!({"query":"sale","sudo":true}),
            )
            .unwrap();
            let error = erp_addon_ai_base::catalog::find_models(&mut tx, &invalid)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
            assert_eq!(
                error.message(),
                "find_models requires exactly one string argument named query."
            );
            tx.rollback().await.unwrap();
        }
        let mut setup = installed.begin(SUPERUSER_ID).await.unwrap();
        let params = setup.env().records("ir.config_parameter").unwrap();
        for (key, value) in [
            ("ai_base.catalog_result_rows", 1_i64),
            ("ai_base.catalog_help_chars", 1),
            ("ai_base.catalog_query_bytes", 4),
        ] {
            params
                .call::<(String, Option<i64>), i64>(
                    &mut setup,
                    "set_int",
                    (key.to_owned(), Some(value)),
                )
                .await
                .unwrap();
        }
        setup.commit().await.unwrap();
        let mut buyer = orm.begin(users[0]).await.unwrap();
        let body = erp_addon_ai_base::catalog::find_models(&mut buyer, &call(""))
            .await
            .unwrap();
        let rows = body.data().value()["models"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(body.data().value()["has_more"], true);
        assert!(rows[0]["description"].as_str().unwrap().chars().count() <= 1);
        assert_eq!(rows[0]["description_truncated"], true);
        let oversized = erp_addon_ai_base::catalog::find_models(&mut buyer, &call("ééé"))
            .await
            .unwrap_err();
        assert_eq!(oversized.kind(), erp_sdk::ErrorKind::Validation);
        assert_eq!(oversized.message(), "find_models query exceeds 4 bytes.");
        buyer.rollback().await.unwrap();
        let definition = erp_addon_ai_base::catalog::find_models_spec().unwrap();
        assert_eq!(definition.name().as_str(), "find_models");
        drop(buyer);
        drop(orm);
        drop(installed);
        db.cleanup().await.unwrap();
    })
}
