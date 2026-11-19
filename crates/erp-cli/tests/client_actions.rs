// SPDX-License-Identifier: MIT OR Apache-2.0
//! Client actions served by declared tags: menu, direct open and action result.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]
use erp_addon_base::ADDON;
use erp_fields::{Cell, RecId};
use erp_modules::{AddonIndex, InitOptions, ModuleSystem};
use erp_orm::{Command, Context, Orm, RawVals, SUPERUSER_ID, Transaction};
use erp_sdk::client_actions::ClientActionDecl;
use erp_sdk::prelude::*;
use erp_security::{resolver::Resolver, source::PolicySource};
use serde_json::json;
use std::{path::Path, sync::Arc};

// The test binary links `base` as the declaring addon, so the tags live in its namespace.
fn check_board<'a>(
    _env: &'a Env,
    _tx: &'a mut Transaction,
    params: &'a erp_sdk::serde_json::Map<String, erp_sdk::serde_json::Value>,
) -> erp_sdk::BoxFuture<'a, Result<erp_sdk::serde_json::Map<String, erp_sdk::serde_json::Value>>> {
    Box::pin(async move {
        match params.get("report") {
            Some(report) if report.is_i64() => Ok(params.clone()),
            _ => Err(Error::validation(
                "The test board needs an integer 'report'.",
            )),
        }
    })
}
fn tags() -> Result<Vec<ClientActionDecl>> {
    Ok(vec![ClientActionDecl::new(
        "base.test_board",
        "Test board",
        check_board,
    )?])
}
erp_sdk::client_action_declarations!(tags);

async fn action(
    tx: &mut Transaction,
    tag: &str,
    target: &str,
    context: &str,
    params: serde_json::Value,
) -> i64 {
    tx.env()
        .records("ir.actions.client")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("Board".into()))
                    .set_cell("tag", Cell::Str(tag.into()))
                    .set_cell("target", Cell::Str(target.into()))
                    .set_cell("context", Cell::Str(context.into()))
                    .set_cell("params", Cell::Json(std::sync::Arc::new(params))),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}
async fn menu(tx: &mut Transaction, name: &str, action: i64) -> i64 {
    tx.env()
        .records("ir.ui.menu")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("active", Cell::Bool(true))
                    .set_cell(
                        "action",
                        Cell::Str(format!("ir.actions.client,{action}").into()),
                    ),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

#[test]
fn declared_client_actions_are_served_and_undeclared_ones_refused_by_name() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(run());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn xid(tx: &mut Transaction, name: &str, model: &str) -> i64 {
    tx.query(
        "SELECT res_id FROM ir_model_data WHERE module='base' AND name=$1 AND model=$2",
        &[&name, &model],
    )
    .await
    .unwrap()[0]
        .get(0)
}
async fn create(tx: &mut Transaction, model: &str, vals: RawVals) -> i64 {
    tx.env()
        .records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}
fn groups(ids: &[i64]) -> Command {
    Command::set(ids.iter().map(|id| RecId::from_real(*id).unwrap()))
}
async fn internal_user(tx: &mut Transaction, login: &str, extra: &[&str]) -> i64 {
    let mut member = vec![xid(tx, "group_user", "res.groups").await];
    for name in extra {
        member.push(xid(tx, name, "res.groups").await);
    }
    let company = xid(tx, "main_company", "res.company").await;
    let partner = create(
        tx,
        "res.partner",
        RawVals::new()
            .set_cell("name", Cell::Str(login.into()))
            .set_cell("company_id", Cell::Id(company)),
    )
    .await;
    create(
        tx,
        "res.users",
        RawVals::new()
            .set_cell("name", Cell::Str(login.into()))
            .set_cell("login", Cell::Str(login.into()))
            .set_cell("partner_id", Cell::Id(partner))
            .set_cell("company_id", Cell::Id(company))
            .set_cell("lang", Cell::Str("en_US".into()))
            .set_cell("tz", Cell::Str("UTC".into()))
            .set_commands("company_ids", [groups(&[company])])
            .set_commands("group_ids", [groups(&member)]),
    )
    .await
}

async fn run() {
    let _ = &ADDON;
    let db = erp_testkit::TestDb::create().await.unwrap();
    let mut index = AddonIndex::new(true);
    index.add_dev_path(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("addons"),
    );
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
        .native_artifact_identities(identities)
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_uom::ADDON,
        ])
        .view_types(erp_addon_web::view_type_contributions())
        .require_core_tables(erp_security::auth::core_tables())
        .build()
        .unwrap();
    let loaded = system.initialize(InitOptions::new()).await.unwrap();
    assert!(loaded.post_commit.is_clean());
    let mut tx = loaded.orm.begin(SUPERUSER_ID).await.unwrap();
    internal_user(&mut tx, "board-admin", &["group_system"]).await;
    let user = internal_user(&mut tx, "board-user", &[]).await;
    let board = action(
        &mut tx,
        "base.test_board",
        "current",
        "{}",
        json!({"report": 7}),
    )
    .await;
    let board_menu = menu(&mut tx, "Board menu", board).await;
    let bad_params = action(
        &mut tx,
        "base.test_board",
        "current",
        "{}",
        json!({"report": "seven"}),
    )
    .await;
    let dialog = action(
        &mut tx,
        "base.test_board",
        "new",
        "{}",
        json!({"report": 7}),
    )
    .await;
    let with_context = action(
        &mut tx,
        "base.test_board",
        "current",
        "{'a': 1}",
        json!({"report": 7}),
    )
    .await;
    tx.commit().await.unwrap();
    drop(tx);
    // An ordinary internal user, under the compiled policy: the read row of `ir.actions.client` is what lets
    // the menu load and the open by id see the action.
    let source = PolicySource::build(Arc::clone(loaded.orm.registry()), db.pool().clone())
        .await
        .unwrap();
    let table = loaded
        .orm
        .env(SUPERUSER_ID, Context::new())
        .generation_attachment::<erp_sdk::views::PublishedViewTypes>()
        .unwrap();
    let provider: Arc<dyn erp_orm::PolicyProvider> = Arc::new(source);
    let orm = Orm::builder(Arc::clone(loaded.orm.registry()), db.pool().clone())
        .require_core_tables(erp_security::auth::core_tables())
        .resolver(Resolver::for_registry(loaded.orm.registry()).unwrap())
        .policy(provider)
        .generation_attachment(erp_orm::GenerationAttachment::new(
            Arc::clone(loaded.orm.registry()),
            table,
        ))
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap();
    let principal = {
        let mut tx = orm
            .begin(erp_orm::UserId::new(user).unwrap())
            .await
            .unwrap();
        let principal = tx.env().principal().clone();
        tx.rollback().await.unwrap();
        principal
    };
    let begin = || async {
        orm.begin_opts(
            erp_orm::BeginOptions::for_principal(principal.clone())
                .read_only("ir.actions.client.erp_load_web_client_action"),
        )
        .await
        .unwrap()
    };
    let support = erp_addon_web::metadata_test_support::test_load_client_action;

    // Menu: a declared tag opens as kind `client`; the node says which action model its id names.
    let mut tx = begin().await;
    let env = tx.env();
    let tree =
        erp_addon_web::metadata_test_support::test_load_menus(&env, &mut tx, 32 * 1024 * 1024)
            .await
            .unwrap();
    let tree = serde_json::to_value(tree).unwrap();
    let node = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == board_menu)
        .unwrap();
    assert_eq!(node["action_id"], board);
    assert_eq!(node["action_kind"], "client");

    // By id: the declared tag, the normalized params and no context.
    let opened = serde_json::to_value(support(&env, &mut tx, board).await.unwrap()).unwrap();
    assert_eq!(
        opened["result"],
        json!({"kind": "client", "id": board, "name": "Board", "tag": "base.test_board", "params": {"report": 7}})
    );
    // The tag's validator refuses invalid params with its own message.
    let error = support(&env, &mut tx, bad_params).await.unwrap_err();
    assert_eq!(error.message(), "The test board needs an integer 'report'.");
    // Only target current, and parameters travel in params, never context.
    let error = support(&env, &mut tx, dialog).await.unwrap_err();
    assert!(
        error.message().contains("only 'current' is supported"),
        "{}",
        error.message()
    );
    let error = support(&env, &mut tx, with_context).await.unwrap_err();
    assert!(
        error.message().contains("has a context"),
        "{}",
        error.message()
    );

    // As an action result: a declared tag normalizes, an undeclared one is refused naming the tag.
    let normalize = erp_addon_web::metadata_test_support::test_normalize_action_result;
    let json_value = |value: serde_json::Value| erp_sdk::Value::Json(Arc::new(value));
    let result = normalize(
        &env,
        &mut tx,
        json_value(
            json!({"type": "ir.actions.client", "tag": "base.test_board", "params": {"report": 3}}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["result"],
        json!({"kind": "client", "id": null, "name": "base.test_board", "tag": "base.test_board", "params": {"report": 3}})
    );
    let error = normalize(
        &env,
        &mut tx,
        json_value(json!({"type": "ir.actions.client", "tag": "base.unknown_tag"})),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.message(),
        "Client action tag 'base.unknown_tag' is not declared by an installed addon."
    );
    // The notification tag keeps its own kind.
    let notified = normalize(
        &env,
        &mut tx,
        json_value(json!({"type": "ir.actions.client", "tag": "display_notification", "params": {"message": "Hi"}})),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(notified).unwrap()["result"]["kind"],
        "notification"
    );
    tx.rollback().await.unwrap();
    drop(tx);

    // A menu naming an undeclared tag fails the menu load, naming menu, action and tag.
    let mut tx = loaded.orm.begin(SUPERUSER_ID).await.unwrap();
    let ghost = action(&mut tx, "base.ghost", "current", "{}", json!({})).await;
    let ghost_menu = menu(&mut tx, "Ghost menu", ghost).await;
    tx.commit().await.unwrap();
    drop(tx);
    let mut tx = begin().await;
    let env = tx.env();
    let error =
        erp_addon_web::metadata_test_support::test_load_menus(&env, &mut tx, 32 * 1024 * 1024)
            .await
            .unwrap_err();
    assert_eq!(
        error.message(),
        format!(
            "Menu {ghost_menu} opens client action {ghost} with tag 'base.ghost', which no installed addon declares."
        )
    );
    tx.rollback().await.unwrap();
}
