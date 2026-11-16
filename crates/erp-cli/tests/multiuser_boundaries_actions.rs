// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real-session authorization boundary acceptance; no mocked RPC responses.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]
#[path = "support/multiuser_boundaries.rs"]
mod boundaries;
#[path = "support/multiuser_fixture.rs"]
mod multiuser_fixture;
#[path = "support/native_http.rs"]
mod native_http;

use boundaries::*;
use multiuser_fixture::{Subject, error, run, success};
use serde_json::json;

#[test]
fn hidden_discovery_and_direct_metadata_enforce_independent_checks() {
    run(|f| {
        let s = Actions::seed(f);
        let menus = success(&f.rpc(Subject::A, "ir.ui.menu", "erp_load_web_menus", json!({})));
        let nodes = &menus["nodes"];
        let visible = row_ids(nodes);
        assert!(visible.contains(&s.menus[0]));
        assert!(!visible.contains(&s.menus[1]));
        assert!(!visible.contains(&s.menus[2]));
        let action = success(&f.rpc(
            Subject::A,
            "ir.actions.act_window",
            "erp_load_web_action",
            json!({"ids":[s.actions[0]]}),
        ));
        assert_eq!(action["model"], CATEGORY);
        for method in ["erp_load_web_action", "erp_load_web_action_views"] {
            let body = |id| {
                if method == "erp_load_web_action" {
                    json!({"ids":[id]})
                } else {
                    json!({"ids":[id],"wire_major":1,"options":{"mobile":false}})
                }
            };
            success(&f.rpc(
                Subject::A,
                "ir.actions.act_window",
                method,
                body(s.actions[0]),
            ));
            error(
                &f.rpc(
                    Subject::A,
                    "ir.actions.act_window",
                    method,
                    body(s.actions[1]),
                ),
                403,
                ACCESS,
                "You may not read this window action.",
            );
            let message = model_denial(
                "Access",
                "ir.access",
                "access",
                &["Settings Administration"],
            );
            error(
                &f.rpc(
                    Subject::A,
                    "ir.actions.act_window",
                    method,
                    body(s.actions[2]),
                ),
                403,
                ACCESS,
                &message,
            );
        }
        for mobile in [false, true] {
            success(&f.rpc(Subject::A,CATEGORY,"get_views",json!({"views":[[null,"list"],[null,"form"]],"wire_major":1,"options":{"mobile":mobile}})));
            let message = model_denial(
                "Access",
                "ir.access",
                "access",
                &["Settings Administration"],
            );
            error(
                &f.rpc(
                    Subject::A,
                    "ir.access",
                    "get_views",
                    json!({"views":[[null,"list"]],"wire_major":1,"options":{"mobile":mobile}}),
                ),
                403,
                ACCESS,
                &message,
            );
        }
        // Generic view records are separate from the authorized get_views delivery path.
        let message = model_denial("View", "ir.ui.view", "access", &["Settings Administration"]);
        error(
            &f.rpc(Subject::A, "ir.ui.view", "search", json!({"domain":[]})),
            403,
            ACCESS,
            &message,
        );
        let message = model_denial(
            "Access",
            "ir.access",
            "access",
            &["Settings Administration"],
        );
        error(
            &f.rpc(Subject::A, "ir.access", "search", json!({"domain":[]})),
            403,
            ACCESS,
            &message,
        );
    });
}

#[test]
fn committed_revocation_reaches_both_warm_processes_and_survives_peer_loss() {
    run(|f| {
        let s = Categories::seed(f);
        let actions = Actions::seed(f);
        let mut peer = f.native.peer();
        let domain = json!([["id", "in", s.ids()]]);
        for port in [f.native.port, peer.port] {
            assert_eq!(
                success(&f.rpc_on(
                    port,
                    Subject::A,
                    CATEGORY,
                    "search_count",
                    json!({"domain":domain})
                )),
                2
            );
            let action = success(&f.rpc_on(
                port,
                Subject::A,
                "ir.actions.act_window",
                "erp_load_web_action",
                json!({"ids":[actions.actions[0]]}),
            ));
            assert_eq!(action["model"], CATEGORY);
        }
        let permission = f.xmlid("product", "access_product_category_user");
        write(f, "ir.access", &[permission], json!({"active":false}));
        assert_eq!(
            admin_read(f, "ir.access", permission, &["active"])["active"],
            false
        );
        let message = model_denial(
            "Product Category",
            CATEGORY,
            "access",
            &[
                "Products / Product Administrator",
                "Settings Administration",
            ],
        );
        for port in [f.native.port, peer.port] {
            error(
                &f.rpc_on(
                    port,
                    Subject::A,
                    CATEGORY,
                    "search_count",
                    json!({"domain":domain}),
                ),
                403,
                ACCESS,
                &message,
            );
            error(
                &f.rpc_on(
                    port,
                    Subject::A,
                    "ir.actions.act_window",
                    "erp_load_web_action",
                    json!({"ids":[actions.actions[0]]}),
                ),
                403,
                ACCESS,
                &message,
            );
        }
        peer.server.stop();
        error(
            &f.rpc(
                Subject::A,
                CATEGORY,
                "search_count",
                json!({"domain":domain}),
            ),
            403,
            ACCESS,
            &message,
        );
        let uid = f.user(Subject::A).id;
        let own = success(&f.rpc(
            Subject::A,
            "res.users",
            "read",
            json!({"ids":[uid],"fields":["id"]}),
        ));
        assert_eq!(row_ids(&own), vec![uid]);
        assert_eq!(
            admin_read(f, "ir.access", permission, &["active"])["active"],
            false
        );
        // No mutation is retried by this test: each write has a terminal HTTP result and readback.
    });
}

/// A window action named by its XML id (dashboard cards): the same
/// authorization as `erp_load_web_action`, and one answer for an unknown, foreign or unreadable reference.
#[test]
fn action_reference_loads_only_readable_window_actions_without_an_existence_oracle() {
    run(|f| {
        let by_ref = |subject, xml_id: &str| {
            f.rpc(
                subject,
                "ir.actions.act_window",
                "erp_load_web_action_ref",
                json!({"xml_id": xml_id}),
            )
        };
        // An ordinary user reads an action without groups on a model it may read, by reference as by id.
        let action = success(&by_ref(Subject::A, "base.action_country"));
        assert_eq!(action["model"], "res.country");
        let by_id = success(&f.rpc(
            Subject::A,
            "ir.actions.act_window",
            "erp_load_web_action",
            json!({"ids":[f.xmlid("base", "action_country")]}),
        ));
        assert_eq!(action, by_id);
        // A system-only action: the administrator reads it, the ordinary user gets the one refusal.
        let technical = success(&by_ref(Subject::Admin, "base.technical_model_action"));
        assert_eq!(technical["model"], "ir.model");
        let denied = "You may not read this window action.";
        for xml_id in [
            "base.technical_model_action", // exists, caller may not read it
            "base.group_system",           // exists, not a window action
            "base.no_such_window_action",  // does not exist
        ] {
            error(&by_ref(Subject::A, xml_id), 403, ACCESS, denied);
        }
        // Not a window action and unknown are refused the same way for the administrator too.
        for xml_id in ["base.group_system", "base.no_such_window_action"] {
            error(&by_ref(Subject::Admin, xml_id), 403, ACCESS, denied);
        }
        // The input is a bounded `module.name`; anything else is refused by what is wrong with it.
        let long = format!("base.{}", "a".repeat(260));
        for (xml_id, problem) in [
            ("action_country", "has no module part"),
            ("base.a.b", "has more than one dot"),
            ("base. action", "contains whitespace or control characters"),
            (long.as_str(), "is longer than 256 bytes"),
        ] {
            let reply = by_ref(Subject::A, xml_id);
            assert_eq!(reply.status, 422, "{problem}: unexpected HTTP status");
            let body: serde_json::Value = serde_json::from_str(&reply.body).expect("error JSON");
            assert_eq!(body["name"], "erp.exceptions.ValidationError");
            let message = body["message"].as_str().expect("error message");
            assert!(
                message.starts_with("Window action reference: invalid XML id '")
                    && message.ends_with(&format!("': it {problem}.")),
                "{problem}: {message}"
            );
        }
    });
}
