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
fn tool(model: &str, fields: Value, domain: Value, order: &str) -> ToolCall {
    ToolCall::new(
        ToolCallId::new("buyer-search-1").unwrap(),
        "search_records".into(),
        json!({"model":model,"fields":fields,"domain":domain,"order":order,"limit":5}),
    )
    .unwrap()
}
#[test]
fn session_search_reads_only_allowed_purchase_orders_through_audited_rpc() {
    big(async {
        let db = TestDb::create().await.unwrap();
        let super::retail::Retail {
            installed,
            orm,
            company: main,
            users,
            ..
        } = super::retail::install(&db).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        let currency = tx
            .env()
            .records("res.company")
            .unwrap()
            .browse([main])
            .unwrap()
            .cell(&mut tx, "currency_id")
            .await
            .unwrap();
        let other = tx
            .env()
            .records("res.company")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Other catalog company".into()))
                        .set_cell("currency_id", currency),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
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
        // An actual group-protected scalar on a model the buyer can read.
        let job_id = tx
            .env()
            .records("report.job")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Buyer report".into()))
                        .set_cell("user_id", Cell::Id(users[0].get()))
                        .set_cell("report_name", Cell::Str("purchase.test".into()))
                        .set_cell("res_model", Cell::Str("purchase.order".into()))
                        .set_cell("res_ids", Cell::Str("[]".into()))
                        .set_cell("parts", Cell::Str("[23]".into())),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let mut orders = Vec::new();
        for company in [main, other] {
            let target = tx
                .env()
                .with_context([(
                    "allowed_company_ids",
                    erp_sdk::Value::list([erp_sdk::Value::Int(company)]),
                )])
                .records("purchase.order")
                .unwrap();
            orders.push(
                target
                    .create(
                        &mut tx,
                        vec![RawVals::new().set_cell("partner_id", Cell::Id(vendor))],
                    )
                    .await
                    .unwrap()
                    .ids()[0],
            );
        }
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
                ClientAddr::InProcess("ai-retail-search"),
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
        let run = async |request: ToolCall| {
            erp_cli::assistant_read::search_records(
                &service,
                auth.clone(),
                cookie.clone(),
                company,
                &request,
            )
            .await
        };
        // The composed test ORM, unlike a running server, has not checked its audit store.
        let withheld = run(tool(
            "purchase.order",
            json!(["name"]),
            json!([["id", ">", 0]]),
            "id asc",
        ))
        .await
        .unwrap_err();
        assert_eq!(
            withheld.message(),
            "Audited web_search_read failed with status 503."
        );
        assert!(orm.check_audit_store().await.unwrap().allows_writes());
        let records = run(tool(
            "purchase.order",
            json!(["name", "state"]),
            json!([["id", ">", 0]]),
            "id asc",
        ))
        .await
        .unwrap();
        assert_eq!(records.data().source().ids(), &[orders[0]]);
        assert_eq!(records.data().source().model(), Some("purchase.order"));
        let rows = records.data().value()["records"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"], "draft");
        assert_eq!(rows[0]["id"], orders[0]);
        assert!(rows[0].get("partner_id").is_none());
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        assert_eq!(tx.query("SELECT COUNT(*) FROM erp_event.ir_audit_entry WHERE outcome='read_ok' AND model='purchase.order' AND method='web_search_read'",&[]).await.unwrap()[0].get::<_,i64>(0),1);
        tx.rollback().await.unwrap();
        let visible = run(tool(
            "report.job",
            json!(["name", "parts"]),
            json!([["id", "=", job_id]]),
            "id asc",
        ))
        .await
        .unwrap();
        assert_eq!(visible.data().source().ids(), &[job_id]);
        assert_eq!(
            visible.data().value()["records"][0],
            json!({"id":job_id, "name":"Buyer report"})
        );
        for (model, fields, domain, order) in [
            (
                "report.job",
                json!(["name"]),
                json!([["parts", "=", "[23]"]]),
                "id asc",
            ),
            (
                "report.job",
                json!(["name"]),
                json!([["id", "=", job_id]]),
                "parts desc",
            ),
            (
                "res.users",
                json!(["id"]),
                json!([["password", "=", "guess"]]),
                "id asc",
            ),
            (
                "res.users",
                json!(["id"]),
                json!([["id", ">", 0]]),
                "password asc",
            ),
            (
                "sale.order",
                json!(["id"]),
                json!([["id", ">", 0]]),
                "id asc",
            ),
        ] {
            let err = run(tool(model, fields, domain, order)).await.unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Access, "{err}");
        }
        let err = run(tool("purchase.order", json!(["name"]), json!([]), "id asc"))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Validation);
        let mut setup = installed.begin(SUPERUSER_ID).await.unwrap();
        setup
            .env()
            .records("ir.config_parameter")
            .unwrap()
            .call::<(String, Option<i64>), i64>(
                &mut setup,
                "set_int",
                ("ai_base.search_text_chars".into(), Some(2)),
            )
            .await
            .unwrap();
        setup.commit().await.unwrap();
        let clipped = run(tool(
            "purchase.order",
            json!(["state"]),
            json!([["id", ">", 0]]),
            "id asc",
        ))
        .await
        .unwrap();
        assert_eq!(clipped.data().source().ids(), &[orders[0]]);
        assert_eq!(
            clipped.data().value(),
            &json!({"records":[{"id":orders[0],"state":"dr"}],"truncated":true})
        );
        let expired = erp_cli::assistant_read::search_records(
            &service,
            auth,
            Arc::new(SessionSecret::generate().unwrap()),
            company,
            &tool(
                "purchase.order",
                json!(["id"]),
                json!([["id", ">", 0]]),
                "id asc",
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(expired.kind(), ErrorKind::SessionExpired);
        drop(service);
        drop(orm);
        drop(installed);
        db.cleanup().await.unwrap();
    })
}
