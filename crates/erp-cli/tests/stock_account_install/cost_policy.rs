// SPDX-License-Identifier: MIT OR Apache-2.0
//! Ordinary company and category configuration for the stock-admission policy.

use super::*;
use erp_sdk::{Records, Transaction, Value};

#[test]
fn company_cost_policy_is_scoped_and_preserves_existing_valuation_history() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(verify_cost_policy());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn verify_cost_policy() {
    let db = TestDb::create().await.unwrap();
    let lifecycle = system(&db);
    let initialized =
        Box::pin(lifecycle.initialize(InitOptions::new().with_skip_auto_install(true)))
            .await
            .unwrap();
    let installed = Box::pin(lifecycle.button_immediate_install(
        &initialized.orm.env(SUPERUSER_ID, Context::new()),
        &[ModuleName::new("stock_account").unwrap()],
    ))
    .await
    .unwrap();
    assert!(installed.post_commit.is_clean());
    let orm = installed.orm;
    let mut setup = orm.begin(SUPERUSER_ID).await.unwrap();
    let ids = setup
        .query(
            "SELECT c.id,c.currency_id,
        (SELECT res_id FROM ir_model_data WHERE module='base' AND name='group_system'),
        (SELECT res_id FROM ir_model_data WHERE module='stock' AND name='group_stock_manager')
        FROM res_company c ORDER BY c.id LIMIT 1",
            &[],
        )
        .await
        .unwrap();
    let (company, currency, admin_group, reader_group): (i64, i64, i64, i64) =
        (ids[0].get(0), ids[0].get(1), ids[0].get(2), ids[0].get(3));
    let env = setup.env();
    let peer = create(
        &mut setup,
        &env.records("res.company").unwrap(),
        RawVals::new()
            .set_cell("name", Cell::Str("Cost policy peer company".into()))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let users = env.records("res.users").unwrap();
    let administrator = create(
        &mut setup,
        &users,
        user_values("cost_policy_admin", company, admin_group, &[company, peer]),
    )
    .await;
    let reader = create(
        &mut setup,
        &users,
        user_values(
            "cost_policy_reader",
            company,
            reader_group,
            &[company, peer],
        ),
    )
    .await;
    let foreign_reader = create(
        &mut setup,
        &users,
        user_values("cost_policy_restricted", company, reader_group, &[company]),
    )
    .await;
    let categories = env
        .with_company(company)
        .unwrap()
        .records("product.category")
        .unwrap();
    let parent = create(
        &mut setup,
        &categories,
        RawVals::new().set_cell("name", Cell::Str("Cost policy parent".into())),
    )
    .await;
    let category = create(
        &mut setup,
        &categories,
        RawVals::new()
            .set_cell("name", Cell::Str("Cost policy leaf".into()))
            .set_cell("parent_id", Cell::Id(parent)),
    )
    .await;
    let templates = env
        .with_company(company)
        .unwrap()
        .records("product.template")
        .unwrap();
    let product = create(
        &mut setup,
        &templates,
        product_values("Shared costing test", category),
    )
    .await;
    let owned = create(
        &mut setup,
        &env.with_company(peer)
            .unwrap()
            .records("product.template")
            .unwrap(),
        product_values("Peer costing test", category).set_cell("company_id", Cell::Id(peer)),
    )
    .await;
    let variant: i64 = setup
        .query(
            "SELECT id FROM product_product WHERE product_tmpl_id=$1",
            &[&product],
        )
        .await
        .unwrap()[0]
        .get(0);
    setup.commit().await.unwrap();
    drop(setup);

    let policy = PolicySource::build(Arc::clone(orm.registry()), db.pool().clone())
        .await
        .unwrap();
    let runtime = Orm::builder(Arc::clone(orm.registry()), db.pool().clone())
        .require_core_tables(erp_security::auth::core_tables())
        .resolver(Resolver::for_registry(orm.registry()).unwrap())
        .policy(Arc::new(policy) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .build()
        .unwrap();
    let mut tx = runtime
        .begin_caller(UserId::new(administrator).unwrap())
        .await
        .unwrap();
    let env = tx.env().with_context([(
        "allowed_company_ids",
        Value::list([Value::Int(company), Value::Int(peer)]),
    )]);
    let main = env
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap();
    let peer_company = env.records("res.company").unwrap().browse([peer]).unwrap();
    let parent = env
        .records("product.category")
        .unwrap()
        .browse([parent])
        .unwrap();
    let leaf = env
        .records("product.category")
        .unwrap()
        .browse([category])
        .unwrap();
    let value = |method: &str| Cell::Str(method.into());
    assert_eq!(
        main.cell(&mut tx, "cost_method").await.unwrap(),
        value("standard")
    );
    assert_eq!(
        peer_company.cell(&mut tx, "cost_method").await.unwrap(),
        value("standard")
    );
    assert_eq!(
        leaf.cell(&mut tx, "property_cost_method").await.unwrap(),
        value("standard")
    );
    parent
        .write(&mut tx, method_values("property_cost_method", "average"))
        .await
        .unwrap();
    peer_company
        .write(&mut tx, method_values("cost_method", "fifo"))
        .await
        .unwrap();
    assert_eq!(
        env.with_company(peer)
            .unwrap()
            .records("product.category")
            .unwrap()
            .browse([category])
            .unwrap()
            .cell(&mut tx, "property_cost_method")
            .await
            .unwrap(),
        value("fifo")
    );
    let current = env
        .records("product.template")
        .unwrap()
        .browse([product])
        .unwrap();
    let owner_scoped = env
        .records("product.template")
        .unwrap()
        .browse([owned])
        .unwrap();
    assert_eq!(
        current.cell(&mut tx, "cost_method").await.unwrap(),
        value("standard")
    );
    assert_eq!(
        owner_scoped.cell(&mut tx, "cost_method").await.unwrap(),
        value("fifo")
    );
    leaf.write(&mut tx, method_values("property_cost_method", "average"))
        .await
        .unwrap();
    assert_eq!(
        current.cell(&mut tx, "cost_method").await.unwrap(),
        value("average")
    );
    leaf.write(&mut tx, method_values("property_cost_method", "standard"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    drop(tx);

    let user = UserId::new(reader).unwrap();
    let mut tx = runtime.begin_caller(user).await.unwrap();
    let env = tx.env().with_context([(
        "allowed_company_ids",
        Value::list([Value::Int(company), Value::Int(peer)]),
    )]);
    for (context, method) in [
        (env.clone(), "standard"),
        (env.with_company(peer).unwrap(), "fifo"),
    ] {
        let record = context
            .records("product.product")
            .unwrap()
            .browse([variant])
            .unwrap();
        assert_eq!(
            record.cell(&mut tx, "cost_method").await.unwrap(),
            value(method)
        );
    }
    let main = env
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap();
    let leaf = env
        .records("product.category")
        .unwrap()
        .browse([category])
        .unwrap();
    for (records, field, description) in [
        (main, "cost_method", "'Company' (res.company)"),
        (
            leaf,
            "property_cost_method",
            "'Product Category' (product.category)",
        ),
    ] {
        let err = records
            .write(&mut tx, method_values(field, "average"))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Access);
        assert!(err.message().contains(description), "{err}");
    }
    tx.rollback().await.unwrap();
    drop(tx);
    let mut tx = runtime
        .begin_caller(UserId::new(foreign_reader).unwrap())
        .await
        .unwrap();
    let forbidden = tx
        .env()
        .with_company(peer)
        .unwrap()
        .records("product.product")
        .unwrap()
        .browse([variant])
        .unwrap();
    let err = forbidden.cell(&mut tx, "cost_method").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Access);
    assert_eq!(
        err.message(),
        "One of the selected companies does not exist or is not one of yours."
    );
    tx.rollback().await.unwrap();
    drop(tx);

    let mut setup = orm.begin(SUPERUSER_ID).await.unwrap();
    let source = seed_valuation_source(&mut setup, company, variant).await;
    setup.commit().await.unwrap();
    drop(setup);
    let mut tx = runtime
        .begin_caller(UserId::new(administrator).unwrap())
        .await
        .unwrap();
    let env = tx.env().with_context([(
        "allowed_company_ids",
        Value::list([Value::Int(company), Value::Int(peer)]),
    )]);
    let main = env
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap();
    let leaf = env
        .records("product.category")
        .unwrap()
        .browse([category])
        .unwrap();
    let historical = env
        .records("product.template")
        .unwrap()
        .browse([product])
        .unwrap();
    for (record, vals) in [
        (main.clone(), method_values("cost_method", "average")),
        (leaf, method_values("property_cost_method", "fifo")),
        (
            historical.clone(),
            RawVals::new().set_cell("categ_id", Cell::Id(parent.ids()[0])),
        ),
        (
            historical.clone(),
            RawVals::new().set_cell("company_id", Cell::Id(peer)),
        ),
    ] {
        refuse_transition(&mut tx, &record, vals).await;
    }
    main.write(&mut tx, method_values("cost_method", "standard"))
        .await
        .unwrap();
    let peer_company = env.records("res.company").unwrap().browse([peer]).unwrap();
    peer_company
        .write(&mut tx, method_values("cost_method", "average"))
        .await
        .unwrap();
    assert_eq!(
        historical.cell(&mut tx, "cost_method").await.unwrap(),
        value("standard")
    );
    assert_eq!(
        tx.query(
            "SELECT method::text FROM stk_value_source WHERE id=$1",
            &[&source]
        )
        .await
        .unwrap()[0]
            .get::<_, String>(0),
        "standard"
    );
    peer_company
        .write(&mut tx, method_values("cost_method", "standard"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    drop(tx);

    // A shared product gathered primary history in both companies at Standard.
    let mut setup = orm.begin(SUPERUSER_ID).await.unwrap();
    let peer_source = seed_valuation_source(&mut setup, peer, variant).await;
    setup.commit().await.unwrap();
    drop(setup);
    let mut tx = runtime
        .begin_caller(UserId::new(administrator).unwrap())
        .await
        .unwrap();
    let env = tx.env().with_context([(
        "allowed_company_ids",
        Value::list([Value::Int(company), Value::Int(peer)]),
    )]);
    let historical = env
        .records("product.template")
        .unwrap()
        .browse([product])
        .unwrap();
    let peer_company = env.records("res.company").unwrap().browse([peer]).unwrap();

    // Same-effective-method ownership transfers work. With the product owned by the
    // main company, a different default in the peer does not yet affect the product.
    historical
        .write_base(
            &mut tx,
            RawVals::new().set_cell("company_id", Cell::Id(company)),
        )
        .await
        .unwrap();
    tx.flush().await.unwrap();
    peer_company
        .write(&mut tx, method_values("cost_method", "fifo"))
        .await
        .unwrap();
    refuse_transition(
        &mut tx,
        &historical,
        RawVals::new().set_cell("company_id", Cell::Null),
    )
    .await;
    assert_eq!(
        historical.cell(&mut tx, "company_id").await.unwrap(),
        Cell::Id(company)
    );
    assert_eq!(
        historical.cell(&mut tx, "cost_method").await.unwrap(),
        value("standard")
    );
    peer_company
        .write(&mut tx, method_values("cost_method", "standard"))
        .await
        .unwrap();
    historical
        .write_base(&mut tx, RawVals::new().set_cell("company_id", Cell::Null))
        .await
        .unwrap();
    tx.flush().await.unwrap();
    assert_eq!(
        historical.cell(&mut tx, "company_id").await.unwrap(),
        Cell::Null
    );
    let stored: Vec<_> = tx.query(
        "SELECT company_id, method::text FROM stk_value_source WHERE id IN ($1,$2) ORDER BY company_id",
        &[&source, &peer_source],
    ).await.unwrap().iter().map(|row| (row.get::<_,i64>(0),row.get::<_,String>(1))).collect();
    assert_eq!(
        stored,
        [
            (company, "standard".to_owned()),
            (peer, "standard".to_owned())
        ]
    );
    tx.commit().await.unwrap();
    drop(tx);

    // Present the 1.5 catalog shape while keeping both companies' real source rows.
    // The native descriptors are current; this exercises the database upgrade path.
    let client = db.client().await.unwrap();
    client.batch_execute(
        "BEGIN;
        DROP TRIGGER stk_val_cost_variant_update ON product_product;
        DROP TRIGGER stk_val_cost_category_delete ON product_category;
        DROP FUNCTION stk_val_fn_cost_variant();
        DROP FUNCTION stk_val_fn_cost_category_delete();
        DELETE FROM erp_schema_sql_object WHERE addon='stock_account'
            AND key IN ('cost_variant_update','cost_category_delete','fn_cost_variant','fn_cost_category_delete');
        UPDATE ir_module_module SET latest_version='1.5.0' WHERE name='stock_account';
        COMMIT;",
    ).await.unwrap();
    let upgraded = Box::pin(lifecycle.button_immediate_upgrade(
        &orm.env(SUPERUSER_ID, Context::new()),
        &[ModuleName::new("stock_account").unwrap()],
    ))
    .await
    .unwrap();
    assert!(upgraded.post_commit.is_clean());
    let mut tx = upgraded.orm.begin(SUPERUSER_ID).await.unwrap();
    let catalog = tx.query(
        "SELECT latest_version, (SELECT count(*) FROM erp_schema_sql_object WHERE addon='stock_account'
         AND key IN ('cost_variant_update','cost_category_delete','fn_cost_variant','fn_cost_category_delete'))
         FROM ir_module_module WHERE name='stock_account'", &[],
    ).await.unwrap();
    assert_eq!(catalog[0].get::<_, String>(0), "1.8.0");
    assert_eq!(catalog[0].get::<_, i64>(1), 4);
    let env = tx.env().with_company(company).unwrap();
    let categories = env.records("product.category").unwrap();
    let templates = env.records("product.template").unwrap();
    let variants = env.records("product.product").unwrap();
    let make_category = |name: &str| RawVals::new().set_cell("name", Cell::Str(name.into()));
    let root = create(&mut tx, &categories, make_category("Guarded root")).await;
    let child = create(
        &mut tx,
        &categories,
        make_category("Guarded child").set_cell("parent_id", Cell::Id(root)),
    )
    .await;
    let ordinary = create(&mut tx, &categories, make_category("Ordinary deletion")).await;
    categories
        .browse([ordinary])
        .unwrap()
        .write(&mut tx, method_values("property_cost_method", "standard"))
        .await
        .unwrap();
    let different = create(
        &mut tx,
        &templates,
        product_values("FIFO only at peer", child),
    )
    .await;
    let equal = create(
        &mut tx,
        &templates,
        product_values("Same effective method", ordinary),
    )
    .await;
    let get_variant = async |tx: &mut Transaction, template: i64| -> i64 {
        tx.query(
            "SELECT id FROM product_product WHERE product_tmpl_id=$1",
            &[&template],
        )
        .await
        .unwrap()[0]
            .get(0)
    };
    let guarded_variant = get_variant(&mut tx, different).await;
    let sibling = get_variant(&mut tx, equal).await;
    env.with_company(peer)
        .unwrap()
        .records("product.category")
        .unwrap()
        .browse([child])
        .unwrap()
        .write(&mut tx, method_values("property_cost_method", "fifo"))
        .await
        .unwrap();
    tx.flush().await.unwrap();
    seed_source_method(&mut tx, peer, guarded_variant, "fifo").await;
    // Archived variants keep their valuation history and can be reassigned without a
    // duplicate active combination on the target template.
    let batch = variants.browse([sibling, variant]).unwrap();
    batch
        .write(
            &mut tx,
            RawVals::new().set_cell("active", Cell::Bool(false)),
        )
        .await
        .unwrap();
    variants
        .browse([variant])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("product_tmpl_id", Cell::Id(equal)),
        )
        .await
        .unwrap();
    tx.flush().await.unwrap();
    let target = RawVals::new().set_cell("product_tmpl_id", Cell::Id(different));
    let variant_error = tx
        .savepoint(async |tx| {
            batch.write(tx, target.clone()).await?;
            tx.flush().await
        })
        .await
        .unwrap_err();
    assert_transition(variant_error);
    refuse_transition(&mut tx, &batch, target).await;
    let original = tx
        .query(
            "SELECT product_tmpl_id FROM product_product WHERE id=ANY($1) ORDER BY id",
            &[&vec![sibling, variant]],
        )
        .await
        .unwrap();
    assert_eq!(original.len(), 2);
    assert!(original.iter().all(|row| row.get::<_, i64>(0) == equal));
    // Both direct child deletion and cascading deletion of its parent must see the
    // original override. The safe sibling in a mixed deletion is rolled back too.
    for ids in [vec![child], vec![ordinary, root]] {
        let deleting = categories.browse(ids).unwrap();
        let err = tx
            .savepoint(async |tx| deleting.unlink(tx).await)
            .await
            .unwrap_err();
        assert_transition(err);
    }
    let lower = tx
        .savepoint(async |tx| {
            tx.execute("DELETE FROM product_category WHERE id=$1", &[&root])
                .await
        })
        .await
        .unwrap_err();
    assert_transition(lower);
    let retained: i64 = tx
        .query(
            "SELECT count(*) FROM product_category WHERE id=ANY($1)",
            &[&vec![ordinary, root, child]],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(retained, 3);
    categories
        .browse([ordinary])
        .unwrap()
        .unlink(&mut tx)
        .await
        .unwrap();
    assert!(
        tx.query(
            "SELECT categ_id IS NULL FROM product_template WHERE id=$1",
            &[&equal]
        )
        .await
        .unwrap()[0]
            .get::<_, bool>(0)
    );
    tx.commit().await.unwrap();
}

async fn refuse_transition(tx: &mut Transaction, record: &Records, vals: RawVals) {
    let err = tx
        .savepoint(async |tx| {
            record.write_base(tx, vals).await?;
            tx.flush().await
        })
        .await
        .unwrap_err();
    assert_transition(err);
}

fn assert_transition(err: erp_sdk::Error) {
    assert_eq!(err.sqlstate(), Some("ERP67"));
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.constraint(),
        Some("stock_valuation_cost_method_transition")
    );
    assert_eq!(
        err.message(),
        "A costing method cannot change after valuation starts. Keep the current method until dated costing transitions are available."
    );
}

async fn seed_valuation_source(tx: &mut Transaction, company: i64, variant: i64) -> i64 {
    seed_source_method(tx, company, variant, "standard").await
}

async fn seed_source_method(tx: &mut Transaction, company: i64, variant: i64, method: &str) -> i64 {
    tx.query("INSERT INTO stk_value_source(company_id,product_id,origin_model,origin_id,component,source_revision,kind,cost_partition,effective_time,affected_from_time,method,policy,schema_version,semantic_version,request_canonical,request_digest) VALUES($1,$2,'product.product',$2,'policy-fixture',0,'Movement','','2026-01-01','2026-01-01',$3,'cost_scale_v1',1,1,'{}','policy-fixture') RETURNING id", &[&company,&variant,&method]).await.unwrap()[0].get(0)
}

async fn create(tx: &mut Transaction, records: &Records, values: RawVals) -> i64 {
    records.create(tx, vec![values]).await.unwrap().ids()[0]
}

fn user_values(login: &str, company: i64, group: i64, allowed: &[i64]) -> RawVals {
    RawVals::new()
        .set_cell("name", Cell::Str(login.into()))
        .set_cell("login", Cell::Str(login.into()))
        .set_cell("company_id", Cell::Id(company))
        .set_commands(
            "company_ids",
            [Command::set(
                allowed.iter().map(|&id| RecId::from_real(id).unwrap()),
            )],
        )
        .set_commands(
            "group_ids",
            [Command::set([RecId::from_real(group).unwrap()])],
        )
}

fn product_values(name: &str, category: i64) -> RawVals {
    RawVals::new()
        .set_cell("name", Cell::Str(name.into()))
        .set_cell("categ_id", Cell::Id(category))
        .set_cell("is_storable", Cell::Bool(true))
}

fn method_values(field: &'static str, method: &str) -> RawVals {
    RawVals::new().set_cell(field, Cell::Str(method.into()))
}
