// SPDX-License-Identifier: MIT OR Apache-2.0
//! Negative admission cases through the installed Sales and Stock policies.

use super::*;

const SOURCE_ACCESS: &str = "The source records for this sales delivery are not accessible.";
const ROUTE_ERROR: &str = "This sales order needs an active one-step warehouse with one outgoing make-to-stock delivery rule. Other routes require a later release.";

#[test]
fn hidden_unit_ancestor_refuses_before_elevated_stock_creation() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install_sales(&db, true).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        admin(&mut tx).await;
        let salesperson = user(&mut tx, "ancestor-admission", "group_sale_salesman").await;
        let world = world(&mut tx).await;
        let goods = product(&mut tx, "Derived shipment units", "consu").await;
        let product = tx
            .env()
            .records("product.product")
            .unwrap()
            .browse([goods])
            .unwrap();
        let root = many2one(&mut tx, &product, "uom_id").await.unwrap();
        let units = tx.env().records("uom.uom").unwrap();
        let middle = units
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Restricted intermediate unit".into()))
                        .set_cell("relative_uom_id", Cell::Id(root))
                        .set_cell("relative_factor", Cell::Decimal(Decimal::from(2))),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let leaf = units
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Readable shipping unit".into()))
                        .set_cell("relative_uom_id", Cell::Id(middle))
                        .set_cell("relative_factor", Cell::Decimal(Decimal::from(3))),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        product
            .write(
                &mut tx,
                RawVals::new().set_commands("uom_ids", [Command::link(id(leaf))]),
            )
            .await
            .unwrap();
        let (order_id, line, reference) =
            order_with_product(&mut tx, &world, salesperson, goods, RawVals::new()).await;
        tx.env()
            .records("sale.order.line")
            .unwrap()
            .browse([line])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("product_uom_id", Cell::Id(leaf)),
            )
            .await
            .unwrap();
        let model: i64 = tx
            .query("SELECT id FROM ir_model WHERE model = 'uom.uom'", &[])
            .await
            .unwrap()[0]
            .get(0);
        tx.env()
            .records("ir.access")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Hide conversion ancestry".into()))
                        .set_cell("model_id", Cell::Id(model))
                        .set_cell("operation", Cell::Str("r".into()))
                        .set_cell(
                            "domain",
                            Cell::Str(format!("[('id', '!=', {middle})]").into()),
                        ),
                ],
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        let orm = policy_orm(&db, &installed).await;
        let mut tx = orm.begin(UserId::new(salesperson).unwrap()).await.unwrap();
        let endpoints = tx
            .env()
            .records("uom.uom")
            .unwrap()
            .browse([root, leaf])
            .unwrap();
        endpoints
            .check_access(&mut tx, Operation::Read)
            .await
            .unwrap();
        assert_eq!(
            endpoints.cells(&mut tx, "factor").await.unwrap(),
            vec![Cell::Decimal(Decimal::ONE), Cell::Decimal(Decimal::from(6))]
        );
        let order = tx
            .env()
            .records("sale.order")
            .unwrap()
            .browse([order_id])
            .unwrap();
        refused_exact(
            order.call::<(), ()>(&mut tx, "action_confirm", ()).await,
            ErrorKind::Access,
            SOURCE_ACCESS,
        );
        assert_no_shipment(&mut tx, &order, &reference).await;
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

async fn assert_no_shipment(tx: &mut Transaction, order: &Records, reference: &str) {
    assert_eq!(text(tx, order, "state").await, "draft");
    assert_eq!(transfers(tx, reference).await, 0);
    let rows = tx
        .query(
            "SELECT count(*) FROM sale_stock_delivery_link WHERE order_id = $1",
            &[&order.ids()[0]],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get::<_, i64>(0), 0);
}

#[test]
fn hidden_product_unit_and_shipping_contact_refuse_the_complete_confirmation() {
    run(async {
        // Each policy is installed in its own database: a denied shared unit must not
        // accidentally decide the product/contact cases before their own checks run.
        for hidden_model in ["product.product", "uom.uom", "res.partner"] {
            let db = TestDb::create().await.unwrap();
            let installed = install_sales(&db, true).await;
            let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
            admin(&mut tx).await;
            let salesperson = user(&mut tx, "source-admission", "group_sale_salesman").await;
            let world = world(&mut tx).await;
            let shipping = partner(
                &mut tx,
                RawVals::new()
                    .set_cell("name", Cell::Str("Restricted shipping desk".into()))
                    .set_cell("parent_id", Cell::Id(world.customer))
                    .set_cell("type", Cell::Str("delivery".into())),
            )
            .await;
            let goods = product(&mut tx, "Restricted source goods", "consu").await;
            let (order_id, line, reference) = order_with_product(
                &mut tx,
                &world,
                salesperson,
                goods,
                RawVals::new().set_cell("partner_shipping_id", Cell::Id(shipping)),
            )
            .await;
            let unit: i64 = tx
                .query(
                    "SELECT product_uom_id FROM sale_order_line WHERE id = $1",
                    &[&line],
                )
                .await
                .unwrap()[0]
                .get(0);
            let hidden_id = match hidden_model {
                "product.product" => goods,
                "uom.uom" => unit,
                "res.partner" => shipping,
                _ => unreachable!("fixed test cases"),
            };
            let model: i64 = tx
                .query("SELECT id FROM ir_model WHERE model = $1", &[&hidden_model])
                .await
                .unwrap()[0]
                .get(0);
            tx.env()
                .records("ir.access")
                .unwrap()
                .create(
                    &mut tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", Cell::Str("Deny copied source record".into()))
                            .set_cell("model_id", Cell::Id(model))
                            .set_cell("operation", Cell::Str("r".into()))
                            .set_cell(
                                "domain",
                                Cell::Str(format!("[('id', '!=', {hidden_id})]").into()),
                            ),
                    ],
                )
                .await
                .unwrap();
            tx.commit().await.unwrap();
            drop(tx);

            let orm = policy_orm(&db, &installed).await;
            let mut tx = orm.begin(UserId::new(salesperson).unwrap()).await.unwrap();
            let order = tx
                .env()
                .records("sale.order")
                .unwrap()
                .browse([order_id])
                .unwrap();
            refused_exact(
                order.call::<(), ()>(&mut tx, "action_confirm", ()).await,
                ErrorKind::Access,
                SOURCE_ACCESS,
            );
            assert_no_shipment(&mut tx, &order, &reference).await;
            tx.rollback().await.unwrap();
            drop(tx);
            db.cleanup().await.unwrap();
        }
    });
}

#[test]
fn unsupported_route_configuration_refuses_without_stock_side_effects() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install_sales(&db, true).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        admin(&mut tx).await;
        let salesperson = user(&mut tx, "route-admission", "group_sale_salesman").await;
        let world = world(&mut tx).await;
        let warehouse: i64 = tx
            .query(
                "SELECT id FROM stock_warehouse WHERE company_id = $1",
                &[&world.company],
            )
            .await
            .unwrap()[0]
            .get(0);
        let goods = product(&mut tx, "Route admission goods", "consu").await;
        let (order_id, _, reference) = order_with_product(
            &mut tx,
            &world,
            salesperson,
            goods,
            RawVals::new().set_cell("warehouse_id", Cell::Id(warehouse)),
        )
        .await;
        tx.commit().await.unwrap();
        drop(tx);
        let orm = policy_orm(&db, &installed).await;
        // These represent persisted legacy/admin configurations. Each transaction
        // rolls back its one mutation, so all cases start from the valid topology.
        for sql in [
            "UPDATE stock_warehouse SET active = false WHERE id = $1",
            "UPDATE stock_warehouse SET delivery_steps = 'pick_ship' WHERE id = $1",
            "UPDATE stock_rule SET active = false WHERE warehouse_id = $1",
            "UPDATE stock_rule SET procure_method = 'make_to_order' WHERE warehouse_id = $1",
            "UPDATE stock_picking_type SET code = 'incoming' WHERE id = (SELECT out_type_id FROM stock_warehouse WHERE id = $1)",
        ] {
            let mut tx = orm.begin(UserId::new(salesperson).unwrap()).await.unwrap();
            tx.execute(sql, &[&warehouse]).await.unwrap();
            let order = tx
                .env()
                .records("sale.order")
                .unwrap()
                .browse([order_id])
                .unwrap();
            refused_exact(
                order.call::<(), ()>(&mut tx, "action_confirm", ()).await,
                ErrorKind::UserError,
                ROUTE_ERROR,
            );
            assert_no_shipment(&mut tx, &order, &reference).await;
            tx.rollback().await.unwrap();
        }
        db.cleanup().await.unwrap();
    });
}

#[test]
fn source_row_limit_counts_descriptive_lines_and_admits_the_exact_boundary() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install_sales(&db, true).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        admin(&mut tx).await;
        let salesperson = user(&mut tx, "bounded-delivery", "group_sale_salesman").await;
        let world = world(&mut tx).await;
        let goods = product(&mut tx, "Bounded delivery goods", "consu").await;
        let (exact, _, exact_ref) =
            order_with_product(&mut tx, &world, salesperson, goods, RawVals::new()).await;
        let (over, _, over_ref) =
            order_with_product(&mut tx, &world, salesperson, goods, RawVals::new()).await;
        tx.env()
            .records("sale.order.line")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("order_id", Cell::Id(over))
                        .set_cell("display_type", Cell::Str("line_note".into()))
                        .set_cell("name", Cell::Str("Counts toward source membership".into())),
                ],
            )
            .await
            .unwrap();
        let _: i64 = tx
            .env()
            .records("ir.config_parameter")
            .unwrap()
            .call(
                &mut tx,
                "set_int",
                (
                    "sale_stock.max_confirmation_source_rows".to_owned(),
                    Some(1_i64),
                ),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let orm = policy_orm(&db, &installed).await;
        let mut tx = orm.begin(UserId::new(salesperson).unwrap()).await.unwrap();
        let over = tx
            .env()
            .records("sale.order")
            .unwrap()
            .browse([over])
            .unwrap();
        refused_exact(
            over.call::<(), ()>(&mut tx, "action_confirm", ()).await,
            ErrorKind::UserError,
            "Sales delivery confirmation exceeds sale_stock.max_confirmation_source_rows (1).",
        );
        assert_no_shipment(&mut tx, &over, &over_ref).await;
        let exact = tx
            .env()
            .records("sale.order")
            .unwrap()
            .browse([exact])
            .unwrap();
        exact
            .call::<(), ()>(&mut tx, "action_confirm", ())
            .await
            .unwrap();
        assert_eq!(text(&mut tx, &exact, "state").await, "sale");
        assert_eq!(transfers(&mut tx, &exact_ref).await, 1);
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

/// An archived constraint still belongs to the movement, even when a normal read omits it.
pub(super) async fn archived_route_prevents_split(tx: &mut Transaction, picking: &Records) {
    let route = picking
        .env()
        .records("stock.route")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("Archived special handling".into()))
                    .set_cell("active", Cell::Bool(false)),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    let movement = picking.related(tx, "move_ids").await.unwrap();
    let move_id = movement.ensure_one().unwrap().int().unwrap();
    tx.execute(
        "INSERT INTO stock_route_move(move_id,route_id) VALUES($1,$2)",
        &[&move_id, &route],
    )
    .await
    .unwrap();
    tx.invalidate_model("stock.move", Some(&["route_ids"]))
        .await
        .unwrap();
    assert!(movement.related(tx, "route_ids").await.unwrap().is_empty());
    let before = movement.cell(tx, "product_uom_qty").await.unwrap();
    let error = picking
        .call::<(), ()>(tx, "button_validate", ())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error}");
    let after = tx
        .query(
            "SELECT COUNT(*) FROM stock_picking WHERE backorder_id=$1",
            &[&picking.ids()[0]],
        )
        .await
        .unwrap();
    assert_eq!(after[0].get::<_, i64>(0), 0);
    assert_eq!(movement.cell(tx, "product_uom_qty").await.unwrap(), before);
    tx.execute(
        "DELETE FROM stock_route_move WHERE move_id=$1 AND route_id=$2",
        &[&move_id, &route],
    )
    .await
    .unwrap();
}
