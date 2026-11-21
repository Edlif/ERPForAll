// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exercise the stock completion callback under the ordinary request database login.

use super::*;
use erp_modules::{AddonIndex, InitOptions, ModuleName, ModuleSystem};
use erp_sdk::{
    Cell, Command, Context, Decimal, ErrorKind, Operation, RawVals, RecId, Transaction, UserId,
};
use erp_security::{resolver::Resolver, source::PolicySource};
use erp_testkit::{TestDb, tokio_postgres::NoTls};

use std::path::Path;

#[test]
fn stock_completion_observer_receives_only_transitioned_lines_and_rolls_back_late_failure() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(16 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(check_completion_callback())
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn check_completion_callback() {
    let db = TestDb::create().await.unwrap();
    let owner = format!("{}_vo", db.name());
    let request = format!("{}_vr", db.name());
    let definer = format!("{}_vd", db.name());
    let (owner_sql, request_sql, definer_sql) = (
        erp_sql::quote(&owner),
        erp_sql::quote(&request),
        erp_sql::quote(&definer),
    );
    let former_sql = erp_sql::quote(&format!("{request}_old"));
    let password = "fixture-valuation-request-password";
    let admin = db.client().await.unwrap();
    admin
        .batch_execute(&format!(
            "CREATE ROLE {owner_sql} NOLOGIN;
         CREATE ROLE {request_sql} LOGIN PASSWORD {};
         CREATE ROLE {definer_sql} NOLOGIN;
         CREATE ROLE {former_sql} LOGIN;
         GRANT {definer_sql} TO {owner_sql} WITH INHERIT TRUE, SET TRUE;
         ALTER SCHEMA public OWNER TO {owner_sql};
         GRANT CREATE ON DATABASE {} TO {owner_sql}",
            erp_sql::literal(password),
            erp_sql::quote(db.name())
        ))
        .await
        .unwrap();
    let db_config = db.config();
    let observation = tokio::task::LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(90),
                    observe_admission(db_config, admin, owner, request, definer, password),
                )
                .await
                .expect("stock receipt concurrency fixture timed out");
            })
            .await
        })
        .await;
    // Preserve cleanup on a setup/read failure and on the intentional test-first denial.
    let mut cleanup = db.config();
    cleanup.dbname("postgres");
    db.cleanup().await.unwrap();
    let (cleanup, connection) = cleanup.connect(NoTls).await.unwrap();
    tokio::spawn(connection);
    cleanup
        .batch_execute(&format!(
            "DROP ROLE {owner_sql}, {request_sql}, {definer_sql}, {former_sql}"
        ))
        .await
        .unwrap();
    if let Err(error) = observation {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("stock completion observation was cancelled: {error}");
    }
}

async fn observe_admission(
    db_config: erp_testkit::tokio_postgres::Config,
    admin: deadpool_postgres::Object,
    owner: String,
    request: String,
    definer: String,
    password: &str,
) {
    let mut owner_config = db_config.clone();
    owner_config.options(format!("-c role={owner}"));
    let owner_pool = erp_orm::pool::create_pool(owner_config, 2).unwrap();
    let mut request_config = db_config;
    request_config.user(&request).password(password);
    let request_pool = erp_orm::pool::create_pool(request_config, 3).unwrap();
    let options = SchemaSyncOptions::new().sql_roles(
        SqlRoleBindings::new()
            .request_owned(request.clone())
            .definer_owned(definer.clone()),
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../addons")
        .canonicalize()
        .unwrap();
    let mut index = AddonIndex::new(true);
    index.add_dev_path(&root);
    let available_sources: Vec<_> = index.discover_entries().unwrap().into_keys().collect();
    let identities = crate::identity::native_artifact_identities().unwrap();
    assert!(
        identities.contains_key(&ModuleName::new("valuation").unwrap()),
        "the shipping CLI must carry the valuation native artifact receipt"
    );
    static PROBE_MODEL: erp_registry::ModelDecl =
        erp_registry::ModelDecl::new("stock_account", "stock.move.line")
            .inherit(&["stock.move.line"]);
    let modules = ModuleSystem::builder(owner_pool.clone(), index)
        .decls([&PROBE_MODEL])
        .schema_sync_options(options.clone())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .require_sql_objects(crate::native_addons::SQL_MANIFESTS.iter().copied())
        .require_sql_uninstall_guards(crate::native_addons::UNINSTALL_MANIFESTS.iter().copied())
        .native_artifact_identities(identities)
        .code(crate::native_addons::DESCRIPTORS.iter().copied())
        .view_types(erp_addon_web::view_type_contributions())
        .build()
        .unwrap();
    eprintln!("receipt-fixture phase=initialize");
    let init = Box::pin(modules.initialize(InitOptions::new().with_skip_auto_install(true)))
        .await
        .unwrap();
    let base_stock = Box::pin(modules.button_immediate_install(
        &init.orm.env(SUPERUSER_ID, Context::new()),
        &[ModuleName::new("stock_account").unwrap()],
    ))
    .await
    .unwrap();
    let mut before = base_stock.orm.begin(SUPERUSER_ID).await.unwrap();
    grant_ordinary(&mut before, &request, base_stock.orm.registry())
        .await
        .unwrap();
    assert!(
        before
            .query(
                "SELECT has_table_privilege($1::name,'public.stk_cost_control','INSERT')",
                &[&request]
            )
            .await
            .unwrap()[0]
            .get::<_, bool>(0)
    );
    let former = format!("{request}_old");
    let former_sql = erp_sql::quote(&former);
    grant_ordinary(&mut before, &former, base_stock.orm.registry())
        .await
        .unwrap();
    before.commit().await.unwrap();
    drop(before);
    let refused = match Box::pin(modules.button_immediate_install(
        &base_stock.orm.env(SUPERUSER_ID, Context::new()),
        &[ModuleName::new("valuation").unwrap()],
    ))
    .await
    {
        Ok(_) => panic!("old request role still has direct grants on the protected control"),
        Err(error) => error,
    };
    let refusal = crate::error::CliError::ModuleOperation(Box::new(refused)).to_string();
    assert!(refusal.contains(&former), "{refusal}");
    let sequence_name: String = admin
        .query_one(
            "SELECT pg_get_serial_sequence('public.stk_cost_control','id')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    admin
        .batch_execute(&format!(
            "REVOKE ALL ON public.stk_cost_control FROM {former_sql};
        REVOKE ALL ON SEQUENCE {sequence_name} FROM {former_sql}"
        ))
        .await
        .unwrap();
    eprintln!("receipt-fixture phase=valuation-install");
    let installed = Box::pin(modules.button_immediate_install(
        &base_stock.orm.env(SUPERUSER_ID, Context::new()),
        &[ModuleName::new("valuation").unwrap()],
    ))
    .await
    .unwrap();
    assert!(installed.post_commit.is_clean());
    // The earlier v16 binary and its addon tree lack valuation. The publisher must refuse
    // an incomplete installed graph before it can calculate or write ordinary grants.
    let mut previous_index = AddonIndex::new(true);
    for name in available_sources
        .iter()
        .filter(|name| name.as_str() != "valuation")
    {
        previous_index.add_dev_path(root.join(name.as_str()));
    }
    let predecessor_sources = previous_index.discover_entries().unwrap();
    let missing: Vec<_> = available_sources
        .iter()
        .filter(|name| !predecessor_sources.contains_key(*name))
        .map(ModuleName::as_str)
        .collect();
    assert_eq!(missing, ["valuation"]);
    let mut previous_identities = crate::identity::native_artifact_identities().unwrap();
    previous_identities.remove(&ModuleName::new("valuation").unwrap());
    let previous = ModuleSystem::builder(owner_pool, previous_index)
        .native_artifact_identities(previous_identities)
        .code(
            crate::native_addons::DESCRIPTORS
                .iter()
                .copied()
                .filter(|addon| addon.name != "valuation"),
        )
        .build()
        .unwrap();
    let mut previous_tx = installed.orm.begin(SUPERUSER_ID).await.unwrap();
    let unavailable = previous
        .registry_for_publication(&mut previous_tx)
        .await
        .unwrap_err();
    assert!(
        unavailable.to_string().contains("unavailable"),
        "{unavailable}"
    );
    previous_tx.rollback().await.unwrap();
    drop(previous_tx);
    eprintln!("receipt-fixture phase=authorization-and-receipt");
    let registry = installed.orm.registry().clone();
    let mut tx = installed.orm.begin(SUPERUSER_ID).await.unwrap();
    assert_eq!(
        tx.query("SELECT current_user::text", &[]).await.unwrap()[0].get::<_, String>(0),
        owner
    );
    tx.reconcile_sql_object_grants(&registry, &options)
        .await
        .unwrap();
    grant_ordinary(&mut tx, &request, &registry).await.unwrap();
    // Stock's unit registration uses the same scoped helper as runtime publication.
    crate::uom_grants::publish(&mut tx, &request).await.unwrap();
    tx.verify_sql_object_publication(&registry, &options)
        .await
        .unwrap();
    for grant in [
        "SELECT ON public.stk_cost_control".to_owned(),
        "UPDATE(seal_revision) ON public.stk_cost_control".to_owned(),
        format!("USAGE ON SEQUENCE {sequence_name}"),
    ] {
        let privilege_error = tx
            .savepoint(async |tx| {
                tx.execute(&format!("GRANT {grant} TO {former_sql}"), &[])
                    .await?;
                tx.verify_sql_object_publication(&registry, &options).await
            })
            .await
            .expect_err("a former login acquired independent access");
        assert_eq!(privilege_error.kind(), ErrorKind::Access);
        assert!(
            privilege_error.message().contains(&former),
            "{privilege_error}"
        );
    }
    let company: i64 = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let group: i64 = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='stock' AND name='group_stock_manager'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let admin_group: i64 = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='base' AND name='group_system'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let user_values = |login: &str, group: i64| {
        RawVals::new()
            .set_cell("name", Cell::Str(login.into()))
            .set_cell("login", Cell::Str(login.into()))
            .set_cell("company_id", Cell::Id(company))
            .set_commands(
                "company_ids",
                [Command::Set(vec![RecId::from_real(company).unwrap()])],
            )
            .set_commands(
                "group_ids",
                [Command::Set(vec![RecId::from_real(group).unwrap()])],
            )
    };
    // ModuleSystem initialization leaves credential activation to its caller.
    create(
        &mut tx,
        company,
        "res.users",
        user_values("valuation-fixture-admin", admin_group),
    )
    .await;
    let manager = create(
        &mut tx,
        company,
        "res.users",
        user_values("valuation-stock-manager", group),
    )
    .await;
    let ordinary_group: i64 = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='base' AND name='group_user'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let ordinary = UserId::new(
        create(
            &mut tx,
            company,
            "res.users",
            user_values("receipt-reader", ordinary_group),
        )
        .await,
    )
    .unwrap();
    let stock_group: i64 = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='stock' AND name='group_stock_user'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let stock_user = UserId::new(
        create(
            &mut tx,
            company,
            "res.users",
            user_values("receipt-stock-user", stock_group),
        )
        .await,
    )
    .unwrap();
    let currency: i64 = tx
        .query(
            "SELECT currency_id FROM res_company WHERE id=$1",
            &[&company],
        )
        .await
        .unwrap()[0]
        .get(0);
    let foreign = create(
        &mut tx,
        company,
        "res.company",
        RawVals::new()
            .set_cell("name", Cell::Str("Off-scope valuation company".into()))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let non_template = create(
        &mut tx,
        company,
        "product.template",
        RawVals::new()
            .set_cell("name", Cell::Str("Non-storable receipt".into()))
            .set_cell("is_storable", Cell::Bool(false)),
    )
    .await;
    let non_storable: i64 = tx
        .query(
            "SELECT id FROM product_product WHERE product_tmpl_id=$1",
            &[&non_template],
        )
        .await
        .unwrap()[0]
        .get(0);
    let template = create(
        &mut tx,
        company,
        "product.template",
        RawVals::new()
            .set_cell("name", Cell::Str("Valuation receipt source".into()))
            .set_cell("is_storable", Cell::Bool(true)),
    )
    .await;
    let product: i64 = tx
        .query(
            "SELECT id FROM product_product WHERE product_tmpl_id=$1",
            &[&template],
        )
        .await
        .unwrap()[0]
        .get(0);
    let extra_template = create(
        &mut tx,
        company,
        "product.template",
        RawVals::new()
            .set_cell("name", Cell::Str("Concurrent first receipt".into()))
            .set_cell("is_storable", Cell::Bool(true)),
    )
    .await;
    let extra_product: i64 = tx
        .query(
            "SELECT id FROM product_product WHERE product_tmpl_id=$1",
            &[&extra_template],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.commit().await.unwrap();
    drop(tx);
    let policy = PolicySource::build(registry.clone(), request_pool.clone())
        .await
        .unwrap();
    let runtime = Orm::builder(registry.clone(), request_pool)
        .require_core_tables(erp_security::auth::core_tables())
        .resolver(Resolver::for_registry(&registry).unwrap())
        .policy(Arc::new(policy.clone()) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .impls([&OBSERVER])
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap();
    let manager = UserId::new(manager).unwrap();
    let mut tx = runtime.begin_caller(manager).await.unwrap();
    assert_eq!(
        tx.query("SELECT current_user::text", &[]).await.unwrap()[0].get::<_, String>(0),
        request
    );
    let operation = &tx
        .query(
            "SELECT id, default_location_src_id, default_location_dest_id
         FROM stock_picking_type WHERE code='incoming' AND company_id=$1 ORDER BY id LIMIT 1",
            &[&company],
        )
        .await
        .unwrap()[0];
    let (kind, src, dest): (i64, i64, i64) = (operation.get(0), operation.get(1), operation.get(2));
    let picking =
        create_transfer(&mut tx, company, kind, src, dest, product, Decimal::from(7)).await;
    let nonvalued = create_transfer(
        &mut tx,
        company,
        kind,
        src,
        dest,
        non_storable,
        Decimal::ONE,
    )
    .await;
    tx.commit().await.unwrap();
    drop(tx);
    {
        let mut scoped = runtime.begin_caller(manager).await.unwrap();
        assert_eq!(
            validate_picking(&mut scoped, foreign, picking)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Access
        );
        scoped.rollback().await.unwrap();
    }
    {
        let mut clerk = runtime.begin_caller(stock_user).await.unwrap();
        let error = validate_picking(&mut clerk, company, picking)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access);
        assert_eq!(
            error.message(),
            "Only stock managers may admit a valued stock receipt."
        );
        validate_picking(&mut clerk, company, nonvalued)
            .await
            .unwrap();
        clerk.commit().await.unwrap();
        assert_eq!(
            admin
                .query_one("SELECT count(*) FROM stk_cost_control", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
    {
        let mut tx = runtime.begin_caller(ordinary).await.unwrap();
        let denied = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("stock.picking")
            .unwrap()
            .browse([picking])
            .unwrap()
            .call::<(), ()>(&mut tx, "button_validate", ())
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Access, "{denied}");
        assert!(tx.extension::<CompletionProbe>().observed.is_empty());
        tx.commit().await.unwrap();
        drop(tx);
    }
    let mut tx = runtime.begin_caller(manager).await.unwrap();
    tx.execute(
        "CREATE TEMP TABLE stock_completion_probe (line_id bigint PRIMARY KEY) ON COMMIT DROP",
        &[],
    )
    .await
    .unwrap();
    {
        tx.extension::<CompletionProbe>().enabled = true;
        tx.extension::<CompletionProbe>().refuse = true;
        let transfer = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("stock.picking")
            .unwrap()
            .browse([picking])
            .unwrap();
        let error = transfer
            .call::<(), ()>(&mut tx, "button_validate", ())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation, "{error}");
        assert_eq!(
            error.message(),
            "Receipt observer fixture declined the completed transfer."
        );
        // The caller intentionally catches the error and keeps using the transaction.
        let row=&tx.query("SELECT state, date_done IS NULL,
            (SELECT count(*) FROM stock_move_line WHERE picking_id=p.id AND done_at IS NOT NULL),
            (SELECT COALESCE(sum(quantity),0)::text FROM stock_quant WHERE company_id=$2 AND product_id=$3 AND location_id=$4)
            FROM stock_picking p WHERE p.id=$1", &[&picking,&company,&product,&dest]).await.unwrap()[0];
        assert_eq!(row.get::<_, String>(0), "draft");
        assert!(row.get::<_, bool>(1));
        assert_eq!(row.get::<_, i64>(2), 0);
        assert_eq!(
            row.get::<_, String>(3).parse::<Decimal>().unwrap(),
            Decimal::ZERO
        );
        assert_eq!(
            tx.query("SELECT count(*) FROM pg_temp.stock_completion_probe", &[])
                .await
                .unwrap()[0]
                .get::<_, i64>(0),
            0
        );
        tx.extension::<CompletionProbe>().refuse = false;
    }
    tx.env()
        .with_company(company)
        .unwrap()
        .records("stock.picking")
        .unwrap()
        .browse([picking])
        .unwrap()
        .call::<(), ()>(&mut tx, "button_validate", ())
        .await
        .unwrap();
    let first_line = {
        let observed = &tx.extension::<CompletionProbe>().observed;
        assert_eq!(observed.len(), 1);
        let (line, observed_company, observed_product, quantity, login) = observed[0].clone();
        assert_eq!(
            (observed_company, observed_product, quantity, login),
            (company, product, Decimal::from(7), request.clone())
        );
        assert_eq!(
            tx.query(
                "SELECT id FROM stock_move_line WHERE picking_id=$1 AND done_at IS NOT NULL",
                &[&picking]
            )
            .await
            .unwrap()[0]
                .get::<_, i64>(0),
            line
        );
        line
    };
    {
        let amount = Decimal::new(125, 2);
        let second = create_transfer(&mut tx, company, kind, src, dest, product, amount).await;
        let transfers = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("stock.picking")
            .unwrap()
            .browse([picking, second])
            .unwrap();
        transfers
            .call::<(), ()>(&mut tx, "button_validate", ())
            .await
            .unwrap();
        let observations = &tx.extension::<CompletionProbe>().observed;
        assert_eq!(
            observations.len(),
            1,
            "the prior receipt is skipped in a mixed validation"
        );
        assert_ne!(observations[0].0, first_line);
        assert_eq!(observations[0].3, amount);
    }
    tx.commit().await.unwrap();
    drop(tx);

    let line = admin.query_one(
        "SELECT id, quantity::text, done_at IS NOT NULL FROM stock_move_line WHERE picking_id=$1",
        &[&picking],
    ).await.unwrap();
    assert_eq!(line.get::<_, i64>(0), first_line);
    assert_eq!(
        line.get::<_, String>(1).parse::<Decimal>().unwrap(),
        Decimal::from(7)
    );
    assert!(line.get::<_, bool>(2));
    // Inspect actual effective database privileges, after ordinary application publication.
    let authority = admin.query_one(
        "SELECT has_table_privilege($1::name, 'public.stk_cost_control', 'INSERT'),
                has_any_column_privilege($1::name, 'public.stk_cost_control', 'UPDATE'),
                has_table_privilege($1::name, 'public.stk_cost_control', 'DELETE'),
                (SELECT count(*) FROM public.stk_cost_control WHERE company_id=$2 AND product_id=$3)",
        &[&request, &company, &product],
    ).await.unwrap();
    let actual = (
        authority.get::<_, bool>(0),
        authority.get::<_, bool>(1),
        authority.get::<_, bool>(2),
        authority.get::<_, i64>(3),
    );
    assert_eq!(
        actual,
        (false, false, false, 1),
        "the receipt's control is created only through bounded admission"
    );
    let mut setup = runtime.begin_caller(manager).await.unwrap();
    let left = create_transfer(
        &mut setup,
        company,
        kind,
        src,
        dest,
        extra_product,
        Decimal::ONE,
    )
    .await;
    let right = create_transfer(
        &mut setup,
        company,
        kind,
        src,
        dest,
        extra_product,
        Decimal::from(2),
    )
    .await;
    let sealed = create_transfer(&mut setup, company, kind, src, dest, product, Decimal::ONE).await;
    let other = create_transfer(
        &mut setup,
        company,
        kind,
        src,
        dest,
        extra_product,
        Decimal::ONE,
    )
    .await;
    setup.commit().await.unwrap();
    drop(setup);

    eprintln!("receipt-fixture phase=source-bounds");
    // Repeated source rows for one product also need a bound; the distinct-key cap cannot help.
    for limit in [1, 10000] {
        let mut configuration = installed.orm.begin(SUPERUSER_ID).await.unwrap();
        configuration
            .env()
            .records("ir.config_parameter")
            .unwrap()
            .call::<(String, Option<i64>), i64>(
                &mut configuration,
                "set_int",
                (
                    "valuation.max_admission_source_rows".to_owned(),
                    Some(limit),
                ),
            )
            .await
            .expect("receipt source-row admission has a declared configurable bound");
        configuration.commit().await.unwrap();
        drop(configuration);
        if limit == 1 {
            let mut limited = runtime.begin_caller(manager).await.unwrap();
            let error = limited
                .env()
                .with_company(company)
                .unwrap()
                .records("stock.picking")
                .unwrap()
                .browse([left, right])
                .unwrap()
                .call::<(), ()>(&mut limited, "button_validate", ())
                .await
                .expect_err("two source rows for the same product exceed the row bound");
            assert_eq!(error.kind(), ErrorKind::Validation);
            assert_eq!(
                error.message(),
                "Valuation receipt sources exceed valuation.max_admission_source_rows (1). Split the operation."
            );
            limited.commit().await.unwrap();
            drop(limited);
            let mut clerk = runtime.begin_caller(stock_user).await.unwrap();
            let denied = validate_batch(&mut clerk, company, &[left, right])
                .await
                .unwrap_err();
            assert_eq!(denied.kind(), ErrorKind::Access);
            assert_eq!(
                denied.message(),
                "Only stock managers may admit a valued stock receipt."
            );
            clerk.commit().await.unwrap();
            drop(clerk);

            let mut config = installed.orm.begin(SUPERUSER_ID).await.unwrap();
            let model = config
                .query("SELECT id FROM ir_model WHERE model='stock.move'", &[])
                .await
                .unwrap()[0]
                .get(0);
            let rule = create(
                &mut config,
                company,
                "ir.access",
                RawVals::new()
                    .set_cell("name", Cell::Str("Hide the second receipt movement".into()))
                    .set_cell("model_id", Cell::Id(model))
                    .set_cell("operation", Cell::Str("r".into()))
                    .set_cell(
                        "domain",
                        Cell::Str(format!("[('picking_id', '!=', {right})]").into()),
                    ),
            )
            .await;
            config.commit().await.unwrap();
            drop(config);
            policy.refresh_shared().await.unwrap();
            let mut restricted = runtime.begin_caller(manager).await.unwrap();
            let denied = validate_batch(&mut restricted, company, &[left, right])
                .await
                .unwrap_err();
            assert_eq!(denied.kind(), ErrorKind::Access, "{denied}");
            restricted.commit().await.unwrap();
            drop(restricted);
            let mut config = installed.orm.begin(SUPERUSER_ID).await.unwrap();
            config
                .env()
                .records("ir.access")
                .unwrap()
                .browse([rule])
                .unwrap()
                .unlink(&mut config)
                .await
                .unwrap();
            config.commit().await.unwrap();
            drop(config);
            policy.refresh_shared().await.unwrap();
            let unchanged = admin
                .query_one(
                    "SELECT (SELECT count(*) FROM public.stk_cost_control WHERE company_id=$1 AND product_id=$2),
                        (SELECT count(*) FROM public.stock_picking WHERE id=ANY($3) AND date_done IS NOT NULL)",
                    &[&company, &extra_product, &vec![left, right]],
                )
                .await
                .unwrap();
            assert_eq!(unchanged.get::<_, i64>(0), 0);
            assert_eq!(unchanged.get::<_, i64>(1), 0);
        }
    }

    eprintln!("receipt-fixture phase=key-bounds");
    // Two distinct valued keys must be refused at an operator-set limit of one.
    for limit in [1, 1000] {
        let mut configuration = installed.orm.begin(SUPERUSER_ID).await.unwrap();
        configuration
            .env()
            .records("ir.config_parameter")
            .unwrap()
            .call::<(String, Option<i64>), i64>(
                &mut configuration,
                "set_int",
                (
                    "valuation.max_admission_keys_per_unit".to_owned(),
                    Some(limit),
                ),
            )
            .await
            .unwrap();
        configuration.commit().await.unwrap();
        drop(configuration);
        if limit == 1 {
            let mut limited = runtime.begin_caller(manager).await.unwrap();
            let error = limited
                .env()
                .with_company(company)
                .unwrap()
                .records("stock.picking")
                .unwrap()
                .browse([other, sealed])
                .unwrap()
                .call::<(), ()>(&mut limited, "button_validate", ())
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation);
            assert_eq!(
                error.message(),
                "Too many valued products in one operation: 2 exceeds 1."
            );
            limited.commit().await.unwrap();
        }
    }

    eprintln!("receipt-fixture phase=mixed-unit");
    // An already successful picking holds physical locks until commit. The next picking
    // cannot extend its valuation prelude to a new product in that same transaction.
    let mut mixed = runtime.begin_caller(manager).await.unwrap();
    validate_picking(&mut mixed, company, sealed).await.unwrap();
    let late = mixed
        .savepoint(async |tx| validate_picking(tx, company, other).await)
        .await
        .unwrap_err();
    assert_eq!(late.sqlstate(), Some("40001"));
    assert!(late.message().contains("one validation"));
    assert!(mixed.is_poisoned());
    let refused = mixed.commit().await.unwrap_err();
    assert_eq!(refused.sqlstate(), Some("40001"));
    assert!(refused.retries_transaction());
    let classified = mixed.commit_pipelined(None).await.unwrap_err();
    assert!(matches!(classified, erp_orm::CommitError::RolledBack(_)));
    assert_eq!(classified.error().sqlstate(), Some("40001"));
    assert_eq!(classified.error().message(), late.message());
    drop(mixed);
    assert!(
        admin
            .query_one(
                "SELECT date_done IS NULL FROM stock_picking WHERE id=$1",
                &[&sealed]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );

    eprintln!("receipt-fixture phase=observer-rollback");
    // A caught late failure followed by a real commit leaves neither control nor stock behind.
    let mut failed = runtime.begin_caller(manager).await.unwrap();
    failed
        .execute(
            "CREATE TEMP TABLE stock_completion_probe (line_id bigint PRIMARY KEY) ON COMMIT DROP",
            &[],
        )
        .await
        .unwrap();
    failed.extension::<CompletionProbe>().enabled = true;
    failed.extension::<CompletionProbe>().refuse = true;
    assert_eq!(
        validate_picking(&mut failed, company, left)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Validation
    );
    failed.commit().await.unwrap();
    drop(failed);
    assert_eq!(
        admin
            .query_one(
                "SELECT count(*) FROM stk_cost_control WHERE company_id=$1 AND product_id=$2",
                &[&company, &extra_product]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );

    eprintln!("receipt-fixture phase=first-control-race");
    // Both snapshots precede the first control. The loser has not yet locked its picking.
    let mut first = runtime.begin_caller(manager).await.unwrap();
    let mut next = runtime.begin_caller(manager).await.unwrap();
    let waiting_pid: i32 = next.query("SELECT pg_backend_pid()", &[]).await.unwrap()[0].get(0);
    validate_picking(&mut first, company, left).await.unwrap();
    let contender = tokio::spawn(async move {
        let result = validate_picking(&mut next, company, right).await;
        next.rollback().await.unwrap();
        result
    });
    wait_for_database_lock(&admin, waiting_pid).await;
    admin
        .query_one(
            "SELECT id FROM stock_picking WHERE id=$1 FOR UPDATE NOWAIT",
            &[&right],
        )
        .await
        .unwrap();
    first.commit().await.unwrap();
    drop(first);
    assert_eq!(
        contender.await.unwrap().unwrap_err().sqlstate(),
        Some("40001")
    );
    let mut retry = runtime.begin_caller(manager).await.unwrap();
    validate_picking(&mut retry, company, right).await.unwrap();
    retry.commit().await.unwrap();
    drop(retry);
    let evidence=admin.query_one("SELECT (SELECT count(*) FROM stk_cost_control WHERE company_id=$1 AND product_id=$2),
        (SELECT sum(quantity) FROM stock_quant WHERE company_id=$1 AND product_id=$2 AND location_id=$3)",&[&company,&extra_product,&dest]).await.unwrap();
    assert_eq!(evidence.get::<_, i64>(0), 1);
    assert_eq!(evidence.get::<_, Decimal>(1), Decimal::from(3));

    eprintln!("receipt-fixture phase=stale-seal");
    // An exclusive coordinator fence precedes an RR writer with an old snapshot.
    let mut stale = runtime.begin_caller(manager).await.unwrap();
    let waiting_pid: i32 = stale.query("SELECT pg_backend_pid()", &[]).await.unwrap()[0].get(0);
    admin.batch_execute("BEGIN").await.unwrap();
    admin.query_one("SELECT pg_advisory_xact_lock(hashtext('erpforall.stock.valuation.control.v1'),hashtext($1::bigint::text || '/' || $2::bigint::text))",&[&company,&product]).await.unwrap();
    admin.execute("UPDATE stk_cost_control SET sealed=true, seal_token='00000000-0000-4000-8000-000000000001',sealed_until=clock_timestamp(),seal_revision=seal_revision+1 WHERE company_id=$1 AND product_id=$2",&[&company,&product]).await.unwrap();
    let waiter = tokio::spawn(async move {
        let result = stale
            .env()
            .with_company(company)
            .unwrap()
            .records("stock.picking")
            .unwrap()
            .browse([other, sealed])
            .unwrap()
            .call::<(), ()>(&mut stale, "button_validate", ())
            .await;
        stale.rollback().await.unwrap();
        result
    });
    wait_for_database_lock(&admin, waiting_pid).await;
    admin
        .query_one(
            "SELECT id FROM stock_picking WHERE id=$1 FOR UPDATE NOWAIT",
            &[&sealed],
        )
        .await
        .unwrap();
    assert!(admin.query_one("SELECT pg_try_advisory_xact_lock(hashtext('erpforall.stock.valuation.control.v1'),hashtext($1::bigint::text || '/' || $2::bigint::text))",&[&company,&extra_product]).await.unwrap().get::<_,bool>(0), "higher key already locked despite reversed input");
    admin.batch_execute("COMMIT").await.unwrap();
    assert_eq!(waiter.await.unwrap().unwrap_err().sqlstate(), Some("40001"));
    let mut fresh = runtime.begin_caller(manager).await.unwrap();
    assert_eq!(
        validate_picking(&mut fresh, company, sealed)
            .await
            .unwrap_err()
            .sqlstate(),
        Some("40001")
    );
    assert_eq!(
        fresh.commit().await.unwrap_err().kind(),
        ErrorKind::Concurrency
    );
    fresh.rollback().await.unwrap();
    drop(fresh);
    let evidence=admin.query_one("SELECT p.date_done IS NULL,c.sealed,c.seal_revision FROM stock_picking p JOIN stk_cost_control c ON c.company_id=p.company_id WHERE p.id=$1 AND c.product_id=$2",&[&sealed,&product]).await.unwrap();
    assert!(evidence.get::<_, bool>(0));
    assert!(evidence.get::<_, bool>(1));
    assert_eq!(evidence.get::<_, i32>(2), 1);

    eprintln!("receipt-fixture phase=timeout-retry");
    // Unlike NOWAIT, a positively identified admission deadline retries the native operation.
    admin.batch_execute("BEGIN").await.unwrap();
    admin.query_one("SELECT pg_advisory_xact_lock(hashtext('erpforall.stock.valuation.control.v1'),hashtext($1::bigint::text || '/' || $2::bigint::text))",&[&company,&extra_product]).await.unwrap();
    let mut attempts = 0;
    let result = runtime
        .run(manager, Context::new(), async |tx| {
            attempts += 1;
            tx.execute("SET LOCAL lock_timeout = '40ms'", &[]).await?;
            let result = tx
                .savepoint(async |tx| validate_picking(tx, company, other).await)
                .await;
            if attempts == 1 {
                assert_eq!(result.as_ref().unwrap_err().sqlstate(), Some("55P03"));
                assert!(result.as_ref().unwrap_err().retries_transaction());
                admin.batch_execute("COMMIT").await.unwrap();
                // Deliberately swallow the nested refusal. The transaction owner must find
                // the original 55P03 at commit and rerun the whole unit on a fresh connection checkout.
                return Ok(());
            }
            result
        })
        .await;
    assert!(
        result.is_ok(),
        "timed-out admission was not retried: {result:?}"
    );
    assert_eq!(attempts, 2);
    let isolated=admin.query_one("SELECT has_table_privilege($1::name,'public.stk_cost_control','SELECT'),
      has_sequence_privilege($1::name,pg_get_serial_sequence('public.stk_cost_control','id'),'USAGE,SELECT,UPDATE'),
      pg_has_role($1::name,$2::name,'SET')",&[&request,&definer]).await.unwrap();
    assert!(
        !isolated.get::<_, bool>(0),
        "control data must only be read through the fixed helper"
    );
    assert!(
        !isolated.get::<_, bool>(1),
        "ordinary publication left prior direct access to the control identity sequence"
    );
    assert!(
        !isolated.get::<_, bool>(2),
        "request login can become the control definer"
    );

    eprintln!("receipt-fixture phase=uninstall");
    // A neutral control alone must retain its protection, even without any valued source.
    let controls: i64 = admin
        .query_one("SELECT count(*) FROM public.stk_cost_control", &[])
        .await
        .unwrap()
        .get(0);
    let refusal = Box::pin(modules.button_immediate_uninstall(
        &modules.system_env(Context::new()),
        &[ModuleName::new("valuation").unwrap()],
    ))
    .await
    .unwrap_err();
    let erp_modules::Error::Orm(refusal) = refusal else {
        panic!("unexpected admission uninstall failure: {refusal:?}");
    };
    assert_eq!(refusal.kind(), ErrorKind::UserError);
    assert_eq!(
        refusal.message(),
        format!(
            "Stock valuation admission controls prevent uninstalling Stock Valuation Admission. ({controls})"
        )
    );
    let retained = admin
        .query_one(
            "SELECT state::text,
                (SELECT count(*) FROM public.stk_cost_control),
                has_function_privilege($1::name,
                    'public.stk_admit_receipt_control(bigint,bigint,integer)', 'EXECUTE'),
                has_table_privilege($1::name, 'public.stk_cost_control', 'INSERT,UPDATE,DELETE')
             FROM public.ir_module_module WHERE name='valuation'",
            &[&request],
        )
        .await
        .unwrap();
    assert_eq!(retained.get::<_, String>(0), "installed");
    assert_eq!(retained.get::<_, i64>(1), controls);
    assert!(retained.get::<_, bool>(2));
    assert!(!retained.get::<_, bool>(3));
}

async fn validate_batch(
    tx: &mut Transaction,
    company: i64,
    pickings: &[i64],
) -> erp_sdk::Result<()> {
    tx.env()
        .with_company(company)?
        .records("stock.picking")?
        .browse(pickings.iter().copied())?
        .call(tx, "button_validate", ())
        .await
}

async fn wait_for_database_lock(admin: &deadpool_postgres::Object, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            let waiting: bool = admin
                .query_one(
                    "SELECT EXISTS (SELECT FROM pg_locks WHERE pid=$1 AND NOT granted)",
                    &[&pid],
                )
                .await
                .unwrap()
                .get(0);
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("writer never reached the database lock");
}

async fn validate_picking(tx: &mut Transaction, company: i64, picking: i64) -> erp_sdk::Result<()> {
    validate_batch(tx, company, &[picking]).await
}

async fn create(tx: &mut Transaction, company: i64, model: &str, values: RawVals) -> i64 {
    tx.env()
        .with_company(company)
        .unwrap()
        .records(model)
        .unwrap()
        .create(tx, vec![values])
        .await
        .unwrap_or_else(|error| panic!("create {model}: {error}"))
        .ids()[0]
}

#[derive(Default)]
struct CompletionProbe {
    enabled: bool,
    refuse: bool,
    observed: Vec<(i64, i64, i64, Decimal, String)>,
}

type ProbeMethod = erp_orm::MethodFn<(), ()>;
fn probe_completion<'a>(
    tx: &'a mut Transaction,
    lines: &'a erp_orm::Records,
    (): (),
    sup: erp_orm::Super<(), ()>,
) -> erp_orm::BoxFuture<'a, erp_sdk::Result<()>> {
    Box::pin(async move {
        sup.call(tx, lines, ()).await?;
        if !tx.extension::<CompletionProbe>().enabled {
            return Ok(());
        }
        lines.check_access(tx, Operation::Read).await?;
        let rows=tx.query("SELECT l.id,l.company_id,l.product_id,l.quantity_product_uom,current_user::text
            FROM stock_move_line l JOIN stock_picking p ON p.id=l.picking_id
            WHERE l.id=ANY($1::int8[]) AND l.membership='done' AND l.done_at IS NOT NULL AND p.date_done IS NOT NULL
            ORDER BY l.id",&[&lines.ids()]).await?;
        assert_eq!(rows.len(), lines.len());
        tx.execute(
            "INSERT INTO pg_temp.stock_completion_probe SELECT unnest($1::int8[])",
            &[&lines.ids()],
        )
        .await?;
        if tx.extension::<CompletionProbe>().refuse {
            return Err(erp_sdk::Error::validation(
                "Receipt observer fixture declined the completed transfer.",
            ));
        }
        tx.extension::<CompletionProbe>().observed = rows
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)))
            .collect();
        Ok(())
    })
}
fn observer_entry() -> &'static (dyn std::any::Any + Send + Sync) {
    static F: ProbeMethod = probe_completion;
    &F
}
static OBSERVER: erp_orm::MethodImpl = erp_orm::MethodImpl {
    addon: "stock_account",
    model: "stock.move.line",
    method: "_after_stock_completion",
    kind: erp_orm::ImplKind::Override,
    sig_name: std::any::type_name::<ProbeMethod>,
    entry: observer_entry,
    file: file!(),
    line: line!(),
};

async fn create_transfer(
    tx: &mut Transaction,
    company: i64,
    kind: i64,
    src: i64,
    dest: i64,
    product: i64,
    quantity: Decimal,
) -> i64 {
    let picking = create(
        tx,
        company,
        "stock.picking",
        RawVals::new()
            .set_cell("company_id", Cell::Id(company))
            .set_cell("picking_type_id", Cell::Id(kind))
            .set_cell("location_id", Cell::Id(src))
            .set_cell("location_dest_id", Cell::Id(dest)),
    )
    .await;
    create(
        tx,
        company,
        "stock.move",
        RawVals::new()
            .set_cell("company_id", Cell::Id(company))
            .set_cell("picking_id", Cell::Id(picking))
            .set_cell("product_id", Cell::Id(product))
            .set_cell("location_id", Cell::Id(src))
            .set_cell("location_dest_id", Cell::Id(dest))
            .set_cell("product_uom_qty", Cell::Decimal(quantity)),
    )
    .await;
    picking
}
