// SPDX-License-Identifier: MIT OR Apache-2.0
//! Purchase warning group gating, length validation, order/line computation, and Settings toggle.

use super::*;
use erp_addon_base::ir_config_setting::{describe, toggle_group, write_values};

#[test]
fn purchase_warnings_group_gating_validation_computation_and_settings_toggle() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let system = std::sync::Arc::new(purchase_module_system(&db));
        erp_cli::module::bind_settings_catalogue(std::sync::Arc::clone(&system)).unwrap();
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

        let mut tx = installed.orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let system_group = xmlid(&mut tx, "base", "group_system").await;
        let buyer_group = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let manager_group = xmlid(&mut tx, "purchase", "group_purchase_manager").await;

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
        let product_admin_group = xmlid(&mut tx, "product", "group_product_manager").await;
        let product_admin = user(
            &mut tx,
            "po-warn-product-admin",
            &[product_admin_group, manager_group],
            &[company],
        )
        .await;

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

        // The generic value write handles declared values only; a feature toggle is switched through
        // `toggle_group` (step 3), so this write answers like an unavailable setting.
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

        // 3. Enable purchase.group_warning_purchase through the Settings switch (`toggle_group`): a purchase
        //    manager (not a Settings administrator) is refused, the administrator switches it on for
        //    every internal user.
        let mut tx = orm.begin(manager).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let toggle_refusal = toggle_group(&env, &mut tx, "purchase.group_warning_purchase", true)
            .await
            .unwrap_err();
        assert_eq!(toggle_refusal.kind(), ErrorKind::Access);
        assert_eq!(
            toggle_refusal.message(),
            "Only Settings administrators may turn a feature on or off for every user."
        );
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(admin).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        assert_eq!(
            toggle_group(&env, &mut tx, "purchase.group_warning_purchase", true)
                .await
                .unwrap(),
            erp_sdk::views::wire::GroupToggleState::Enabled
        );
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
            "Purchase order notice cannot exceed 4000 characters (base.warning_message_max_chars)."
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
            "Purchase order line notice cannot exceed 4000 characters (base.warning_message_max_chars)."
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

        // Who writes the messages: a purchase manager may, a plain buyer may not.
        let mut tx = orm.begin(manager).await.unwrap();
        let manager_env = tx.env().with_company(company).unwrap();
        let manager_partner = manager_env
            .records("res.partner")
            .unwrap()
            .browse([quiet_vendor_id])
            .unwrap();
        write(
            &mut tx,
            &manager_partner,
            RawVals::new().set_cell("purchase_warn_msg", text("Manager note")),
        )
        .await
        .unwrap();
        assert_eq!(
            manager_partner
                .cell(&mut tx, "purchase_warn_msg")
                .await
                .unwrap(),
            text("Manager note")
        );
        // The product notice is written by the Product Administrator, not by the purchase manager.
        let denied = write(
            &mut tx,
            &manager_env
                .records("product.template")
                .unwrap()
                .browse([plain_tmpl_id])
                .unwrap(),
            RawVals::new().set_cell("purchase_line_warn_msg", text("Manager product note")),
        )
        .await;
        refused(denied, ErrorKind::Access, &[]);
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(product_admin).await.unwrap();
        let product_env = tx.env().with_company(company).unwrap();
        let product_tmpl = product_env
            .records("product.template")
            .unwrap()
            .browse([plain_tmpl_id])
            .unwrap();
        write(
            &mut tx,
            &product_tmpl,
            RawVals::new().set_cell("purchase_line_warn_msg", text("Product admin note")),
        )
        .await
        .unwrap();
        assert_eq!(
            product_tmpl
                .cell(&mut tx, "purchase_line_warn_msg")
                .await
                .unwrap(),
            text("Product admin note")
        );
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(buyer).await.unwrap();
        let buyer_env = tx.env().with_company(company).unwrap();
        let denied = write(
            &mut tx,
            &buyer_env
                .records("res.partner")
                .unwrap()
                .browse([quiet_vendor_id])
                .unwrap(),
            RawVals::new().set_cell("purchase_warn_msg", text("Buyer note")),
        )
        .await;
        refused(denied, ErrorKind::Access, &[]);
        let denied = write(
            &mut tx,
            &buyer_env
                .records("product.template")
                .unwrap()
                .browse([plain_tmpl_id])
                .unwrap(),
            RawVals::new().set_cell("purchase_line_warn_msg", text("Buyer product note")),
        )
        .await;
        refused(denied, ErrorKind::Access, &[]);
        tx.rollback().await.unwrap();
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

async fn create_one(tx: &mut Transaction, company: i64, model: &str, vals: RawVals) -> i64 {
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

/// One record of `model` as seen by user `uid` working in `companies`.
fn record_as(tx: &Transaction, uid: UserId, companies: &[i64], model: &str, id: i64) -> Records {
    tx.env()
        .with_user(uid)
        .with_context([(
            "allowed_company_ids",
            Value::list(companies.iter().map(|c| Value::Int(*c))),
        )])
        .records(model)
        .unwrap()
        .browse([id])
        .unwrap()
}

/// Which notices an order shows: a contact without a note of its own shows its parent's, a note of
/// spaces shows nothing, a product the caller's own rights hide adds nothing even though the
/// computation is elevated, two callers in one transaction each get their own text, and an edit of
/// a note shows when the order is read again.
#[test]
fn purchase_warnings_follow_the_callers_own_rights_and_current_notes() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let currency: i64 = tx
            .query(
                "SELECT currency_id FROM res_company WHERE id=$1",
                &[&company],
            )
            .await
            .unwrap()[0]
            .get(0);
        let other_company = create_one(
            &mut tx,
            company,
            "res.company",
            RawVals::new()
                .set_cell("name", text("Notice Other Company"))
                .set_cell("currency_id", Cell::Id(currency)),
        )
        .await;
        let user_group = xmlid(&mut tx, "base", "group_user").await;
        let warning_group = xmlid(&mut tx, "purchase", "group_warning_purchase").await;
        let buyer_group = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let manager_group = xmlid(&mut tx, "purchase", "group_purchase_manager").await;
        let system_group = xmlid(&mut tx, "base", "group_system").await;
        user(&mut tx, "notice-admin", &[system_group], &[company]).await;
        let wide_buyer = user(
            &mut tx,
            "notice-wide",
            &[buyer_group],
            &[company, other_company],
        )
        .await;
        let narrow_buyer = user(&mut tx, "notice-narrow", &[buyer_group], &[company]).await;
        let manager = user(&mut tx, "notice-manager", &[manager_group], &[company]).await;

        let parent = create_one(
            &mut tx,
            company,
            "res.partner",
            RawVals::new()
                .set_cell("name", text("Parent Holdings"))
                .set_cell("is_company", Cell::Bool(true))
                .set_cell("purchase_warn_msg", text("Parent requires PO reference")),
        )
        .await;
        let child = create_one(
            &mut tx,
            company,
            "res.partner",
            RawVals::new()
                .set_cell("name", text("Silent Branch"))
                .set_cell("parent_id", Cell::Id(parent)),
        )
        .await;
        let blank = create_one(
            &mut tx,
            company,
            "res.partner",
            RawVals::new()
                .set_cell("name", text("Blank Note Vendor"))
                .set_cell("purchase_warn_msg", text("   ")),
        )
        .await;
        let mut products = Vec::new();
        for (name, note) in [
            ("Visible Valve", "Visible note"),
            ("Guarded Valve", "Guarded note"),
        ] {
            let template = create_one(
                &mut tx,
                company,
                "product.template",
                RawVals::new()
                    .set_cell("name", text(name))
                    .set_cell("purchase_line_warn_msg", text(note)),
            )
            .await;
            let product: i64 = tx
                .query(
                    "SELECT id::int8 FROM product_product WHERE product_tmpl_id = $1",
                    &[&template],
                )
                .await
                .unwrap()[0]
                .get(0);
            products.push((template, product));
        }
        let [(_, visible), (guarded_template, guarded)] = products[..] else {
            unreachable!()
        };
        let notice_order = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(child))
                .set_cell("company_id", Cell::Id(company)),
        )
        .await
        .ids()[0];
        let blank_order = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(blank))
                .set_cell("company_id", Cell::Id(company)),
        )
        .await
        .ids()[0];
        // A vendor of the other company: the caller's own partner rule hides it from a caller
        // limited to the first company (moved after the order exists, so no company check refuses it).
        let foreign_vendor = create_one(
            &mut tx,
            company,
            "res.partner",
            RawVals::new()
                .set_cell("name", text("Foreign Vendor"))
                .set_cell("purchase_warn_msg", text("Foreign vendor note")),
        )
        .await;
        let foreign_order = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(foreign_vendor))
                .set_cell("company_id", Cell::Id(company)),
        )
        .await
        .ids()[0];
        tx.execute(
            "UPDATE res_partner SET company_id = $1 WHERE id = $2",
            &[&other_company, &foreign_vendor],
        )
        .await
        .unwrap();
        for product in [visible, guarded] {
            create_one(
                &mut tx,
                company,
                "purchase.order.line",
                RawVals::new()
                    .set_cell("order_id", Cell::Id(notice_order))
                    .set_cell("product_id", Cell::Id(product))
                    .set_cell("product_qty", Cell::Decimal(Decimal::from(1))),
            )
            .await;
        }
        // a product without attributes or a code displays under its name
        let (visible_name, guarded_name) = ("Visible Valve", "Guarded Valve");
        // The company rule of the product hides it from callers outside the other company; the
        // product joins that company after the order lines exist, so no company check refuses them.
        tx.execute(
            "UPDATE product_template SET company_id = $1 WHERE id = $2",
            &[&other_company, &guarded_template],
        )
        .await
        .unwrap();
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
        let orm = purchase_policy_orm(&db, &orm, &[]).await;

        // The system principal may switch users without an impersonation capability.
        let mut tx = orm.begin(erp_orm::PrincipalHint::system()).await.unwrap();
        // a first write resolves the principal of the transaction before any switched read
        let parent_record = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("res.partner")
            .unwrap()
            .browse([parent])
            .unwrap();
        write(
            &mut tx,
            &parent_record,
            RawVals::new().set_cell("purchase_warn_msg", text("Parent requires PO reference")),
        )
        .await
        .unwrap();
        let parent_line = "Parent Holdings - Parent requires PO reference";
        let narrow_text = format!("{parent_line}\n{visible_name} - Visible note");
        let wide_text = format!("{narrow_text}\n{guarded_name} - Guarded note");

        // Two callers, one transaction: each one's text follows that caller's own rights.
        let narrow_order = record_as(
            &tx,
            narrow_buyer,
            &[company],
            "purchase.order",
            notice_order,
        );
        let wide_order = record_as(
            &tx,
            wide_buyer,
            &[company, other_company],
            "purchase.order",
            notice_order,
        );
        assert_eq!(
            narrow_order
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            text(&narrow_text)
        );
        assert_eq!(
            wide_order
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            text(&wide_text)
        );
        assert_eq!(
            narrow_order
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            text(&narrow_text)
        );
        // The line of the hidden product shows nothing to the narrow caller, its note to the wide one.
        let guarded_line: i64 = tx
            .query(
                "SELECT id::int8 FROM purchase_order_line WHERE product_id = $1",
                &[&guarded],
            )
            .await
            .unwrap()[0]
            .get(0);
        let narrow_line = record_as(
            &tx,
            narrow_buyer,
            &[company],
            "purchase.order.line",
            guarded_line,
        );
        let wide_line = record_as(
            &tx,
            wide_buyer,
            &[company, other_company],
            "purchase.order.line",
            guarded_line,
        );
        assert_eq!(
            narrow_line
                .cell(&mut tx, "purchase_line_warn_msg")
                .await
                .unwrap(),
            Cell::Null
        );
        assert_eq!(
            wide_line
                .cell(&mut tx, "purchase_line_warn_msg")
                .await
                .unwrap(),
            text("Guarded note")
        );
        // The vendor of the other company adds no notice for the narrow caller, its note for the wide one.
        let narrow_foreign = record_as(
            &tx,
            narrow_buyer,
            &[company],
            "purchase.order",
            foreign_order,
        );
        let wide_foreign = record_as(
            &tx,
            wide_buyer,
            &[company, other_company],
            "purchase.order",
            foreign_order,
        );
        assert_eq!(
            narrow_foreign
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            Cell::Null
        );
        assert_eq!(
            wide_foreign
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            text("Foreign Vendor - Foreign vendor note")
        );
        // A note of spaces only gives no banner.
        let blank_as_narrow =
            record_as(&tx, narrow_buyer, &[company], "purchase.order", blank_order);
        assert_eq!(
            blank_as_narrow
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            Cell::Null
        );

        // An edit of a note shows when the order is read again in the same transaction.
        let manager_order = record_as(&tx, manager, &[company], "purchase.order", notice_order);
        assert_eq!(
            manager_order
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            text(&narrow_text)
        );
        let manager_parent = record_as(&tx, manager, &[company], "res.partner", parent);
        write(
            &mut tx,
            &manager_parent,
            RawVals::new().set_cell("purchase_warn_msg", text("Changed reference rule")),
        )
        .await
        .unwrap();
        assert_eq!(
            manager_order
                .cell(&mut tx, "purchase_warning_text")
                .await
                .unwrap(),
            text(&format!(
                "Parent Holdings - Changed reference rule\n{visible_name} - Visible note"
            ))
        );
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

/// The warning notice length bound is the system parameter `base.warning_message_max_chars`.
#[test]
fn purchase_notice_length_follows_the_system_parameter() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let vendor = create_one(
            &mut tx,
            company,
            "res.partner",
            RawVals::new().set_cell("name", text("Short Notice Vendor")),
        )
        .await;
        let params = tx.env().records("ir.config_parameter").unwrap();
        let set = async |tx: &mut Transaction, value: i64| {
            let params = params.clone();
            // a refused value leaves nothing pending: each try runs in its own savepoint
            tx.savepoint(async move |tx| {
                params
                    .call::<(String, Option<i64>), i64>(
                        tx,
                        "set_int",
                        ("base.warning_message_max_chars".to_owned(), Some(value)),
                    )
                    .await
            })
            .await
        };
        for outside in [0, 20_001] {
            assert_eq!(
                set(&mut tx, outside).await.unwrap_err().kind(),
                ErrorKind::Validation,
                "{outside}"
            );
        }
        set(&mut tx, 12).await.unwrap();
        let partner = tx
            .env()
            .records("res.partner")
            .unwrap()
            .browse([vendor])
            .unwrap();
        write(
            &mut tx,
            &partner,
            RawVals::new().set_cell("purchase_warn_msg", text("123456789012")),
        )
        .await
        .unwrap();
        let error = write(
            &mut tx,
            &partner,
            RawVals::new().set_cell("purchase_warn_msg", text("1234567890123")),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(
            error.message(),
            "Purchase order notice cannot exceed 12 characters (base.warning_message_max_chars)."
        );
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

/// Removing Purchase while its notice switch is on takes the feature group and its columns away and
/// leaves the internal-user group intact.
#[test]
fn purchase_uninstall_with_the_notice_switch_on() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let system = purchase_module_system(&db);
        let init = Box::pin(system.initialize(InitOptions::new().with_skip_auto_install(true)))
            .await
            .unwrap();
        let env = init.orm.env(SUPERUSER_ID, Context::new());
        let purchase = ModuleName::new("purchase").unwrap();
        let installed =
            Box::pin(system.button_immediate_install(&env, std::slice::from_ref(&purchase)))
                .await
                .unwrap();
        assert!(
            installed.post_commit.is_clean(),
            "{:?}",
            installed.post_commit
        );

        let mut tx = installed.orm.begin(SUPERUSER_ID).await.unwrap();
        let user_group = xmlid(&mut tx, "base", "group_user").await;
        let warning_group = xmlid(&mut tx, "purchase", "group_warning_purchase").await;
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

        let caller = system.system_env(Context::new());
        let removed =
            Box::pin(system.button_immediate_uninstall(&caller, std::slice::from_ref(&purchase)))
                .await
                .unwrap();
        assert!(removed.uninstalled.contains(&purchase));
        let mut tx = removed.orm.begin(SUPERUSER_ID).await.unwrap();
        assert_eq!(
            strings(
                &mut tx,
                "SELECT count(*)::text FROM ir_model_data \
                 WHERE module = 'purchase' AND name = 'group_warning_purchase'"
            )
            .await,
            ["0"],
            "the feature group record is gone"
        );
        // the internal-user group and its other implications stay
        let internal = tx
            .query(
                "SELECT count(*)::int8 FROM res_groups WHERE id = $1",
                &[&user_group],
            )
            .await
            .unwrap()[0]
            .get::<_, i64>(0);
        assert_eq!(internal, 1);
        let stale = tx
            .query(
                "SELECT count(*)::int8 FROM res_groups_implied_rel r \
                 WHERE NOT EXISTS (SELECT 1 FROM res_groups g WHERE g.id = r.hid)",
                &[],
            )
            .await
            .unwrap()[0]
            .get::<_, i64>(0);
        assert_eq!(stale, 0, "no implication points at a removed group");
        assert_eq!(
            strings(
                &mut tx,
                "SELECT count(*)::text FROM information_schema.columns \
                 WHERE table_name = 'res_partner' AND column_name = 'purchase_warn_msg'"
            )
            .await,
            ["0"],
            "the notice column is gone"
        );
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}
