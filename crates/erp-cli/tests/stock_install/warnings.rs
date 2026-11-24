// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stock transfer warning group gating, length validation, picking computation, and Settings toggle.

use super::*;
use erp_addon_base::ir_config_setting::{describe, toggle_group};
use erp_orm::UserId;
use erp_sdk::views::wire::{SettingsScope, WireId};

fn company_scope(id: i64) -> SettingsScope {
    SettingsScope::Company {
        company_id: WireId::try_from(u64::try_from(id).unwrap()).unwrap(),
    }
}

#[test]
fn stock_warnings_group_gating_validation_computation_and_settings_toggle() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let installed = install(&db).await;
        let actors = super::security::roles(&installed).await;
        let company = super::security::xid(&installed, "base", "main_company").await;
        let warning_group = super::security::xid(&installed, "stock", "group_warning_stock").await;

        let admin = UserId::new(actors[0]).unwrap();
        let stock_user = UserId::new(actors[2]).unwrap();
        let stock_manager = UserId::new(actors[3]).unwrap();

        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        // Verify partner view extension installed and group_warning_stock has no default implication.
        let view_rows = tx
            .query(
                "SELECT name FROM ir_model_data \
                 WHERE module = 'stock' AND model = 'ir.ui.view' AND name = 'res_partner_form_stock_warnings'",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(view_rows.len(), 1);
        let implied_rows = tx
            .query(
                "SELECT count(*)::int8 FROM res_groups_implied_rel WHERE hid = $1",
                &[&warning_group],
            )
            .await
            .unwrap();
        assert_eq!(implied_rows[0].get::<_, i64>(0), 0);

        let picking_type_row = tx
            .query(
                "SELECT id::int8, default_location_src_id::int8, default_location_dest_id::int8 \
                 FROM stock_picking_type WHERE company_id = $1 ORDER BY id LIMIT 1",
                &[&company],
            )
            .await
            .unwrap();
        let picking_type_id: i64 = picking_type_row[0].get(0);
        let loc_src_id: i64 = picking_type_row[0].get(1);
        let loc_dest_id: i64 = picking_type_row[0].get(2);

        let env = tx.env().with_company(company).unwrap();
        let partners = env.records("res.partner").unwrap();
        let parent_id = partners
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Distribution Group".into()))
                        .set_cell("is_company", Cell::Bool(true))
                        .set_cell(
                            "picking_warn_msg",
                            Cell::Str("Call 30 minutes ahead".into()),
                        ),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let contact_id = partners
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Branch Dock".into()))
                        .set_cell("parent_id", Cell::Id(parent_id))
                        .set_cell(
                            "picking_warn_msg",
                            Cell::Str("Use rear loading dock".into()),
                        ),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let quiet_partner_id = partners
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", Cell::Str("Quiet Customer".into()))],
            )
            .await
            .unwrap()
            .ids()[0];

        // a contact without a message of its own whose parent company has one
        let silent_child_id = partners
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Silent Branch".into()))
                        .set_cell("parent_id", Cell::Id(parent_id)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        // a message of spaces only
        let blank_partner_id = partners
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Blank Note Customer".into()))
                        .set_cell("picking_warn_msg", Cell::Str("   ".into())),
                ],
            )
            .await
            .unwrap()
            .ids()[0];

        let pickings = env.records("stock.picking").unwrap();
        let make_picking = |partner: Option<i64>| {
            let mut vals = RawVals::new()
                .set_cell("company_id", Cell::Id(company))
                .set_cell("picking_type_id", Cell::Id(picking_type_id))
                .set_cell("location_id", Cell::Id(loc_src_id))
                .set_cell("location_dest_id", Cell::Id(loc_dest_id));
            if let Some(pid) = partner {
                vals = vals.set_cell("partner_id", Cell::Id(pid));
            }
            vals
        };
        let picking_warn_id = pickings
            .create(&mut tx, vec![make_picking(Some(contact_id))])
            .await
            .unwrap()
            .ids()[0];
        let picking_quiet_id = pickings
            .create(&mut tx, vec![make_picking(Some(quiet_partner_id))])
            .await
            .unwrap()
            .ids()[0];
        let picking_silent_id = pickings
            .create(&mut tx, vec![make_picking(Some(silent_child_id))])
            .await
            .unwrap()
            .ids()[0];
        let picking_blank_id = pickings
            .create(&mut tx, vec![make_picking(Some(blank_partner_id))])
            .await
            .unwrap()
            .ids()[0];
        let picking_none_id = pickings
            .create(&mut tx, vec![make_picking(None)])
            .await
            .unwrap()
            .ids()[0];
        tx.commit().await.unwrap();
        drop(tx);

        let orm = super::security::secure(&db, &installed).await;

        // 1. Feature disabled: Settings shows Disabled; stock manager cannot read/write picking_warn_msg;
        //    stock user reads empty picking_warning_text.
        let mut tx = orm.begin(admin).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let desc = describe(&env, &mut tx, company_scope(company))
            .await
            .unwrap();
        let json = serde_json::to_value(&desc).unwrap();
        let toggle = json["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == "stock.group_warning_stock")
            .expect("stock.group_warning_stock toggle");
        assert_eq!(toggle["kind"], "group");
        assert_eq!(toggle["state"], "disabled");
        tx.rollback().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(stock_manager).await.unwrap();
        let mgr_env = tx.env().with_company(company).unwrap();
        let mgr_partner = mgr_env
            .records("res.partner")
            .unwrap()
            .browse([contact_id])
            .unwrap();
        let read_err = tx
            .savepoint({
                let rec = mgr_partner.clone();
                async move |tx| rec.cell(tx, "picking_warn_msg").await
            })
            .await
            .unwrap_err();
        assert_eq!(read_err.kind(), ErrorKind::Access);
        assert!(
            read_err.message().contains("picking_warn_msg"),
            "{}",
            read_err.message()
        );
        let write_err = tx
            .savepoint({
                let rec = mgr_partner.clone();
                async move |tx| {
                    rec.write(
                        tx,
                        RawVals::new().set_cell("picking_warn_msg", Cell::Str("Blocked".into())),
                    )
                    .await
                }
            })
            .await
            .unwrap_err();
        assert_eq!(write_err.kind(), ErrorKind::Access);
        assert!(
            write_err.message().contains("picking_warn_msg"),
            "{}",
            write_err.message()
        );
        tx.rollback().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(stock_user).await.unwrap();
        let user_env = tx.env().with_company(company).unwrap();
        assert_eq!(
            user_env
                .records("stock.picking")
                .unwrap()
                .browse([picking_warn_id])
                .unwrap()
                .cell(&mut tx, "picking_warning_text")
                .await
                .unwrap(),
            Cell::Null
        );
        tx.rollback().await.unwrap();
        drop(tx);

        // 2. Enable stock.group_warning_stock through the Settings switch (`toggle_group`), which is
        //    what the Settings page calls: a stock manager is refused, the administrator switches it
        //    on for every internal user.
        let toggle = async |uid: UserId| {
            let orm = super::security::secure(&db, &installed).await;
            let mut tx = orm.begin(uid).await.unwrap();
            let env = tx.env().with_company(company).unwrap();
            let result = toggle_group(&env, &mut tx, "stock.group_warning_stock", true).await;
            if result.is_ok() {
                tx.commit().await.unwrap();
            } else {
                tx.rollback().await.unwrap();
            }
            result
        };
        let refused = toggle(stock_manager).await.unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Access);
        assert_eq!(
            refused.message(),
            "Only Settings administrators may turn a feature on or off for every user."
        );
        assert_eq!(
            toggle(admin).await.unwrap(),
            erp_sdk::views::wire::GroupToggleState::Enabled
        );

        let orm = super::security::secure(&db, &installed).await;

        // Settings reports Enabled.
        let mut tx = orm.begin(admin).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let desc = describe(&env, &mut tx, company_scope(company))
            .await
            .unwrap();
        let json = serde_json::to_value(&desc).unwrap();
        let toggle = json["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == "stock.group_warning_stock")
            .expect("stock.group_warning_stock toggle");
        assert_eq!(toggle["state"], "enabled");
        tx.rollback().await.unwrap();
        drop(tx);

        // Stock manager can read/write picking_warn_msg and hits the 4000-char validation bound.
        let mut tx = orm.begin(stock_manager).await.unwrap();
        let mgr_env = tx.env().with_company(company).unwrap();
        assert_eq!(
            mgr_env
                .records("res.partner")
                .unwrap()
                .browse([contact_id])
                .unwrap()
                .cell(&mut tx, "picking_warn_msg")
                .await
                .unwrap(),
            Cell::Str("Use rear loading dock".into())
        );
        let quiet_rec = mgr_env
            .records("res.partner")
            .unwrap()
            .browse([quiet_partner_id])
            .unwrap();
        let max_ok = "b".repeat(4_000);
        let too_long = "b".repeat(4_001);
        quiet_rec
            .write(
                &mut tx,
                RawVals::new().set_cell("picking_warn_msg", Cell::Str(max_ok.into())),
            )
            .await
            .unwrap();
        tx.flush().await.unwrap();
        let over_err = tx
            .savepoint({
                let rec = quiet_rec.clone();
                async move |tx| {
                    rec.write(
                        tx,
                        RawVals::new().set_cell("picking_warn_msg", Cell::Str(too_long.into())),
                    )
                    .await?;
                    tx.flush().await
                }
            })
            .await
            .unwrap_err();
        assert_eq!(over_err.kind(), ErrorKind::Validation);
        assert_eq!(
            over_err.message(),
            "Transfer notice cannot exceed 4000 characters (base.warning_message_max_chars)."
        );
        quiet_rec
            .write(
                &mut tx,
                RawVals::new().set_cell("picking_warn_msg", Cell::Null),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        // Stock user reads partner + parent warning, and empty when unset or partner missing.
        let mut tx = orm.begin(stock_user).await.unwrap();
        let user_env = tx.env().with_company(company).unwrap();
        let pickings = user_env.records("stock.picking").unwrap();
        assert_eq!(
            pickings
                .browse([picking_warn_id])
                .unwrap()
                .cell(&mut tx, "picking_warning_text")
                .await
                .unwrap(),
            Cell::Str("Use rear loading dock\nCall 30 minutes ahead".into())
        );
        assert_eq!(
            pickings
                .browse([picking_quiet_id])
                .unwrap()
                .cell(&mut tx, "picking_warning_text")
                .await
                .unwrap(),
            Cell::Null
        );
        assert_eq!(
            pickings
                .browse([picking_none_id])
                .unwrap()
                .cell(&mut tx, "picking_warning_text")
                .await
                .unwrap(),
            Cell::Null
        );
        // A contact with no message of its own still shows its parent company's.
        assert_eq!(
            pickings
                .browse([picking_silent_id])
                .unwrap()
                .cell(&mut tx, "picking_warning_text")
                .await
                .unwrap(),
            Cell::Str("Call 30 minutes ahead".into())
        );
        // A message of spaces only shows no banner.
        assert_eq!(
            pickings
                .browse([picking_blank_id])
                .unwrap()
                .cell(&mut tx, "picking_warning_text")
                .await
                .unwrap(),
            Cell::Null
        );
        tx.rollback().await.unwrap();
        drop(tx);

        // Editing the contact's message and reading the transfer again in the same transaction shows
        // the new text (the computed value follows its dependencies).
        let mut tx = orm.begin(stock_manager).await.unwrap();
        let mgr_env = tx.env().with_company(company).unwrap();
        let picking = mgr_env
            .records("stock.picking")
            .unwrap()
            .browse([picking_warn_id])
            .unwrap();
        assert_eq!(
            picking.cell(&mut tx, "picking_warning_text").await.unwrap(),
            Cell::Str("Use rear loading dock\nCall 30 minutes ahead".into())
        );
        mgr_env
            .records("res.partner")
            .unwrap()
            .browse([contact_id])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new()
                    .set_cell("picking_warn_msg", Cell::Str("Gate closes at five".into())),
            )
            .await
            .unwrap();
        assert_eq!(
            picking.cell(&mut tx, "picking_warning_text").await.unwrap(),
            Cell::Str("Gate closes at five\nCall 30 minutes ahead".into())
        );
        tx.rollback().await.unwrap();
        drop(tx);

        // The length bound is the system parameter base.warning_message_max_chars.
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        let params = tx.env().records("ir.config_parameter").unwrap();
        for bad in [0_i64, 20_001] {
            let err = params
                .call::<(String, Option<i64>), i64>(
                    &mut tx,
                    "set_int",
                    ("base.warning_message_max_chars".to_owned(), Some(bad)),
                )
                .await
                .unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Validation, "{bad}");
        }
        params
            .call::<(String, Option<i64>), i64>(
                &mut tx,
                "set_int",
                ("base.warning_message_max_chars".to_owned(), Some(10)),
            )
            .await
            .unwrap();
        let env = tx.env().with_company(company).unwrap();
        let quiet = env
            .records("res.partner")
            .unwrap()
            .browse([quiet_partner_id])
            .unwrap();
        quiet
            .write(
                &mut tx,
                RawVals::new().set_cell("picking_warn_msg", Cell::Str("0123456789".into())),
            )
            .await
            .unwrap();
        let over = tx
            .savepoint({
                let rec = quiet.clone();
                async move |tx| {
                    rec.write(
                        tx,
                        RawVals::new()
                            .set_cell("picking_warn_msg", Cell::Str("01234567890".into())),
                    )
                    .await?;
                    tx.flush().await
                }
            })
            .await
            .unwrap_err();
        assert_eq!(over.kind(), ErrorKind::Validation);
        assert_eq!(
            over.message(),
            "Transfer notice cannot exceed 10 characters (base.warning_message_max_chars)."
        );
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}
