// SPDX-License-Identifier: MIT OR Apache-2.0
//! Expected outcomes are written from the spec cases, not read from the
//! rows under test; refusals are checked by kind and by the absence of the change.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "integration fixture assertions and shared helpers"
)]
#[path = "support/native_http.rs"]
mod native_http;

use native_http::*;
use serde_json::{Value, json};

const ACCESS: &str = "erp.exceptions.AccessError";

/// One signed-in session.
struct Session {
    id: i64,
    cookies: String,
}

fn call(port: u16, cookies: &str, model: &str, method: &str, body: Value) -> Reply {
    json_post(
        port,
        cookies,
        &format!("/erp/json/2/{model}/{method}"),
        &body.to_string(),
    )
}

fn ok(reply: &Reply, what: &str) -> Value {
    assert_eq!(reply.status, 200, "{what}: {}", reply.body);
    serde_json::from_str(&reply.body).unwrap()
}

fn denied(reply: &Reply, what: &str) {
    assert_eq!(reply.status, 403, "{what} must be refused: {}", reply.body);
    let body: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(body["name"], ACCESS, "{what}: {}", reply.body);
}

/// The id of a many2one value, sent as an id or as an `[id, name]` pair.
#[track_caller]
fn m2o(value: &Value) -> i64 {
    match value.as_i64().or_else(|| value[0].as_i64()) {
        Some(id) => id,
        None => panic!("not a many2one: {value}"),
    }
}

fn ids(value: &Value) -> Vec<i64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect()
}

fn login(fixture: &NativeFixture, login: &str, password: &str) -> String {
    let page = request(fixture.port, "GET", "/erp/login", &[], "");
    assert_eq!(page.status, 200);
    let prefs = cookie(&page, "prefs");
    let csrf = embedded(&page, "erp-login")["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let form = [
        ("db", fixture.db.name()),
        ("login", login),
        ("password", password),
        ("csrf_token", csrf.as_str()),
    ]
    .into_iter()
    .map(|(name, value)| format!("{name}={}", encoded(value.as_bytes())))
    .collect::<Vec<_>>()
    .join("&");
    let logged = request(
        fixture.port,
        "POST",
        "/erp/login",
        &[
            ("cookie", &prefs),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        &form,
    );
    assert_eq!(logged.status, 303, "{login} signs in");
    format!("{prefs}; {}", cookie(&logged, "session_id"))
}

/// The world of the matrix: company A (the main company) and B, a customer, and the orders and
/// lines of A owned by the own-documents user, by another salesperson and by nobody, and one in B.
struct World {
    port: u16,
    admin: String,
    a: i64,
    b: i64,
    customer: i64,
    own: Session,
    other: Session,
    all: Session,
    manager: Session,
    o_own: i64,
    o_other: i64,
    o_none: i64,
    o_b: i64,
    l_own: i64,
    l_other: i64,
    l_none: i64,
    l_b: i64,
}

impl World {
    fn admin(&self, model: &str, method: &str, mut body: Value) -> Value {
        body["context"] = json!({"allowed_company_ids": [self.a, self.b]});
        ok(
            &call(self.port, &self.admin, model, method, body),
            &format!("admin {model}.{method}"),
        )
    }

    fn as_user(&self, who: &Session, model: &str, method: &str, body: Value) -> Reply {
        call(self.port, &who.cookies, model, method, body)
    }

    fn admin_read(&self, model: &str, id: i64, field: &str) -> Value {
        self.admin(model, "read", json!({"ids": [id], "fields": [field]}))[0][field].clone()
    }

    fn exists(&self, model: &str, id: i64) -> bool {
        !ids(&self.admin(model, "search", json!({"domain": [["id", "=", id]]}))).is_empty()
    }

    fn orders(&self) -> Vec<i64> {
        vec![self.o_own, self.o_other, self.o_none, self.o_b]
    }

    fn lines(&self) -> Vec<i64> {
        vec![self.l_own, self.l_other, self.l_none, self.l_b]
    }

    fn visible(&self, who: &Session, model: &str, among: Vec<i64>) -> Vec<i64> {
        ids(&ok(
            &self.as_user(
                who,
                model,
                "search",
                json!({"domain": [["id", "in", among]], "order": "id"}),
            ),
            &format!("search {model}"),
        ))
    }

    fn line(&self, order: i64, name: &str) -> i64 {
        self.admin(
            "sale.order.line",
            "create",
            json!({"vals_list": [{"order_id": order, "display_type": "line_note", "name": name}]}),
        )[0]
        .as_i64()
        .unwrap()
    }
}

fn group(groups: &[(String, i64)], name: &str) -> i64 {
    groups
        .iter()
        .find(|(xmlid, _)| xmlid == name)
        .unwrap_or_else(|| panic!("group {name}"))
        .1
}

async fn world(fixture: &NativeFixture) -> World {
    let port = fixture.port;
    let admin = fixture.cookies.clone();
    let installed = request_within(
        port,
        "POST",
        "/erp/module/install",
        &[
            ("cookie", &admin),
            ("content-type", "application/json"),
            ("sec-fetch-site", "same-origin"),
        ],
        r#"{"modules":["sale"]}"#,
        std::time::Duration::from_secs(600),
    );
    assert_eq!(installed.status, 200, "install sale: {}", installed.body);
    let groups: Vec<(String, i64)> = fixture
        .client
        .query(
            "SELECT module || '.' || name, res_id::int8 FROM ir_model_data \
              WHERE model = 'res.groups' AND module = 'sales_team'",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let (admin_id, a): (i64, i64) = {
        let row = fixture
            .client
            .query_one(
                "SELECT id, company_id::int8 FROM res_users WHERE login = 'fixture-admin'",
                &[],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    let post = |model: &str, method: &str, body: Value| {
        ok(
            &call(port, &admin, model, method, body),
            &format!("setup {model}.{method}"),
        )
    };
    let b = post(
        "res.company",
        "create",
        json!({"vals_list": [{"name": "Sales Company B"}]}),
    )[0]
    .as_i64()
    .unwrap();
    post(
        "res.users",
        "write",
        json!({"ids": [admin_id], "vals": {"company_ids": [[6, 0, [a, b]]]}}),
    );
    let customer = post(
        "res.partner",
        "create",
        json!({"vals_list": [{"name": "Sales Access Customer"}]}),
    )[0]
    .as_i64()
    .unwrap();
    let mut sessions = Vec::new();
    for (login_name, xmlid) in [
        ("sale-own", "sales_team.group_sale_salesman"),
        ("sale-other", "sales_team.group_sale_salesman"),
        ("sale-all", "sales_team.group_sale_salesman_all_leads"),
        ("sale-manager", "sales_team.group_sale_manager"),
    ] {
        let password = format!("{login_name} fixture password");
        let id = post(
            "res.users",
            "create",
            json!({"vals_list": [{
                "name": login_name, "login": login_name, "password": password,
                "company_id": a, "company_ids": [[6, 0, [a]]],
                "group_ids": [[6, 0, [group(&groups, xmlid)]]]
            }]}),
        )[0]
        .as_i64()
        .unwrap();
        sessions.push(Session {
            id,
            cookies: login(fixture, login_name, &password),
        });
    }
    let [own, other, all, manager]: [Session; 4] = sessions.try_into().ok().unwrap();
    let mut world = World {
        port,
        admin,
        a,
        b,
        customer,
        own,
        other,
        all,
        manager,
        o_own: 0,
        o_other: 0,
        o_none: 0,
        o_b: 0,
        l_own: 0,
        l_other: 0,
        l_none: 0,
        l_b: 0,
    };
    let order = |world: &World, company: i64, user: Value| {
        let id = world.admin(
            "sale.order",
            "create",
            json!({"vals_list": [{"partner_id": world.customer, "company_id": company}]}),
        )[0]
        .as_i64()
        .unwrap();
        let wanted = user.as_i64();
        world.admin(
            "sale.order",
            "write",
            json!({"ids": [id], "vals": {"user_id": user}}),
        );
        if let Some(user) = wanted {
            assert_eq!(
                m2o(&world.admin_read("sale.order", id, "user_id")),
                user,
                "the written salesperson"
            );
        }
        id
    };
    world.o_own = order(&world, a, json!(world.own.id));
    world.o_other = order(&world, a, json!(world.other.id));
    world.o_none = order(&world, a, Value::Null);
    world.o_b = order(&world, b, Value::Null);
    world.l_own = world.line(world.o_own, "own line");
    world.l_other = world.line(world.o_other, "other line");
    world.l_none = world.line(world.o_none, "unassigned line");
    world.l_b = world.line(world.o_b, "company B line");
    assert_eq!(
        m2o(&world.admin_read("sale.order", world.o_other, "user_id")),
        world.other.id
    );
    assert_eq!(
        m2o(&world.admin_read("sale.order.line", world.l_other, "salesman_id")),
        world.other.id
    );
    world
}

/// The own-documents user (case 031).
fn own_documents_user(w: &World) {
    let own = &w.own;
    assert_eq!(
        w.visible(own, "sale.order", w.orders()),
        vec![w.o_own, w.o_none]
    );
    denied(
        &w.as_user(
            own,
            "sale.order",
            "read",
            json!({"ids": [w.o_other], "fields": ["name"]}),
        ),
        "read another salesperson's order",
    );
    denied(
        &w.as_user(
            own,
            "sale.order",
            "write",
            json!({"ids": [w.o_own, w.o_other], "vals": {"client_order_ref": "mixed"}}),
        ),
        "write a batch with another salesperson's order",
    );
    assert_eq!(
        w.admin_read("sale.order", w.o_own, "client_order_ref"),
        json!(false),
        "a refused batch changes nothing"
    );
    assert_eq!(
        w.admin_read("sale.order", w.o_other, "client_order_ref"),
        json!(false)
    );
    ok(
        &w.as_user(
            own,
            "sale.order",
            "write",
            json!({"ids": [w.o_own], "vals": {"client_order_ref": "mine"}}),
        ),
        "write the own order",
    );
    assert_eq!(
        w.admin_read("sale.order", w.o_own, "client_order_ref"),
        json!("mine")
    );
    denied(
        &w.as_user(
            own,
            "sale.order",
            "create",
            json!({"vals_list": [{"partner_id": w.customer, "user_id": w.other.id}]}),
        ),
        "create an order for another salesperson",
    );
    let created = ids(&ok(
        &w.as_user(
            own,
            "sale.order",
            "create",
            json!({"vals_list": [{"partner_id": w.customer}]}),
        ),
        "create an own quotation",
    ));
    assert_eq!(
        m2o(&w.admin_read("sale.order", created[0], "user_id")),
        own.id,
        "the creator is the salesperson"
    );
    denied(
        &w.as_user(own, "sale.order", "unlink", json!({"ids": [created[0]]})),
        "delete an own quotation",
    );
    assert!(w.exists("sale.order", created[0]));
    ok(
        &w.as_user(
            own,
            "sale.order",
            "action_confirm",
            json!({"ids": [created[0]]}),
        ),
        "confirm an own quotation",
    );
    assert_eq!(
        w.admin_read("sale.order", created[0], "state"),
        json!("sale")
    );
}

/// Line rights and the company rules (case 036).
fn line_permissions(w: &World) {
    let own = &w.own;
    assert_eq!(
        w.visible(own, "sale.order.line", w.lines()),
        vec![w.l_own, w.l_none]
    );
    denied(
        &w.as_user(
            own,
            "sale.order.line",
            "read",
            json!({"ids": [w.l_other], "fields": ["name"]}),
        ),
        "read a line of another salesperson",
    );
    denied(
        &w.as_user(
            own,
            "sale.order.line",
            "create",
            json!({"vals_list": [{"order_id": w.o_other, "display_type": "line_note", "name": "x"}]}),
        ),
        "add a line to another salesperson's order",
    );
    denied(
        &w.as_user(
            own,
            "sale.order.line",
            "unlink",
            json!({"ids": [w.l_other]}),
        ),
        "delete a line of another salesperson",
    );
    assert!(w.exists("sale.order.line", w.l_other));
    denied(
        &w.as_user(own, "sale.order", "unlink", json!({"ids": [w.o_other]})),
        "delete another salesperson's order",
    );
    assert!(w.exists("sale.order", w.o_other));
    for order in [w.o_own, w.o_none] {
        let line = ids(&ok(
            &w.as_user(
                own,
                "sale.order.line",
                "create",
                json!({"vals_list": [{"order_id": order, "display_type": "line_note", "name": "own"}]}),
            ),
            "add a line to an own or unassigned order",
        ))[0];
        ok(
            &w.as_user(
                own,
                "sale.order.line",
                "write",
                json!({"ids": [line], "vals": {"name": "edited"}}),
            ),
            "edit the line",
        );
        assert_eq!(
            w.admin_read("sale.order.line", line, "name"),
            json!("edited")
        );
        ok(
            &w.as_user(own, "sale.order.line", "unlink", json!({"ids": [line]})),
            "delete the line",
        );
        assert!(!w.exists("sale.order.line", line));
    }
    for who in [&w.all, &w.manager] {
        assert_eq!(
            w.visible(who, "sale.order.line", w.lines()),
            vec![w.l_own, w.l_other, w.l_none]
        );
        let line = ids(&ok(
            &w.as_user(
                who,
                "sale.order.line",
                "create",
                json!({"vals_list": [{"order_id": w.o_other, "display_type": "line_note", "name": "all"}]}),
            ),
            "add a line to another salesperson's order",
        ))[0];
        ok(
            &w.as_user(
                who,
                "sale.order.line",
                "write",
                json!({"ids": [line], "vals": {"name": "edited"}}),
            ),
            "edit it",
        );
        ok(
            &w.as_user(who, "sale.order.line", "unlink", json!({"ids": [line]})),
            "delete it",
        );
        assert!(!w.exists("sale.order.line", line));
    }
    // line rights never give order deletion to an ordinary sales user
    denied(
        &w.as_user(&w.all, "sale.order", "unlink", json!({"ids": [w.o_other]})),
        "delete an order with all-documents rights",
    );
    assert!(w.exists("sale.order", w.o_other));
    // the company rule holds for all three roles, on orders and on lines
    for who in [&w.own, &w.all, &w.manager] {
        assert!(w.visible(who, "sale.order", vec![w.o_b]).is_empty());
        assert!(w.visible(who, "sale.order.line", vec![w.l_b]).is_empty());
        denied(
            &w.as_user(
                who,
                "sale.order",
                "read",
                json!({"ids": [w.o_b], "fields": ["name"]}),
            ),
            "read an order of company B",
        );
        denied(
            &w.as_user(
                who,
                "sale.order.line",
                "write",
                json!({"ids": [w.l_b], "vals": {"name": "foreign"}}),
            ),
            "write a line of company B",
        );
    }
    assert_eq!(
        w.admin_read("sale.order.line", w.l_b, "name"),
        json!("company B line")
    );
}

/// All-documents user and manager (case 032).
fn manager_and_all_documents(w: &World) {
    let all = &w.all;
    assert_eq!(
        w.visible(all, "sale.order", w.orders()),
        vec![w.o_own, w.o_other, w.o_none]
    );
    ok(
        &w.as_user(
            all,
            "sale.order",
            "write",
            json!({"ids": [w.o_other], "vals": {"client_order_ref": "by all"}}),
        ),
        "all-documents user edits another salesperson's order",
    );
    let manager = &w.manager;
    assert_eq!(
        w.visible(manager, "sale.order", w.orders()),
        vec![w.o_own, w.o_other, w.o_none]
    );
    ok(
        &w.as_user(
            manager,
            "sale.order",
            "write",
            json!({"ids": [w.o_other], "vals": {"client_order_ref": "by manager"}}),
        ),
        "manager edits another salesperson's order",
    );
    assert_eq!(
        w.admin_read("sale.order", w.o_other, "client_order_ref"),
        json!("by manager")
    );
    // a draft of another salesperson is deleted; a confirmed one once cancelled
    let draft = w.admin(
        "sale.order",
        "create",
        json!({"vals_list": [{"partner_id": w.customer, "company_id": w.a, "user_id": w.other.id}]}),
    )[0]
    .as_i64()
    .unwrap();
    ok(
        &w.as_user(manager, "sale.order", "unlink", json!({"ids": [draft]})),
        "manager deletes a draft",
    );
    assert!(!w.exists("sale.order", draft));
    for operation in ["action_confirm", "action_cancel"] {
        ok(
            &w.as_user(
                manager,
                "sale.order",
                operation,
                json!({"ids": [w.o_other]}),
            ),
            operation,
        );
    }
    assert_eq!(
        w.admin_read("sale.order", w.o_other, "state"),
        json!("cancel")
    );
    ok(
        &w.as_user(manager, "sale.order", "unlink", json!({"ids": [w.o_other]})),
        "manager deletes a cancelled order",
    );
    assert!(!w.exists("sale.order", w.o_other));
    // the manager is confined to the allowed companies too
    denied(
        &w.as_user(manager, "sale.order", "unlink", json!({"ids": [w.o_b]})),
        "delete an order of company B",
    );
    assert!(w.exists("sale.order", w.o_b));
}

/// The denial matrix of the Sales groups on a served node, one fixture for the three cases (the
/// manager case deletes an order the others read, so it runs last).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sales_groups_and_companies_through_the_rpc_surface() {
    let fixture = NativeFixture::start().await;
    let w = world(&fixture).await;
    own_documents_user(&w);
    line_permissions(&w);
    manager_and_all_documents(&w);
    fixture.cleanup().await;
}
