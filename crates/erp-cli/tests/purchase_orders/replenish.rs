// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn a_manual_buy_rule_creates_one_vendor_rfq_only_below_minimum() {
    run_body(|| async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "purchase_stock").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let env = tx.env().with_company(company).unwrap();
        let k = catalog(&mut tx, company).await;
        let route = xmlid(&mut tx, "purchase_stock", "route_buy").await;
        let w = tx.query("SELECT id, lot_stock_id, in_type_id FROM stock_warehouse WHERE active AND company_id=$1", &[&company]).await.unwrap();
        let (warehouse, stock, receipt): (i64, i64, i64) = (w[0].get(0), w[0].get(1), w[0].get(2));
        let existing = tx.query("SELECT id FROM stock_rule WHERE route_id=$1 AND location_dest_id=$2 AND picking_type_id=$3 AND action='buy' AND generated_role='buy_receipt'", &[&route,&stock,&receipt]).await.unwrap();
        assert_eq!(existing.len(), 1);
        env.records("stock.warehouse")
            .unwrap()
            .call::<(), ()>(&mut tx, "_purchase_stock_setup_buy_routes", ())
            .await
            .unwrap();
        let again = tx
            .query(
                "SELECT id FROM stock_rule WHERE route_id=$1 AND location_dest_id=$2",
                &[&route, &stock],
            )
            .await
            .unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(existing[0].get::<_, i64>(0), again[0].get::<_, i64>(0));
        let point = env
            .records("stock.warehouse.orderpoint")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Manual buy"))
                        .set_cell("trigger", text("manual"))
                        .set_cell("product_id", Cell::Id(k.bolt))
                        .set_cell("company_id", Cell::Id(company))
                        .set_cell("warehouse_id", Cell::Id(warehouse))
                        .set_cell("location_id", Cell::Id(stock))
                        .set_cell("route_id", Cell::Id(route))
                        .set_cell("product_max_qty", Cell::Decimal(dec("12"))),
                ],
            )
            .await
            .unwrap();
        let count = async |tx: &mut Transaction| -> i64 {
            tx.query("SELECT count(*) FROM purchase_order", &[])
                .await
                .unwrap()[0]
                .get(0)
        };
        tx.execute("INSERT INTO stock_quant(company_id,product_id,location_id,quantity,reserved_quantity,in_date) VALUES($1,$2,$3,5,0,now()),($1,$4,$3,2,0,now())",&[&company,&k.bolt,&stock,&k.nut]).await.unwrap();
        point
            .write(
                &mut tx,
                RawVals::new().set_cell("product_min_qty", Cell::Decimal(dec("5"))),
            )
            .await
            .unwrap();
        let before = count(&mut tx).await;
        let err = point
            .call::<(), Value>(&mut tx, "action_replenish_buy", ())
            .await
            .unwrap_err();
        assert_eq!(
            err.message(),
            "Stock already meets this rule's minimum. No request for quotation is needed."
        );
        assert_eq!(count(&mut tx).await, before);
        point
            .write(
                &mut tx,
                RawVals::new().set_cell("product_min_qty", Cell::Decimal(dec("6"))),
            )
            .await
            .unwrap();
        let err = point
            .call::<(), Value>(&mut tx, "action_replenish_buy", ())
            .await
            .unwrap_err();
        assert_eq!(
            err.message(),
            "No eligible vendor price covers this product, quantity and company."
        );
        assert_eq!(count(&mut tx).await, before);
        point
            .write(
                &mut tx,
                RawVals::new().set_cell("product_id", Cell::Id(k.nut)),
            )
            .await
            .unwrap();
        let source = point.ensure_one().unwrap().int().unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let request = || {
            let orm = orm.clone();
            async move {
                orm.run(SUPERUSER_ID, Context::new(), async |tx| {
                    tx.env()
                        .with_company(company)?
                        .records("stock.warehouse.orderpoint")?
                        .browse([source])?
                        .call::<(), Value>(tx, "action_replenish_buy", ())
                        .await
                })
                .await
            }
        };
        let (first, concurrent) = tokio::join!(request(), request());
        let opened = first.unwrap();
        assert_eq!(opened, concurrent.unwrap());
        assert_eq!(opened, request().await.unwrap());
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let Value::Json(action) = opened else {
            panic!("expected a native window action")
        };
        assert_eq!(action["target"], "new");
        assert_eq!(action["res_model"], "purchase.order");
        let order = env
            .records("purchase.order")
            .unwrap()
            .browse([action["res_id"].as_i64().unwrap()])
            .unwrap();
        assert_eq!(order.cell(&mut tx, "state").await.unwrap(), text("draft"));
        assert_eq!(
            order.cell(&mut tx, "partner_id").await.unwrap(),
            Cell::Id(k.vendor)
        );
        let line = order.related(&mut tx, "order_line").await.unwrap();
        assert_eq!(line.len(), 1);
        assert_eq!(
            line.cell(&mut tx, "product_qty").await.unwrap(),
            Cell::Decimal(dec("10"))
        );
        assert_eq!(count(&mut tx).await, before + 1);
    });
}

#[test]
fn shared_destination_keeps_buy_rules_local_and_requires_complete_local_access() {
    run_body(|| async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "purchase_stock").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let main = xmlid(&mut tx, "base", "main_company").await;
        let env = tx.env().with_company(main).unwrap();
        let route = xmlid(&mut tx, "purchase_stock", "route_buy").await;
        let catalog = catalog(&mut tx, main).await;
        let currency: i64 = tx
            .query("SELECT currency_id FROM res_company WHERE id=$1", &[&main])
            .await
            .unwrap()[0]
            .get(0);
        let system = xmlid(&mut tx, "base", "group_system").await;
        user(&mut tx, "cross-dock-admin", &[system], &[main]).await;
        let buyer_group = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let manager_group = xmlid(&mut tx, "stock", "group_stock_manager").await;
        let buyer = user(
            &mut tx,
            "cross-dock-buyer",
            &[buyer_group, manager_group],
            &[main],
        )
        .await;
        let selected_route = env.records("stock.route").unwrap().browse([route]).unwrap();
        selected_route
            .write(
                &mut tx,
                RawVals::new().set_cell("company_id", Cell::Id(main)),
            )
            .await
            .unwrap();
        let other = env
            .records("res.company")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Other receiving company"))
                        .set_cell("currency_id", Cell::Id(currency)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let home = env
            .records("stock.warehouse")
            .unwrap()
            .search(&mut tx, &Domain::field("company_id").unwrap().eq(main))
            .await
            .unwrap();
        home.write(
            &mut tx,
            RawVals::new().set_cell("active", Cell::Bool(false)),
        )
        .await
        .unwrap();
        let stock = env
            .records("stock.location")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Shared receiving floor"))
                        .set_cell("usage", text("internal"))
                        .set_cell("company_id", Cell::Null),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let supplier = xmlid(&mut tx, "stock", "stock_location_suppliers").await;
        let make_wh = async |tx: &mut Transaction, company: i64, code: &str| -> i64 {
            let env = tx.env().with_company(company).unwrap();
            let incoming = env
                .records("stock.picking.type")
                .unwrap()
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", text(code))
                            .set_cell("company_id", Cell::Id(company))
                            .set_cell("code", text("incoming"))
                            .set_cell("sequence_code", text(code))
                            .set_cell("default_location_src_id", Cell::Id(supplier))
                            .set_cell("default_location_dest_id", Cell::Id(stock)),
                    ],
                )
                .await
                .unwrap();
            let wh = env
                .records("stock.warehouse")
                .unwrap()
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", text(code))
                            .set_cell("code", text(code))
                            .set_cell("company_id", Cell::Id(company))
                            .set_cell("view_location_id", Cell::Id(stock))
                            .set_cell("lot_stock_id", Cell::Id(stock))
                            .set_cell("in_type_id", Cell::Id(incoming.ids()[0])),
                    ],
                )
                .await
                .unwrap()
                .ids()[0];
            incoming
                .write(tx, RawVals::new().set_cell("warehouse_id", Cell::Id(wh)))
                .await
                .unwrap();
            wh
        };
        let warehouse = make_wh(&mut tx, main, "LOCAL").await;
        let receipt: i64 = tx
            .query(
                "SELECT in_type_id FROM stock_warehouse WHERE id=$1",
                &[&warehouse],
            )
            .await
            .unwrap()[0]
            .get(0);
        let foreign_wh = make_wh(&mut tx, other, "OTHER").await;
        let own_rules = async |tx: &mut Transaction, wh: i64, company: i64| -> i64 {
            tx.query("SELECT count(*) FROM stock_rule WHERE generated_warehouse_id=$1 AND generated_addon='purchase_stock' AND generated_role='buy_receipt' AND company_id=$2 AND route_id=$3 AND location_dest_id=$4",&[&wh,&company,&route,&stock]).await.unwrap()[0].get(0)
        };
        assert_eq!(
            own_rules(&mut tx, foreign_wh, other).await,
            0,
            "the route belongs to the other warehouse's peer company"
        );
        selected_route
            .write(&mut tx, RawVals::new().set_cell("company_id", Cell::Null))
            .await
            .unwrap();
        env.records("stock.warehouse")
            .unwrap()
            .call::<(), ()>(&mut tx, "_purchase_stock_setup_buy_routes", ())
            .await
            .unwrap();
        assert_eq!(
            own_rules(&mut tx, foreign_wh, other).await,
            1,
            "another company at the same dock must receive its own Buy rule"
        );
        assert_eq!(own_rules(&mut tx, warehouse, main).await, 1);
        env.records("stock.warehouse")
            .unwrap()
            .call::<(), ()>(&mut tx, "_purchase_stock_setup_buy_routes", ())
            .await
            .unwrap();
        assert_eq!(own_rules(&mut tx, foreign_wh, other).await, 1);
        let point = env
            .records("stock.warehouse.orderpoint")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Shared dock Buy rule"))
                        .set_cell("trigger", text("manual"))
                        .set_cell("product_id", Cell::Id(catalog.nut))
                        .set_cell("company_id", Cell::Id(main))
                        .set_cell("warehouse_id", Cell::Id(warehouse))
                        .set_cell("location_id", Cell::Id(stock))
                        .set_cell("route_id", Cell::Id(route))
                        .set_cell("product_min_qty", Cell::Decimal(dec("1")))
                        .set_cell("product_max_qty", Cell::Decimal(dec("12"))),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.install_audit_schema().await.unwrap();
        tx.commit().await.unwrap();
        let orm = purchase_policy_orm(&db, &orm, &[]).await;
        let mut tx = orm.begin(buyer).await.unwrap();
        let buyer_point = tx
            .env()
            .with_company(main)
            .unwrap()
            .records("stock.warehouse.orderpoint")
            .unwrap()
            .browse([point])
            .unwrap();
        let result = buyer_point
            .call::<(), Value>(&mut tx, "action_replenish_buy", ())
            .await
            .unwrap();
        let Value::Json(action) = result else {
            panic!("expected a native RFQ action")
        };
        assert_eq!(action["res_model"], "purchase.order");
        tx.rollback().await.unwrap();

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let env = tx.env().with_company(main).unwrap();
        // Older databases can contain a shared custom rule for this receipt. Clear its generated
        // identity so only the route/destination/company comparison can preserve the custom rule.
        let changed = tx.execute(
            "UPDATE stock_rule SET company_id=NULL, generated_warehouse_id=NULL, generated_role=NULL, generated_addon=NULL \
             WHERE generated_warehouse_id=$1 AND generated_role='buy_receipt' AND generated_addon='purchase_stock'",
            &[&warehouse],
        ).await.unwrap();
        assert_eq!(changed, 1);
        env.records("stock.warehouse")
            .unwrap()
            .call::<(), ()>(&mut tx, "_purchase_stock_setup_buy_routes", ())
            .await
            .unwrap();
        assert_eq!(
            own_rules(&mut tx, warehouse, main).await,
            0,
            "an existing shared rule prevents a duplicate"
        );
        tx.commit().await.unwrap();
        let mut tx = orm.begin(buyer).await.unwrap();
        let approved = tx
            .env()
            .with_company(main)
            .unwrap()
            .records("stock.warehouse.orderpoint")
            .unwrap()
            .browse([point])
            .unwrap();
        let approved = approved
            .call::<(), Value>(&mut tx, "action_replenish_buy", ())
            .await
            .unwrap();
        let Value::Json(action) = approved else {
            panic!("expected a shared receiving rule to open its RFQ")
        };
        assert_eq!(action["res_model"], "purchase.order");
        tx.rollback().await.unwrap();

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let env = tx.env().with_company(main).unwrap();
        let hidden = env
            .records("stock.rule")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Restricted receiving rule"))
                        .set_cell("action", text("buy"))
                        .set_cell("company_id", Cell::Id(main))
                        .set_cell("route_id", Cell::Id(route))
                        .set_cell("location_dest_id", Cell::Id(stock))
                        .set_cell("picking_type_id", Cell::Id(receipt)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let model = xmlid(&mut tx, "stock", "model_stock_rule").await;
        env.records("ir.access")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Hide second local rule"))
                        .set_cell("model_id", Cell::Id(model))
                        .set_cell("operation", text("r"))
                        .set_cell("domain", text(&format!("[('id','!=',{hidden})]"))),
                ],
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let orm = purchase_policy_orm(&db, &orm, &[]).await;
        let mut tx = orm.begin(buyer).await.unwrap();
        let buyer_point = tx
            .env()
            .with_company(main)
            .unwrap()
            .records("stock.warehouse.orderpoint")
            .unwrap()
            .browse([point])
            .unwrap();
        let err = buyer_point
            .call::<(), Value>(&mut tx, "action_replenish_buy", ())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Access);
        assert_eq!(
            tx.query(
                "SELECT count(*) FROM purchase_stock_replenishment_link WHERE orderpoint_id=$1",
                &[&point]
            )
            .await
            .unwrap()[0]
                .get::<_, i64>(0),
            0
        );
        assert_eq!(
            tx.query(
                "SELECT count(*) FROM purchase_order WHERE company_id=$1 AND origin=$2",
                &[&main, &"Shared dock Buy rule"]
            )
            .await
            .unwrap()[0]
                .get::<_, i64>(0),
            0
        );
        assert_eq!(
            err.message(),
            "Complete access to this route's receiving rules is required."
        );
        tx.rollback().await.unwrap();
    });
}
