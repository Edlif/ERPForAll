// SPDX-License-Identifier: MIT OR Apache-2.0
//! Purchase warning group gating, length validation, order/line computation, and Settings toggle.

use super::*;
use erp_addon_base::ir_config_setting::{describe, write_values};

#[test]
fn purchase_warnings_group_gating_validation_computation_and_settings_toggle() {
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
        assert!(
            installed.post_commit.is_clean(),
            "{:?}",
            installed.post_commit
        );
        let orm = purchase_policy_orm(&db, &installed.orm, &[]).await;

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let system_group = xmlid(&mut tx, "base", "group_system").await;
        let user_group = xmlid(&mut tx, "base", "group_user").await;
        let buyer_group = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let manager_group = xmlid(&mut tx, "purchase", "group_purchase_manager").await;
        let warning_group = xmlid(&mut tx, "purchase", "group_warning_purchase").await;

        // Verify partner and product view extensions installed cleanly.
        for view_name in [
            "res_partner_form_purchase_warnings",
            "product_template_form_purchase_warnings",
        ] {
            let found = strings(
                &mut tx,
                &format!(
                    "SELECT name FROM ir_model_data \
                     WHERE module = 'purchase' AND model = 'ir.ui.view' AND name = '{view_name}'"
                ),
            )
            .await;
            assert_eq!(found, [view_name]);
        }

        let admin = user(&mut tx, "po-warn-admin", &[system_group], &[company]).await;
        let manager = user(&mut tx, "po-warn-manager", &[manager_group], &[company]).await;
        let buyer = user(&mut tx, "po-warn-buyer", &[buyer_group], &[company]).await;

        let setup_env = tx.env().with_company(company).unwrap();
        let partners = setup_env.records("res.partner").unwrap();
        let parent_id = partners
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Parent Holdings"))
                        .set_cell("is_company", Cell::Bool(true)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let vendor_id = partners
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Regional Vendor"))
                        .set_cell("parent_id", Cell::Id(parent_id)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let quiet_vendor_id = partners
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", text("Quiet Vendor"))],
            )
            .await
            .unwrap()
            .ids()[0];

        let templates = setup_env.records("product.template").unwrap();
        let warn_tmpl_id = templates
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", text("Precision Valve"))],
            )
            .await
            .unwrap()
            .ids()[0];
        let plain_tmpl_id = templates
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", text("Standard Bolt"))],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.flush().await.unwrap();

        let warn_prod_id: i64 = tx
            .query(
                "SELECT id::int8 FROM product_product WHERE product_tmpl_id = $1 LIMIT 1",
                &[&warn_tmpl_id],
            )
            .await
            .unwrap()[0]
            .get(0);
        let plain_prod_id: i64 = tx
            .query(
                "SELECT id::int8 FROM product_product WHERE product_tmpl_id = $1 LIMIT 1",
                &[&plain_tmpl_id],
            )
            .await
            .unwrap()[0]
            .get(0);
        let warn_prod_display = match setup_env
            .records("product.product")
            .unwrap()
            .browse([warn_prod_id])
            .unwrap()
            .cell(&mut tx, "display_name")
            .await
            .unwrap()
        {
            Cell::Str(s) => s.to_string(),
            other => panic!("expected display_name string, got {other:?}"),
        };

        let po_warn = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(vendor_id))
                .set_cell("company_id", Cell::Id(company)),
        )
        .await;
        let po_warn_id = po_warn.ids()[0];
        let lines = setup_env.records("purchase.order.line").unwrap();
        let line_warn_1 = lines
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("order_id", Cell::Id(po_warn_id))
                        .set_cell("product_id", Cell::Id(warn_prod_id))
                        .set_cell("product_qty", Cell::Decimal(Decimal::from(2))),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let _line_warn_dup = lines
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("order_id", Cell::Id(po_warn_id))
                        .set_cell("product_id", Cell::Id(warn_prod_id))
                        .set_cell("product_qty", Cell::Decimal(Decimal::from(1))),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let line_plain = lines
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("order_id", Cell::Id(po_warn_id))
                        .set_cell("product_id", Cell::Id(plain_prod_id))
                        .set_cell("product_qty", Cell::Decimal(Decimal::from(5))),
                ],
            )
            .await
            .unwrap()
            .ids()[0];

        let po_plain = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(quiet_vendor_id))
                .set_cell("company_id", Cell::Id(company)),
        )
        .await;
        let po_plain_id = po_plain.ids()[0];
        lines
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("order_id", Cell::Id(po_plain_id))
                        .set_cell("product_id", Cell::Id(plain_prod_id))
                        .set_cell("product_qty", Cell::Decimal(Decimal::from(1))),
                ],
            )
            .await
            .unwrap();

        // Populate warning texts as superuser while the feature group is still disabled.
        setup_env
            .records("res.partner")
            .unwrap()
            .browse([parent_id])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("purchase_warn_msg", text("Parent requires PO reference")),
            )
            .await
            .unwrap();
        setup_env
            .records("res.partner")
            .unwrap()
            .browse([vendor_id])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("purchase_warn_msg", text("Confirm lead time")),
            )
            .await
            .unwrap();
        setup_env
            .records("product.template")
            .unwrap()
            .browse([warn_tmpl_id])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("purchase_line_warn_msg", text("Check batch certificate")),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        let company_scope = SettingsScope::Company {
            company_id: WireId::try_from(u64::try_from(company).unwrap()).unwrap(),
        };

        // 1. Settings describe: admin sees purchase.group_warning_purchase as Disabled; manager does not see it.
        let mut tx = orm.begin(admin).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let desc = describe(&env, &mut tx, company_scope.clone())
            .await
            .unwrap();
        let desc_json = erp_sdk::serde_json::to_value(&desc).unwrap();
        let group_item = desc_json["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == "purchase.group_warning_purchase")
            .expect("purchase.group_warning_purchase item");
        assert_eq!(group_item["kind"], "group");
        assert_eq!(group_item["label"], "Warnings");
        assert_eq!(group_item["block"], "Orders");
        assert_eq!(group_item["group"], "purchase.group_warning_purchase");
        assert_eq!(group_item["state"], "disabled");
        assert_eq!(group_item["group_id"], warning_group);
        assert_eq!(group_item["internal_group_id"], user_group);

        let err = write_values(
            &env,
            &mut tx,
            company_scope.clone(),
            "0".into(),
            vec![("purchase.group_warning_purchase".into(), Value::Bool(true))],
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Access);
        assert_eq!(
            err.message(),
            "Setting purchase.group_warning_purchase is unavailable or access is denied."
        );
        tx.rollback().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(manager).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let mgr_desc = describe(&env, &mut tx, company_scope.clone())
            .await
            .unwrap();
        let mgr_json = erp_sdk::serde_json::to_value(&mgr_desc).unwrap();
        assert!(
            !mgr_json["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["key"] == "purchase.group_warning_purchase"),
            "non-admin manager must not see purchase.group_warning_purchase"
        );
        tx.rollback().await.unwrap();
        drop(tx);

        // 2. Outside purchase.group_warning_purchase: direct field read/write is refused (even for
        //    admin who has model write rights) and computed warning fields read empty.
        let mut tx = orm.begin(admin).await.unwrap();
        let admin_env = tx.env().with_company(company).unwrap();
        let admin_partner_rec = admin_env
            .records("res.partner")
            .unwrap()
            .browse([vendor_id])
            .unwrap();
        let write_partner_err = write(
            &mut tx,
            &admin_partner_rec,
            RawVals::new().set_cell("purchase_warn_msg", text("Blocked write")),
        )
        .await;
        refused(write_partner_err, ErrorKind::Access, &["purchase_warn_msg"]);

        let admin_tmpl_rec = admin_env
            .records("product.template")
            .unwrap()
            .browse([warn_tmpl_id])
            .unwrap();
        let write_tmpl_err = write(
            &mut tx,
            &admin_tmpl_rec,
            RawVals::new().set_cell("purchase_line_warn_msg", text("Blocked write")),
        )
        .await;
        refused(
            write_tmpl_err,
            ErrorKind::Access,
            &["purchase_line_warn_msg"],
        );
        tx.rollback().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(buyer).await.unwrap();
        let buyer_env = tx.env().with_company(company).unwrap();
        let partner_rec = buyer_env
            .records("res.partner")
            .unwrap()
            .browse([vendor_id])
            .unwrap();
        let read_partner_err = tx
            .savepoint({
                let rec = partner_rec.clone();
                async move |tx| rec.cell(tx, "purchase_warn_msg").await
            })
            .await;
        refused(read_partner_err, ErrorKind::Access, &["purchase_warn_msg"]);

        let tmpl_rec = buyer_env
            .records("product.template")
            .unwrap()
            .browse([warn_tmpl_id])
            .unwrap();
        let read_tmpl_err = tx
            .savepoint({
                let rec = tmpl_rec.clone();
                async move |tx| rec.cell(tx, "purchase_line_warn_msg").await
            })
            .await;
        refused(
            read_tmpl_err,
            ErrorKind::Access,
            &["purchase_line_warn_msg"],
        );

        let po_rec = buyer_env
            .records("purchase.order")
            .unwrap()
            .browse([po_warn_id])
            .unwrap();
        assert_eq!(
            po_rec.cell(&mut tx, "purchase_warning_text").await.unwrap(),
            Cell::Null
        );
        let line_rec = buyer_env
            .records("purchase.order.line")
            .unwrap()
            .browse([line_warn_1])
            .unwrap();
        assert_eq!(
            line_rec
                .cell(&mut tx, "purchase_line_warn_msg")
                .await
                .unwrap(),
            Cell::Null
        );
        tx.rollback().await.unwrap();
        drop(tx);

        // 3. Enable purchase.group_warning_purchase via base.group_user.implied_ids (Settings toggle path).
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        tx.env()
            .records("res.groups")
            .unwrap()
            .browse([user_group])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_commands(
                    "implied_ids",
                    [Command::Link(RecId::from_real(warning_group).unwrap())],
                ),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        let orm = purchase_policy_orm(&db, &installed.orm, &[]).await;

        // Settings describe now reports Enabled.
        let mut tx = orm.begin(admin).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let desc = describe(&env, &mut tx, company_scope).await.unwrap();
        let desc_json = erp_sdk::serde_json::to_value(&desc).unwrap();
        let group_item = desc_json["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == "purchase.group_warning_purchase")
            .expect("purchase.group_warning_purchase item");
        assert_eq!(group_item["state"], "enabled");

        // Length validation: 4000 chars allowed, 4001 chars refused with Validation error.
        let max_ok = "a".repeat(4_000);
        let too_long = "a".repeat(4_001);
        let admin_partner = env
            .records("res.partner")
            .unwrap()
            .browse([quiet_vendor_id])
            .unwrap();
        write(
            &mut tx,
            &admin_partner,
            RawVals::new().set_cell("purchase_warn_msg", text(&max_ok)),
        )
        .await
        .unwrap();
        let partner_over = write(
            &mut tx,
            &admin_partner,
            RawVals::new().set_cell("purchase_warn_msg", text(&too_long)),
        )
        .await
        .unwrap_err();
        assert_eq!(partner_over.kind(), ErrorKind::Validation);
        assert_eq!(
            partner_over.message(),
            "Purchase order warning cannot exceed 4000 characters."
        );
        // Reset quiet_vendor warning to empty.
        write(
            &mut tx,
            &admin_partner,
            RawVals::new().set_cell("purchase_warn_msg", Cell::Null),
        )
        .await
        .unwrap();

        let admin_tmpl = env
            .records("product.template")
            .unwrap()
            .browse([plain_tmpl_id])
            .unwrap();
        write(
            &mut tx,
            &admin_tmpl,
            RawVals::new().set_cell("purchase_line_warn_msg", text(&max_ok)),
        )
        .await
        .unwrap();
        let tmpl_over = write(
            &mut tx,
            &admin_tmpl,
            RawVals::new().set_cell("purchase_line_warn_msg", text(&too_long)),
        )
        .await
        .unwrap_err();
        assert_eq!(tmpl_over.kind(), ErrorKind::Validation);
        assert_eq!(
            tmpl_over.message(),
            "Purchase order line warning cannot exceed 4000 characters."
        );
        write(
            &mut tx,
            &admin_tmpl,
            RawVals::new().set_cell("purchase_line_warn_msg", Cell::Null),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        // 4. Inside purchase.group_warning_purchase: buyer reads partner, product, line, and order warnings.
        let mut tx = orm.begin(buyer).await.unwrap();
        let buyer_env = tx.env().with_company(company).unwrap();
        assert_eq!(
            buyer_env
                .records("res.partner")
                .unwrap()
                .browse([vendor_id])
                .unwrap()
                .cell(&mut tx, "purchase_warn_msg")
                .await
                .unwrap(),
            text("Confirm lead time")
        );
        assert_eq!(
            buyer_env
                .records("product.template")
                .unwrap()
                .browse([warn_tmpl_id])
                .unwrap()
                .cell(&mut tx, "purchase_line_warn_msg")
                .await
                .unwrap(),
            text("Check batch certificate")
        );
        assert_eq!(
            buyer_env
                .records("purchase.order.line")
                .unwrap()
                .browse([line_warn_1])
                .unwrap()
                .cell(&mut tx, "purchase_line_warn_msg")
                .await
                .unwrap(),
            text("Check batch certificate")
        );
        assert_eq!(
            buyer_env
                .records("purchase.order.line")
                .unwrap()
                .browse([line_plain])
                .unwrap()
                .cell(&mut tx, "purchase_line_warn_msg")
                .await
                .unwrap(),
            Cell::Null
        );
        let expected_order_warn = format!(
            "Regional Vendor - Confirm lead time\n\
             Parent Holdings - Parent requires PO reference\n\
             {warn_prod_display} - Check batch certificate"
        );
        assert_eq!(
            buyer_env
                .records("purchase.order")
                .unwrap()
                .browse([po_warn_id])
                .unwrap()
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            text(&expected_order_warn)
        );
        assert_eq!(
            buyer_env
                .records("purchase.order")
                .unwrap()
                .browse([po_plain_id])
                .unwrap()
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            Cell::Null
        );
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}
