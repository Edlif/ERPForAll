// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every window action of a set of addons opens through the web client's metadata path, with the
//! sub-list views of every one2many it reaches. Shared by the served install tests.

use std::collections::BTreeSet;

use crate::native_http::{NativeFixture, json_post};

/// The one2many comodels named in a view description's field map.
fn one2many_comodels(description: &serde_json::Value, into: &mut Vec<String>) {
    for fields in description["fields"].as_object().unwrap().values() {
        for field in fields.as_object().unwrap().values() {
            if field["type"] == "one2many" {
                into.push(field["relation"].as_str().unwrap().to_owned());
            }
        }
    }
}

/// Every window action of `modules` (all their menus' actions and the ones dashboard cards name)
/// opens through the path the web client takes (`erp_load_web_action_views`), and every one2many
/// comodel reached from those views describes its list and form the way the client asks for its
/// sub-lists (`x2many.ts`, `attachSubviews`). A view, action context or sub-list the web server
/// refuses fails here with the server's message, so an install that only stores views can no
/// longer hide a screen that cannot open. Actions are listed by external id, not by menu, because
/// feature-group menus are hidden from the administrator until their setting is on.
pub async fn assert_actions_open(fixture: &NativeFixture, modules: &[&str]) {
    let modules: Vec<String> = modules.iter().map(|m| (*m).to_owned()).collect();
    let actions: Vec<(String, i64)> = fixture
        .client
        .query(
            "SELECT module || '.' || name, res_id::int8 FROM ir_model_data \
              WHERE model = 'ir.actions.act_window' AND module = ANY($1) ORDER BY module, name",
            &[&modules],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    for module in &modules {
        assert!(
            actions
                .iter()
                .any(|(xmlid, _)| xmlid.starts_with(&format!("{module}."))),
            "{module} declares no window action"
        );
    }
    // every refusal is collected: one run names all screens that do not open
    let mut refused = Vec::new();
    let mut pending = Vec::new();
    for (xmlid, action) in &actions {
        let body =
            serde_json::json!({"ids": [action], "wire_major": 1, "options": {"mobile": false}});
        let reply = json_post(
            fixture.port,
            &fixture.cookies,
            "/erp/json/2/ir.actions.act_window/erp_load_web_action_views",
            &body.to_string(),
        );
        if reply.status != 200 {
            refused.push(format!("action {xmlid}: {} {}", reply.status, reply.body));
            continue;
        }
        let bundle: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        one2many_comodels(&bundle["views"], &mut pending);
    }
    let mut described = BTreeSet::new();
    while let Some(model) = pending.pop() {
        if !described.insert(model.clone()) {
            continue;
        }
        let body = serde_json::json!({"views": [[null, "list"], [null, "form"]], "wire_major": 1, "options": {"mobile": false}});
        let reply = json_post(
            fixture.port,
            &fixture.cookies,
            &format!("/erp/json/2/{model}/get_views"),
            &body.to_string(),
        );
        if reply.status != 200 {
            refused.push(format!(
                "sub-list views of {model}: {} {}",
                reply.status, reply.body
            ));
            continue;
        }
        one2many_comodels(&serde_json::from_str(&reply.body).unwrap(), &mut pending);
    }
    assert!(refused.is_empty(), "{}", refused.join("\n"));
}
