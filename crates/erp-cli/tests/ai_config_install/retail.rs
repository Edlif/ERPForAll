// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use erp_security::{resolver::Resolver, source::PolicySource};
pub(super) struct Retail {
    pub installed: Orm,
    pub orm: Orm,
    pub company: i64,
    pub users: Vec<UserId>,
}
pub(super) async fn install(db: &TestDb) -> Retail {
    let installed = install_modules(
        db,
        &["ai_base", "purchase", "sale", "stock"],
        erp_cli::native_addons::DESCRIPTORS,
    )
    .await;
    let mut setup = installed.begin(SUPERUSER_ID).await.unwrap();
    setup.install_audit_schema().await.unwrap();
    let company: i64 = setup
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let mut users = Vec::new();
    for (module, group) in [
        ("base", "group_system"),
        ("purchase", "group_purchase_user"),
        ("sales_team", "group_sale_salesman"),
        ("stock", "group_stock_user"),
        ("account", "group_account_readonly"),
    ] {
        let gid: i64 = setup
            .query(
                "SELECT res_id FROM ir_model_data WHERE module=$1 AND name=$2",
                &[&module, &group],
            )
            .await
            .unwrap()[0]
            .get(0);
        let id = setup
            .env()
            .records("res.users")
            .unwrap()
            .create(
                &mut setup,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str(module.into()))
                        .set_cell("login", Cell::Str(format!("catalog-{module}").into()))
                        .set_cell(
                            "password",
                            Cell::Str(format!("fixture-{module}-password").into()),
                        )
                        .set_cell("company_id", Cell::Id(company))
                        .set_commands(
                            "company_ids",
                            [Command::Set(vec![RecId::from_real(company).unwrap()])],
                        )
                        .set_commands(
                            "group_ids",
                            [Command::Set(vec![RecId::from_real(gid).unwrap()])],
                        ),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        if module != "base" {
            users.push(erp_orm::UserId::new(id).unwrap());
        }
    }
    setup.commit().await.unwrap();
    let source = PolicySource::build(Arc::clone(installed.registry()), db.pool().clone())
        .await
        .unwrap();
    let orm = Orm::builder(Arc::clone(installed.registry()), db.pool().clone())
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .resolver(Resolver::for_registry(installed.registry()).unwrap())
        .policy(Arc::new(source) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .build()
        .unwrap();
    Retail {
        installed,
        orm,
        company,
        users,
    }
}
