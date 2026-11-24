// SPDX-License-Identifier: MIT OR Apache-2.0
//! Purchase settings module toggle panel items in company and global scopes.

use super::*;
use erp_addon_base::ir_config_setting::{describe, write_values};
use erp_sdk::views::wire::{ModuleToggleState, SettingsItem};

#[test]
fn purchase_settings_describe_module_toggles_and_states() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let system = purchase_module_system(&db);
        let init = Box::pin(system.initialize(InitOptions::new().with_skip_auto_install(true)))
            .await
            .unwrap();
        let env = init.orm.env(SUPERUSER_ID, Context::new());
        let installed = Box::pin(
            system.button_immediate_install(&env, &[ModuleName::new("purchase").unwrap()]),
        )
        .await
        .unwrap();
        assert!(installed.post_commit.is_clean());
        let orm = purchase_policy_orm(&db, &installed.orm, &[]).await;

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let system_group = xmlid(&mut tx, "base", "group_system").await;
        let manager_group = xmlid(&mut tx, "purchase", "group_purchase_manager").await;
        let admin = user(
            &mut tx,
            "purchase-settings-admin",
            &[system_group],
            &[company],
        )
        .await;
        let manager = user(
            &mut tx,
            "purchase-settings-manager",
            &[manager_group],
            &[company],
        )
        .await;
        tx.commit().await.unwrap();
        drop(tx);

        let company_scope = SettingsScope::Company {
            company_id: WireId::try_from(u64::try_from(company).unwrap()).unwrap(),
        };

        // 1. As administrator, both company and global scopes describe both module toggles as NotInstalled
        let mut tx = orm.begin(admin).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        for scope in [company_scope.clone(), SettingsScope::Global {}] {
            let desc = describe(&env, &mut tx, scope).await.unwrap();
            let toggle = |key: &str| {
                desc.items
                    .iter()
                    .find_map(|item| match item {
                        SettingsItem::Module {
                            key: k,
                            label,
                            app,
                            block,
                            sequence,
                            addon,
                            state,
                        } if k == key => Some((
                            label.as_str(),
                            app.as_str(),
                            block.as_str(),
                            *sequence,
                            addon.as_str(),
                            *state,
                        )),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("missing module toggle {key} in description"))
            };
            assert_eq!(
                toggle("purchase.module_three_way_match"),
                (
                    "Three-way bill matching",
                    "purchase",
                    "Invoicing",
                    200,
                    "purchase_three_way_match",
                    ModuleToggleState::NotInstalled
                )
            );
            assert_eq!(
                toggle("purchase.module_stock_dropshipping"),
                (
                    "Direct supplier delivery",
                    "purchase",
                    "Logistics",
                    200,
                    "stock_dropshipping",
                    ModuleToggleState::NotInstalled
                )
            );
        }

        // write_values on module toggles is rejected with Access error
        for key in [
            "purchase.module_three_way_match",
            "purchase.module_stock_dropshipping",
        ] {
            let error = write_values(
                &env,
                &mut tx,
                company_scope.clone(),
                "0".into(),
                vec![(key.into(), Value::Bool(true))],
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Access);
            assert_eq!(
                error.message(),
                format!("Setting {key} is unavailable or access is denied.")
            );
        }
        tx.rollback().await.unwrap();
        drop(tx);

        // 2. As non-admin manager, neither module toggle is described
        let mut tx = orm.begin(manager).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        for scope in [company_scope.clone(), SettingsScope::Global {}] {
            let desc = describe(&env, &mut tx, scope).await.unwrap();
            for key in [
                "purchase.module_three_way_match",
                "purchase.module_stock_dropshipping",
            ] {
                assert!(
                    !desc.items.iter().any(|item| match item {
                        SettingsItem::Module { key: k, .. } => k == key,
                        _ => false,
                    }),
                    "non-admin manager must not see {key}"
                );
            }
        }
        tx.rollback().await.unwrap();
        drop(tx);

        // 3. Install purchase_three_way_match; its state transitions to Installed while stock_dropshipping remains NotInstalled
        let env = installed.orm.env(SUPERUSER_ID, Context::new());
        let upgraded = Box::pin(system.button_immediate_install(
            &env,
            &[ModuleName::new("purchase_three_way_match").unwrap()],
        ))
        .await
        .unwrap();
        assert!(upgraded.post_commit.is_clean());
        let orm = purchase_policy_orm(&db, &upgraded.orm, &[]).await;

        let mut tx = orm.begin(admin).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let desc = describe(&env, &mut tx, company_scope).await.unwrap();
        let match_item = desc
            .items
            .iter()
            .find_map(|item| match item {
                SettingsItem::Module { key, state, .. }
                    if key == "purchase.module_three_way_match" =>
                {
                    Some(*state)
                }
                _ => None,
            })
            .expect("purchase.module_three_way_match");
        assert_eq!(match_item, ModuleToggleState::Installed);

        let dropship_item = desc
            .items
            .iter()
            .find_map(|item| match item {
                SettingsItem::Module { key, state, .. }
                    if key == "purchase.module_stock_dropshipping" =>
                {
                    Some(*state)
                }
                _ => None,
            })
            .expect("purchase.module_stock_dropshipping");
        assert_eq!(dropship_item, ModuleToggleState::NotInstalled);

        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}
