// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use erp_rpc::{RpcConfig, RpcService};
use erp_sdk::ai::{ToolCall, ToolCallId};
use erp_security::auth::{Auth, AuthConfig, ClientAddr, HashPool, LoginRequest, SessionSecret};
use serde_json::{Value, json};

struct Host;
impl erp_rpc::RpcHost for Host {
    fn language_active(&self, code: &str) -> erp_fields::Result<bool> {
        Ok(code == "en_US")
    }
    fn has_capability(
        &self,
        _: &erp_orm::Principal,
        _: &erp_orm::Capability,
    ) -> erp_fields::Result<bool> {
        Ok(false)
    }
}

fn agg_tool(
    model: &str,
    groupby: Value,
    aggregates: Value,
    domain: Value,
    order: &str,
) -> ToolCall {
    ToolCall::new(
        ToolCallId::new("agg-call-1").unwrap(),
        "aggregate_records".into(),
        json!({
            "model": model,
            "groupby": groupby,
            "aggregates": aggregates,
            "domain": domain,
            "order": order,
            "limit": 5
        }),
    )
    .unwrap()
}

#[test]
fn session_aggregate_reads_purchase_order_groups_through_audited_rpc() {
    big(async {
        let db = TestDb::create().await.unwrap();
        let super::retail::Retail {
            installed,
            orm,
            company: main,
            ..
        } = super::retail::install(&db).await;

        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        let vendor = tx
            .env()
            .records("res.partner")
            .unwrap()
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", Cell::Str("Retail supplier".into()))],
            )
            .await
            .unwrap()
            .ids()[0];
        let target = tx
            .env()
            .with_context([(
                "allowed_company_ids",
                erp_sdk::Value::list([erp_sdk::Value::Int(main)]),
            )])
            .records("purchase.order")
            .unwrap();
        target
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("partner_id", Cell::Id(vendor))],
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let auth = Arc::new(
            Auth::new(orm.clone(), AuthConfig::DEFAULT, HashPool::global())
                .await
                .unwrap(),
        );

        let login = auth
            .authenticate(LoginRequest::new(
                "catalog-purchase",
                "fixture-purchase-password",
                ClientAddr::InProcess("ai-retail-agg"),
            ))
            .await
            .unwrap();

        let cookie =
            Arc::new(SessionSecret::from_cookie(&login.session().secret().to_cookie()).unwrap());

        let service = RpcService::new(
            orm.clone(),
            Host,
            RpcConfig::builder()
                .read_audit_policy(erp_rpc::ReadAuditPolicy::all_users())
                .build()
                .unwrap(),
        );

        let company = erp_orm::CompanyId::new(main).unwrap();

        assert!(orm.check_audit_store().await.unwrap().allows_writes());

        let result = erp_cli::assistant_read::aggregate_records(
            &service,
            auth.clone(),
            cookie.clone(),
            company,
            &agg_tool(
                "purchase.order",
                json!(["state"]),
                json!(["id:count"]),
                json!([]),
                "id:count desc",
            ),
        )
        .await
        .unwrap();

        assert_eq!(result.data().source().model(), Some("purchase.order"));
        let val = result.data().value();
        let groups = val["groups"].as_array().unwrap();
        assert!(!groups.is_empty(), "expected at least one aggregate group");
        assert_eq!(groups[0]["state"], "draft");
        assert!(groups[0]["__count"].as_i64().unwrap() >= 1);

        // Canvas staging tool verification
        let canvas_tool = ToolCall::new(
            ToolCallId::new("canvas-call-1").unwrap(),
            "render_canvas".into(),
            json!({
                "canvas_type": "chart",
                "title": "Top Purchase States",
                "description": "State breakdown",
                "data_json": "{\"series\":[{\"label\":\"Draft\",\"value\":1}]}"
            }),
        )
        .unwrap();

        let canvas_res = erp_addon_ai_base::canvas::render_canvas(&canvas_tool).unwrap();
        assert_eq!(canvas_res.data().value()["status"], "staged");
        assert_eq!(canvas_res.data().value()["canvas_type"], "chart");

        let payload = erp_addon_ai_base::canvas::extract_canvas_payload(&canvas_tool).unwrap();
        assert_eq!(payload["title"], "Top Purchase States");
        assert_eq!(payload["data"]["series"][0]["label"], "Draft");

        db.cleanup().await.unwrap();
    });
}

#[test]
fn session_aggregate_security_denies_secret_fields_and_unsupported_clauses() {
    big(async {
        let db = TestDb::create().await.unwrap();
        let super::retail::Retail {
            installed: _,
            orm,
            company: main,
            ..
        } = super::retail::install(&db).await;

        let auth = Arc::new(
            Auth::new(orm.clone(), AuthConfig::DEFAULT, HashPool::global())
                .await
                .unwrap(),
        );

        let login = auth
            .authenticate(LoginRequest::new(
                "catalog-purchase",
                "fixture-purchase-password",
                ClientAddr::InProcess("ai-retail-agg-sec"),
            ))
            .await
            .unwrap();

        let cookie =
            Arc::new(SessionSecret::from_cookie(&login.session().secret().to_cookie()).unwrap());

        let service = RpcService::new(
            orm.clone(),
            Host,
            RpcConfig::builder()
                .read_audit_policy(erp_rpc::ReadAuditPolicy::all_users())
                .build()
                .unwrap(),
        );

        let company = erp_orm::CompanyId::new(main).unwrap();

        // Verify security: unreadable or secret domain fields are denied
        let denied_domain = erp_cli::assistant_read::aggregate_records(
            &service,
            auth.clone(),
            cookie.clone(),
            company,
            &agg_tool(
                "res.users",
                json!(["login"]),
                json!(["id:count"]),
                json!([["password_hash", "ilike", "test%"]]),
                "id:count desc",
            ),
        )
        .await;
        assert!(
            denied_domain.is_err(),
            "secret password_hash domain condition must be denied"
        );

        // Verify security: invalid order direction is rejected
        let invalid_order = erp_cli::assistant_read::aggregate_records(
            &service,
            auth.clone(),
            cookie.clone(),
            company,
            &agg_tool(
                "purchase.order",
                json!(["state"]),
                json!(["id:count"]),
                json!([]),
                "state invalid_direction",
            ),
        )
        .await;
        assert!(
            invalid_order.is_err(),
            "invalid order direction must be rejected"
        );

        // Verify safety: invalid aggregate function is rejected
        let invalid_agg = erp_cli::assistant_read::aggregate_records(
            &service,
            auth.clone(),
            cookie.clone(),
            company,
            &agg_tool(
                "purchase.order",
                json!(["state"]),
                json!(["id:unsupported_func"]),
                json!([]),
                "",
            ),
        )
        .await;
        assert!(
            invalid_agg.is_err(),
            "unsupported aggregate function must be rejected"
        );

        // Verify bounds: oversized canvas data_json is rejected
        let big_data = "a".repeat(70000);
        let oversized_tool = ToolCall::new(
            ToolCallId::new("canvas-call-err").unwrap(),
            "render_canvas".into(),
            json!({
                "canvas_type": "chart",
                "title": "Oversized",
                "description": "Too big",
                "data_json": big_data
            }),
        )
        .unwrap();
        assert!(erp_addon_ai_base::canvas::render_canvas(&oversized_tool).is_err());

        db.cleanup().await.unwrap();
    });
}
