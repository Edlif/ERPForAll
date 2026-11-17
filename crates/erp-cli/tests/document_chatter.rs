// SPDX-License-Identifier: MIT OR Apache-2.0
//! Message thread and chatter on the documents people work in: journal entries and invoices,
//! purchase orders and transfers. The bridges `account_mail`, `purchase_mail` and `stock_mail`
//! install with `mail` on a database that already has these documents; the old rows take
//! messages, their forms carry the chatter, and a change of a tracked field is captured.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use erp_fields::{Cell, RecId, Value};
use erp_modules::{AddonIndex, InitOptions, ModuleName, ModuleSystem};
use erp_orm::rpc::{Exposure, RpcArgs};
use erp_orm::{Command, Context, Orm, RawVals, SUPERUSER_ID, Transaction, UserId};
use erp_testkit::TestDb;

#[path = "support/accounting.rs"]
mod accounting;

const BRIDGES: [&str; 3] = ["account_mail", "purchase_mail", "stock_mail"];

fn repo_addons() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("addons")
}

/// The module system of the served binary: every linked native addon.
fn system(db: &TestDb) -> ModuleSystem {
    let mut index = AddonIndex::new(true);
    index.add_dev_path(repo_addons());
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
    ModuleSystem::builder(db.pool().clone(), index)
        .code(erp_cli::native_addons::DESCRIPTORS.iter().copied())
        .require_sql_objects(erp_cli::native_addons::SQL_MANIFESTS.iter().copied())
        .native_artifact_identities(identities)
        .view_types(erp_addon_web::view_type_contributions())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .build()
        .unwrap()
}

fn run(body: impl std::future::Future<Output = ()> + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(body);
        })
        .unwrap()
        .join()
        .unwrap();
}

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

async fn create(tx: &mut Transaction, model: &str, vals: RawVals) -> i64 {
    let company = one_id(
        tx,
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
    )
    .await;
    tx.env()
        .with_company(company)
        .unwrap()
        .records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

async fn one_id(tx: &mut Transaction, sql: &str) -> i64 {
    tx.query(sql, &[]).await.unwrap()[0].get(0)
}

/// The documents written before `mail` is installed.
struct Documents {
    entry: i64,
    order: i64,
    picking: i64,
    vendor: i64,
}

async fn documents(tx: &mut Transaction) -> Documents {
    let company: i64 = one_id(
        tx,
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
    )
    .await;
    let journal: i64 = tx
        .query(
            "INSERT INTO account_journal (name, code, active, type, company_id, sequence, \
             refund_sequence, payment_sequence, sequence_reset, sequence_padding, color, \
             show_on_dashboard) \
             VALUES (jsonb_build_object('en_US', 'Miscellaneous'), 'OLD', true, 'general', $1, \
             10, false, false, 'month', 4, 0, true) RETURNING id::int8",
            &[&company],
        )
        .await
        .unwrap()[0]
        .get(0);
    let entry = create(
        tx,
        "account.move",
        RawVals::new()
            .set_cell("journal_id", Cell::Id(journal))
            .set_cell(
                "date",
                Cell::Date(erp_fields::Date::from_ymd(2026, 11, 2).unwrap()),
            ),
    )
    .await;
    let vendor = create(
        tx,
        "res.partner",
        RawVals::new().set_cell("name", text("Old vendor")),
    )
    .await;
    let order = create(
        tx,
        "purchase.order",
        RawVals::new().set_cell("partner_id", Cell::Id(vendor)),
    )
    .await;
    let rows = tx
        .query(
            "SELECT id::int8, default_location_src_id::int8, default_location_dest_id::int8 \
             FROM stock_picking_type WHERE code = 'incoming' ORDER BY id LIMIT 1",
            &[],
        )
        .await
        .unwrap();
    let (kind, from, to): (i64, i64, i64) = (rows[0].get(0), rows[0].get(1), rows[0].get(2));
    let picking = create(
        tx,
        "stock.picking",
        RawVals::new()
            .set_cell("name", text("Old receipt"))
            .set_cell("picking_type_id", Cell::Id(kind))
            .set_cell("location_id", Cell::Id(from))
            .set_cell("location_dest_id", Cell::Id(to)),
    )
    .await;
    Documents {
        entry,
        order,
        picking,
        vendor,
    }
}

/// A user with the settings and access-rights groups, for the served view path.
async fn admin_user(orm: &Orm) -> i64 {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let groups: Vec<RecId> = tx
        .query(
            "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' \
             AND name IN ('group_system', 'group_erp_manager') AND model = 'res.groups'",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| RecId::from_real(row.get::<_, i64>(0)).unwrap())
        .collect();
    let company = one_id(
        &mut tx,
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
    )
    .await;
    let uid = create(
        &mut tx,
        "res.users",
        RawVals::new()
            .set_cell("name", text("Document Admin"))
            .set_cell("login", text("document_admin"))
            .set_cell("company_id", Cell::Id(company))
            .set_commands(
                "company_ids",
                [Command::Set(vec![RecId::from_real(company).unwrap()])],
            )
            .set_commands("group_ids", [Command::Set(groups)]),
    )
    .await;
    tx.commit().await.unwrap();
    uid
}

/// The ORM of the served process with the access policy it compiles (`erp-cli` serving).
async fn served_orm(orm: &Orm, db: &TestDb) -> Orm {
    let source =
        erp_security::source::PolicySource::build(Arc::clone(orm.registry()), db.pool().clone())
            .await
            .unwrap();
    let published_views = orm
        .env(SUPERUSER_ID, Context::new())
        .generation_attachment::<erp_views::PublishedViewTypes>()
        .unwrap();
    Orm::builder(Arc::clone(orm.registry()), db.pool().clone())
        .generation_attachment(erp_orm::GenerationAttachment::new(
            Arc::clone(orm.registry()),
            published_views,
        ))
        .require_core_tables(erp_security::auth::core_tables())
        .resolver(erp_security::resolver::Resolver::for_registry(orm.registry()).unwrap())
        .policy(Arc::new(source) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap()
}

/// The form `xmlid` as the web client receives it (`get_views`), as `uid`.
async fn served_form(
    orm: &Orm,
    db: &TestDb,
    uid: i64,
    model: &str,
    xmlid: &str,
) -> serde_json::Value {
    let runtime = served_orm(orm, db).await;
    let mut tx = runtime.begin(UserId::new(uid).unwrap()).await.unwrap();
    let (module, name) = xmlid.split_once('.').unwrap();
    let view: i64 = tx
        .query(
            "SELECT res_id::int8 FROM ir_model_data WHERE module = $1 AND name = $2 \
             AND model = 'ir.ui.view'",
            &[&module, &name],
        )
        .await
        .unwrap()[0]
        .get(0);
    let env = tx.env().clone();
    let described = erp_addon_web::metadata_test_support::test_get_views(
        &env,
        &mut tx,
        model,
        vec![Value::list([Value::Int(view), Value::str("form")])],
        32 * 1024 * 1024,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    serde_json::to_value(described).unwrap()
}

fn has_chatter(described: &serde_json::Value) -> bool {
    described["views"][0]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["kind"] == "chatter")
}

/// The superuser posts as a partner (the thread names an author).
async fn superuser_author(tx: &mut Transaction) {
    let partner = create(
        tx,
        "res.partner",
        RawVals::new().set_cell("name", text("Superuser")),
    )
    .await;
    tx.execute(
        "UPDATE res_users SET partner_id = $2, active = true, share = false WHERE id = $1",
        &[&1i64, &partner],
    )
    .await
    .unwrap();
    tx.invalidate_all().await.unwrap();
}

/// The thread's `store_fetch` as the web client sends it, as `uid`: the topic `schema`, and
/// the topic `thread` of `model` `id` when given. The answer's `store` payload and topic slots.
async fn store_fetch(orm: &Orm, uid: i64, thread: Option<(&str, i64)>) -> serde_json::Value {
    let topic = |name: &str, params: Option<erp_fields::ValueMap>| {
        let mut map = erp_fields::ValueMap::from_iter([("name".into(), Value::str(name))]);
        if let Some(params) = params {
            map.insert("params".into(), Value::Map(Arc::new(params)));
        }
        Value::Map(Arc::new(map))
    };
    let mut topics = vec![topic("schema", None)];
    if let Some((model, id)) = thread {
        topics.push(topic(
            "thread",
            Some(erp_fields::ValueMap::from_iter([
                ("model".into(), Value::str(model)),
                ("id".into(), Value::Int(id)),
                ("messages".into(), Value::Bool(true)),
            ])),
        ));
    }
    let mut tx = orm.begin(UserId::new(uid).unwrap()).await.unwrap();
    let records = tx.env().records("mail.store").unwrap();
    let Exposure::Exposed(method) = tx
        .exposure()
        .get(records.model_id(), "store_fetch")
        .expect("store_fetch is exposed")
        .clone()
    else {
        panic!("store_fetch is exposed")
    };
    let args = erp_fields::ValueMap::from_iter([
        ("wire".into(), Value::Int(1)),
        ("topics".into(), Value::list(topics)),
    ]);
    let answer = method
        .call(&mut tx, records, RpcArgs::new(args))
        .await
        .unwrap_or_else(|e| panic!("store_fetch: {e:?}"));
    tx.rollback().await.unwrap();
    let Value::Map(map) = answer else {
        panic!("store_fetch answered {answer:?}")
    };
    let json = |key: &str| match map.get(key) {
        Some(Value::Json(json)) => (**json).clone(),
        other => panic!("store_fetch {key}: {other:?}"),
    };
    serde_json::json!({"store": json("store"), "topics": json("topics")})
}

/// The models of the schema document a `store_fetch` answered, and its generation.
fn schema_models(answer: &serde_json::Value) -> (String, Vec<String>) {
    let document = &answer["topics"][0];
    assert_eq!(document["topic"], "schema", "{answer}");
    assert_eq!(document["ok"], true, "{answer}");
    let bytes = serde_json::to_vec(&document["data"]).unwrap();
    let schema = erp_sdk::store_wire::Decoder::new(erp_sdk::store_wire::WireLimits::default())
        .decode_schema(&bytes)
        .expect("the schema topic's document is a valid schema");
    let generation = schema.generation().as_str().to_owned();
    assert_eq!(
        answer["store"]["schema"], generation,
        "the answer and the schema topic carry one generation"
    );
    (
        generation,
        schema
            .models()
            .map(|(name, _)| name.as_str().to_owned())
            .collect(),
    )
}

/// Post `body` on `model` `id` through the exposed `thread_post`; the new message's id.
async fn post(tx: &mut Transaction, model: &str, id: i64, body: &str) -> i64 {
    try_post(tx, model, id, body)
        .await
        .unwrap_or_else(|e| panic!("{model} thread_post: {e:?}"))
}

/// [`post`], with the refusal kept.
async fn try_post(
    tx: &mut Transaction,
    model: &str,
    id: i64,
    body: &str,
) -> erp_fields::Result<i64> {
    let records = tx.env().records(model).unwrap().browse([id]).unwrap();
    let Exposure::Exposed(method) = tx
        .exposure()
        .get(records.model_id(), "thread_post")
        .unwrap_or_else(|| panic!("thread_post is exposed on {model}"))
        .clone()
    else {
        panic!("thread_post is exposed on {model}")
    };
    let args = erp_fields::ValueMap::from_iter([(
        "post".into(),
        Value::Map(Arc::new(erp_fields::ValueMap::from_iter([(
            "body".into(),
            Value::str(body),
        )]))),
    )]);
    let answer = match method.call(tx, records, RpcArgs::new(args)).await {
        Ok(answer) => answer,
        Err(erp_orm::rpc::BindError::Call(error)) => return Err(error),
        Err(other) => panic!("{model}: thread_post arguments: {other:?}"),
    };
    match answer {
        Value::Map(map) => match map.get("message_id") {
            Some(Value::Int(id)) => Ok(*id),
            other => panic!("{model}: thread_post answered {other:?}"),
        },
        other => panic!("{model}: thread_post answered {other:?}"),
    }
}

/// A user of `company` only, in the groups `xmlids` (`base.group_user` included by the caller),
/// with a contact of its own (the author of what it posts).
async fn user_in(tx: &mut Transaction, company: i64, login: &str, xmlids: &[&str]) -> i64 {
    let mut groups = Vec::new();
    for xmlid in xmlids {
        let (module, name) = xmlid.split_once('.').unwrap();
        let rows = tx
            .query(
                "SELECT res_id::int8 FROM ir_model_data WHERE module = $1 AND name = $2 \
                 AND model = 'res.groups'",
                &[&module, &name],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "group {xmlid}");
        groups.push(RecId::from_real(rows[0].get::<_, i64>(0)).unwrap());
    }
    let env = tx.env().with_company(company).unwrap();
    let partner = env
        .records("res.partner")
        .unwrap()
        .create(tx, vec![RawVals::new().set_cell("name", text(login))])
        .await
        .unwrap()
        .ids()[0];
    env.records("res.users")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text(login))
                    .set_cell("login", text(login))
                    .set_cell("partner_id", Cell::Id(partner))
                    .set_cell("company_id", Cell::Id(company))
                    .set_commands(
                        "company_ids",
                        [Command::Set(vec![RecId::from_real(company).unwrap()])],
                    )
                    .set_commands("group_ids", [Command::Set(groups)]),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// Whether the `thread` topic of `model` `id` answered `uid` (`false`: an access denial).
async fn thread_answered(orm: &Orm, uid: i64, model: &str, id: i64) -> bool {
    let answer = store_fetch(orm, uid, Some((model, id))).await;
    let slot = &answer["topics"][1];
    assert_eq!(slot["topic"], "thread", "{answer}");
    if slot["ok"] == false {
        assert_eq!(slot["error"]["kind"], "access", "{model} {id}: {answer}");
    }
    slot["ok"] == true
}

/// The rows of `model` about the document `doc_model` `id` that `uid` can find: none when the
/// search is refused.
async fn found_about(orm: &Orm, uid: i64, model: &str, doc_model: &str, id: i64) -> Vec<i64> {
    let mut tx = orm.begin(UserId::new(uid).unwrap()).await.unwrap();
    let field = if model == "mail.message" {
        "model"
    } else {
        "res_model"
    };
    let domain = erp_sdk::Domain::and([
        erp_sdk::Domain::condition(field, erp_sdk::Operator::Eq, doc_model).unwrap(),
        erp_sdk::Domain::condition("res_id", erp_sdk::Operator::Eq, id).unwrap(),
    ]);
    let found = match tx
        .env()
        .records(model)
        .unwrap()
        .search(&mut tx, &domain)
        .await
    {
        Ok(records) => records.ids(),
        Err(error) => {
            assert_eq!(
                error.kind(),
                erp_fields::ErrorKind::Access,
                "{model} search as {uid}: {error:?}"
            );
            Vec::new()
        }
    };
    tx.rollback().await.unwrap();
    found
}

/// The bridges with real users: a read-only accounting user reads the thread of an entry and an
/// order and posts on them (their `_mail_post_access` is read); an internal user without the
/// applications' groups, and a user of another company with them, get nothing of any of the three
/// documents: no thread (so no follower panel), no message, no history line, and no post.
async fn bridge_access(orm: &Orm, db: &TestDb, docs: &Documents) {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let main = one_id(
        &mut tx,
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
    )
    .await;
    // the superuser of this database has no default company, and a new company joins its
    // allowed companies, which the user check then refuses: give it the main company first
    tx.execute(
        &format!(
            "UPDATE res_users SET company_id = {main} WHERE id = {SUPERUSER_ID} AND company_id IS NULL"
        ),
        &[],
    )
    .await
    .unwrap();
    tx.execute(
        &format!(
            "INSERT INTO res_company_users_rel (cid, user_id) SELECT {main}, {SUPERUSER_ID} \
             WHERE NOT EXISTS (SELECT 1 FROM res_company_users_rel \
             WHERE cid = {main} AND user_id = {SUPERUSER_ID})"
        ),
        &[],
    )
    .await
    .unwrap();
    let currency = one_id(
        &mut tx,
        &format!("SELECT currency_id::int8 FROM res_company WHERE id = {main}"),
    )
    .await;
    let other_company = tx
        .env()
        .records("res.company")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Other company"))
                    .set_cell("currency_id", Cell::Id(currency)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    let reader = user_in(
        &mut tx,
        main,
        "bridge_reader",
        &["base.group_user", "account.group_account_readonly"],
    )
    .await;
    let outsider = user_in(&mut tx, main, "bridge_outsider", &["base.group_user"]).await;
    let stranger = user_in(
        &mut tx,
        other_company,
        "bridge_stranger",
        &[
            "base.group_user",
            "account.group_account_user",
            "purchase.group_purchase_user",
            "stock.group_stock_user",
        ],
    )
    .await;
    tx.commit().await.unwrap();
    drop(tx);
    let served = served_orm(orm, db).await;
    let documents = [
        ("account.move", docs.entry),
        ("purchase.order", docs.order),
        ("stock.picking", docs.picking),
    ];
    // every document has messages and a history line by now (posts and status operations above)
    for (model, id) in documents {
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let history = one_id(
            &mut tx,
            &format!(
                "SELECT count(*)::int8 FROM mail_message WHERE model = '{model}' AND res_id = {id} \
                 AND message_type = 'tracking'"
            ),
        )
        .await;
        assert!(history > 0, "{model} has history lines");
        tx.rollback().await.unwrap();
    }
    // the read-only accounting user: entry and order are readable and take its post
    for (model, id) in &documents[..2] {
        assert!(
            thread_answered(&served, reader, model, *id).await,
            "{model}: a reader of the application reads the thread"
        );
        let mut tx = served.begin(UserId::new(reader).unwrap()).await.unwrap();
        let message = try_post(&mut tx, model, *id, "Read-only note").await;
        assert!(message.is_ok(), "{model}: a reader posts: {message:?}");
        tx.commit().await.unwrap();
        assert!(
            !found_about(&served, reader, "mail.message", model, *id)
                .await
                .is_empty(),
            "{model}: the reader finds the thread's messages"
        );
    }
    assert!(
        !thread_answered(&served, reader, "stock.picking", docs.picking).await,
        "an accounting reader has no access to transfers"
    );
    // no access to the application, or the document of another company: nothing at all
    for (who, uid) in [("outsider", outsider), ("other company", stranger)] {
        for (model, id) in documents {
            assert!(
                !thread_answered(&served, uid, model, id).await,
                "{who}: no thread of {model}"
            );
            // the follower rows themselves are readable by every internal user, as in the
            // reference (`mail.followers` read for `base.group_user`, no rule): the thread topic's
            // follower panel is what is closed
            assert_eq!(
                found_about(&served, uid, "mail.message", model, id).await,
                Vec::<i64>::new(),
                "{who}: no message or history line of {model}"
            );
            let mut tx = served.begin(UserId::new(uid).unwrap()).await.unwrap();
            let refused = try_post(&mut tx, model, id, "Not mine").await;
            let error = refused.expect_err(&format!("{who}: no post on {model}"));
            assert_eq!(
                error.kind(),
                erp_fields::ErrorKind::Access,
                "{who} on {model}: {error:?}"
            );
            tx.rollback().await.unwrap();
        }
    }
}

/// The ORM of the served process for these checks: the module system's registry with the mail
/// history hook the composition root registers (`erp-cli` `server_orm_builder`).
fn history_orm(orm: &Orm, db: &TestDb) -> Orm {
    Orm::builder(Arc::clone(orm.registry()), db.pool().clone())
        .require_core_tables(erp_security::auth::core_tables())
        .policy(erp_orm::StaticPolicy::SuperuserOnly)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .precommit_hook(erp_addon_mail::TRACKING_HOOK)
        .build()
        .unwrap()
}

async fn state_of(tx: &mut Transaction, model: &str, id: i64) -> String {
    let table = model.replace('.', "_");
    tx.query(
        &format!("SELECT state::text FROM {table} WHERE id = $1"),
        &[&id],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// The status operations whose state the documents' own statements write (posting code, the
/// transfer triggers) or the base write sets (purchase, sales): each leaves one history message
/// whose `state` change is the old and the new status, through the mixin's one consumer.
async fn status_history(orm: &Orm, db: &TestDb, docs: &Documents) {
    let history = history_orm(orm, db);
    let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
    superuser_author(&mut tx).await;
    let template = create(
        &mut tx,
        "product.template",
        RawVals::new().set_cell("name", text("Pallet")),
    )
    .await;
    let rows = tx
        .query(
            "SELECT p.id::int8, t.uom_id::int8 FROM product_product p \
             JOIN product_template t ON t.id = p.product_tmpl_id WHERE t.id = $1",
            &[&template],
        )
        .await
        .unwrap();
    let (product, uom): (i64, i64) = (rows[0].get(0), rows[0].get(1));
    create(
        &mut tx,
        "stock.move",
        RawVals::new()
            .set_cell("picking_id", Cell::Id(docs.picking))
            .set_cell("product_id", Cell::Id(product))
            .set_cell("uom_id", Cell::Id(uom))
            .set_cell("product_uom_qty", Cell::Decimal(1.into())),
    )
    .await;
    let quotation = create(
        &mut tx,
        "sale.order",
        RawVals::new().set_cell("partner_id", Cell::Id(docs.vendor)),
    )
    .await;
    tx.commit().await.unwrap();
    // a finished transaction keeps its connection until dropped; the test pool is small
    drop(tx);
    // a record created through a served ORM gets its creation line at commit
    let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
    let created: Vec<String> = tx
        .query(
            "SELECT body FROM mail_message WHERE model = 'sale.order' AND res_id = $1 \
             AND message_type = 'notification'",
            &[&quotation],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(created, ["<p>Sales Order created</p>"]);
    tx.rollback().await.unwrap();
    drop(tx);
    for (model, id, method) in [
        ("account.move", docs.entry, "button_cancel"),
        ("account.move", docs.entry, "button_draft"),
        ("purchase.order", docs.order, "button_cancel"),
        ("sale.order", quotation, "action_cancel"),
        ("stock.picking", docs.picking, "action_confirm"),
        ("stock.picking", docs.picking, "action_cancel"),
    ] {
        let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
        let before = state_of(&mut tx, model, id).await;
        let records = tx.env().records(model).unwrap().browse([id]).unwrap();
        let () = records
            .call(&mut tx, method, ())
            .await
            .unwrap_or_else(|e| panic!("{model}.{method}: {e:?}"));
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
        let after = state_of(&mut tx, model, id).await;
        assert_ne!(before, after, "{model}.{method} changes the status");
        let rows = tx
            .query(
                "SELECT tracking::text, author_id IS NOT NULL FROM mail_message \
                 WHERE model = $1 AND res_id = $2 AND message_type = 'tracking' \
                 ORDER BY id DESC LIMIT 1",
                &[&model, &id],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "{model}.{method} leaves a history message");
        let tracking: serde_json::Value =
            serde_json::from_str(&rows[0].get::<_, String>(0)).unwrap();
        let state = tracking
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["n"] == "state")
            .unwrap_or_else(|| panic!("{model}.{method}: {tracking}"));
        assert_eq!(state["o"], before.as_str(), "{model}.{method}");
        assert_eq!(state["v"], after.as_str(), "{model}.{method}");
        assert!(rows[0].get::<_, bool>(1), "{model}.{method}: authored");
        tx.rollback().await.unwrap();
    }
    invoice_history(&history).await;
}

/// The history changes of field `name` on `account.move` `id`, oldest first.
async fn changes_of(tx: &mut Transaction, id: i64, name: &str) -> Vec<serde_json::Value> {
    tx.query(
        "SELECT tracking::text FROM mail_message WHERE model = 'account.move' AND res_id = $1 \
         AND message_type = 'tracking' ORDER BY id",
        &[&id],
    )
    .await
    .unwrap()
    .iter()
    .flat_map(|row| {
        let changes: serde_json::Value = serde_json::from_str(&row.get::<_, String>(0)).unwrap();
        changes.as_array().unwrap().clone()
    })
    .filter(|change| change["n"] == name)
    .collect()
}

/// The number posting gives an invoice and the payment status a full match sets are written by
/// the posting and matching statements, not the ORM write: both reach the invoice's history.
async fn invoice_history(history: &Orm) {
    let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
    let company = accounting::main_company(&mut tx).await;
    tx.commit().await.unwrap();
    drop(tx);
    accounting::periods(history, company).await;
    let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
    let b = accounting::books(&mut tx, company).await;
    let line = Command::create(
        RawVals::new()
            .set_cell("name", text("Service"))
            .set_cell("quantity", Cell::Decimal(1.into()))
            .set_cell("price_unit", Cell::Decimal(500.into())),
    );
    let date = accounting::d(2024, 1, 10);
    // created in its own transaction: a record's creation carries no field history
    let invoice = accounting::moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("move_type", text("out_invoice"))
                    .set_cell("partner_id", Cell::Id(b.customer))
                    .set_cell("invoice_date", Cell::Date(date))
                    .set_cell("date", Cell::Date(date))
                    .set_commands("invoice_line_ids", vec![line]),
            ],
        )
        .await
        .unwrap();
    let id = invoice.ids()[0];
    tx.commit().await.unwrap();
    drop(tx);

    let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
    let invoice = tx
        .env()
        .records("account.move")
        .unwrap()
        .browse([id])
        .unwrap();
    let () = invoice.call(&mut tx, "action_post", ()).await.unwrap();
    let payment = accounting::posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.bank,
        Some(b.customer),
        "-500",
        accounting::d(2024, 2, 1),
    )
    .await;
    tx.commit().await.unwrap();
    drop(tx);
    let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
    let number: String = tx
        .query("SELECT name FROM account_move WHERE id = $1", &[&id])
        .await
        .unwrap()[0]
        .get(0);
    let named = changes_of(&mut tx, id, "name").await;
    assert_eq!(named.len(), 1, "posting reports the number once: {named:?}");
    assert!(named[0]["o"].is_null(), "{named:?}");
    assert_eq!(named[0]["v"], number.as_str(), "{named:?}");
    assert!(
        changes_of(&mut tx, id, "payment_state").await.is_empty(),
        "no payment status change before the match"
    );

    let items = accounting::items_on(&mut tx, &[&invoice, &payment], b.receivable).await;
    accounting::try_call(&items, &mut tx, "reconcile")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    let mut tx = history.begin(SUPERUSER_ID).await.unwrap();
    let paid = changes_of(&mut tx, id, "payment_state").await;
    assert_eq!(paid.len(), 1, "{paid:?}");
    assert_eq!(paid[0]["od"], "Not Paid", "{paid:?}");
    assert_eq!(paid[0]["vd"], "Paid", "{paid:?}");
    tx.rollback().await.unwrap();
}

/// orders and transfers auto-installs the three bridges; the old rows take and read messages,
/// their forms are served with the chatter, a tracked field's change is captured, and the status
/// operations of entries, orders, quotations and transfers leave their history line)
/// with real users: a read-only accounting user posts on entries and orders, users without the
/// application or of another company get nothing)
#[test]
fn mail_on_a_populated_database_gives_entries_orders_and_transfers_their_chatter() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let system = system(&db);
        let initialized = Box::pin(system.initialize(InitOptions::new()))
            .await
            .unwrap();
        assert!(initialized.post_commit.is_clean());
        let names = ["account", "purchase", "stock"].map(|n| ModuleName::new(n).unwrap());
        let first =
            Box::pin(system.button_immediate_install(&system.system_env(Context::new()), &names))
                .await
                .unwrap();
        let mut tx = first.orm.begin(SUPERUSER_ID).await.unwrap();
        for model in ["account.move", "purchase.order", "stock.picking"] {
            assert!(
                !tx.env()
                    .records(model)
                    .unwrap()
                    .spec()
                    .inherits("mail.thread"),
                "{model} has no thread before mail"
            );
        }
        let docs = documents(&mut tx).await;
        tx.commit().await.unwrap();

        let second = Box::pin(system.button_immediate_install(
            &system.system_env(Context::new()),
            &[ModuleName::new("mail").unwrap()],
        ))
        .await
        .unwrap();
        assert!(second.post_commit.is_clean(), "{:?}", second.post_commit);
        let orm = second.orm;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let bridges: Vec<String> = tx
            .query(
                "SELECT name FROM ir_module_module WHERE name = ANY($1) AND state = 'installed' \
                 ORDER BY name",
                &[&BRIDGES.to_vec()],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect();
        assert_eq!(bridges, BRIDGES, "the bridges install with mail");
        superuser_author(&mut tx).await;
        for (model, id) in [
            ("account.move", docs.entry),
            ("purchase.order", docs.order),
            ("stock.picking", docs.picking),
        ] {
            let spec = tx.env().records(model).unwrap();
            assert!(spec.spec().inherits("mail.thread"), "{model}");
            assert!(spec.spec().inherits("mail.activity.mixin"), "{model}");
            let message = post(&mut tx, model, id, &format!("Note on {model}")).await;
            let record = tx.env().records(model).unwrap().browse([id]).unwrap();
            let thread = record.related(&mut tx, "message_ids").await.unwrap();
            assert!(thread.ids().contains(&message), "{model} reads its message");
        }

        // a change of a tracked field of an old row is captured for the thread
        let other = create(
            &mut tx,
            "res.partner",
            RawVals::new().set_cell("name", text("New vendor")),
        )
        .await;
        let changes = [
            ("account.move", docs.entry, "ref", text("Accrual")),
            ("purchase.order", docs.order, "partner_id", Cell::Id(other)),
            ("stock.picking", docs.picking, "partner_id", Cell::Id(other)),
        ];
        for (model, id, field, value) in &changes {
            tx.env()
                .records(model)
                .unwrap()
                .browse([*id])
                .unwrap()
                .write(&mut tx, RawVals::new().set_cell(*field, value.clone()))
                .await
                .unwrap();
        }
        let tracked = tx.take_tracked_changes().await.unwrap();
        for (model, id, field, value) in &changes {
            let record = tracked
                .iter()
                .find(|r| r.model() == *model && r.id().as_real().unwrap().get() == *id)
                .unwrap_or_else(|| panic!("{model} {id} is tracked: {tracked:?}"));
            let change = record
                .changes()
                .iter()
                .find(|c| c.field() == *field)
                .unwrap_or_else(|| panic!("{model}.{field}: {record:?}"));
            assert_eq!(change.new_value(), value, "{model}.{field}");
            let old = if *model == "purchase.order" {
                Cell::Id(docs.vendor)
            } else {
                Cell::Null
            };
            assert_eq!(change.old(), &old, "{model}.{field}");
        }
        tx.rollback().await.unwrap();

        let uid = admin_user(&orm).await;
        // the schema the web client fetched before sales was installed has the bridges' documents
        let (before, models) =
            schema_models(&store_fetch(&served_orm(&orm, &db).await, uid, None).await);
        for model in ["account.move", "purchase.order", "stock.picking"] {
            assert!(models.iter().any(|m| m == model), "{model} in {models:?}");
        }
        assert!(!models.iter().any(|m| m == "sale.order"), "{models:?}");
        for (model, form) in [
            ("account.move", "account.view_invoice_form"),
            ("account.move", "account.view_move_form"),
            ("purchase.order", "purchase.purchase_order_form"),
            ("stock.picking", "stock.view_picking_form"),
        ] {
            let described = served_form(&orm, &db, uid, model, form).await;
            assert!(has_chatter(&described), "{form} is served with its chatter");
        }
        drop(orm);

        // sales needs mail, so it installs after it
        let third = Box::pin(system.button_immediate_install(
            &system.system_env(Context::new()),
            &[ModuleName::new("sale").unwrap()],
        ))
        .await
        .unwrap();
        assert!(third.post_commit.is_clean(), "{:?}", third.post_commit);
        // the installed sales order is a threaded model: the next schema names it, under a new
        // generation, and its thread loads against it
        let mut tx = third.orm.begin(SUPERUSER_ID).await.unwrap();
        let order = create(
            &mut tx,
            "sale.order",
            RawVals::new().set_cell("partner_id", Cell::Id(docs.vendor)),
        )
        .await;
        // the administrator of this test becomes a sales manager to read the order
        let manager = one_id(
            &mut tx,
            "SELECT res_id::int8 FROM ir_model_data WHERE module = 'sales_team' \
             AND name = 'group_sale_manager' AND model = 'res.groups'",
        )
        .await;
        tx.env()
            .records("res.users")
            .unwrap()
            .browse([uid])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_commands(
                    "group_ids",
                    [Command::Link(RecId::from_real(manager).unwrap())],
                ),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let answer = store_fetch(
            &served_orm(&third.orm, &db).await,
            uid,
            Some(("sale.order", order)),
        )
        .await;
        let (after, models) = schema_models(&answer);
        assert!(models.iter().any(|m| m == "sale.order"), "{models:?}");
        assert_ne!(
            after, before,
            "installing a threaded model changes the generation"
        );
        assert_eq!(answer["topics"][1]["topic"], "thread", "{answer}");
        assert_eq!(answer["topics"][1]["ok"], true, "{answer}");
        status_history(&third.orm, &db, &docs).await;
        bridge_access(&third.orm, &db, &docs).await;
        drop(third);
        db.cleanup().await.unwrap();
    });
}
