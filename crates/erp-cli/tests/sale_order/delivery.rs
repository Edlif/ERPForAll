// SPDX-License-Identifier: MIT OR Apache-2.0
//! The direct Stock bridge through confirmation of a committed quotation.

use erp_fields::Decimal;
use erp_orm::{Operation, UserId};

use super::*;

#[path = "delivery_admission.rs"]
mod admission;

fn refused_exact(result: erp_fields::Result<()>, kind: ErrorKind, message: &str) {
    let error = result.expect_err(message);
    assert_eq!(error.kind(), kind, "{error:?}");
    assert_eq!(error.message(), message);
}

async fn install_sales(db: &TestDb, stock: bool) -> Orm {
    let system = system_with_stock(db, true);
    let initialized = Box::pin(system.initialize(InitOptions::new()))
        .await
        .unwrap();
    assert!(initialized.post_commit.is_clean());
    let modules: &[&str] = if stock {
        &["sale", "stock_account"]
    } else {
        &["sale"]
    };
    let modules = modules
        .iter()
        .map(|name| ModuleName::new(*name).unwrap())
        .collect::<Vec<_>>();
    let installed =
        Box::pin(system.button_immediate_install(&system.system_env(Context::new()), &modules))
            .await
            .unwrap();
    assert!(
        installed.post_commit.is_clean(),
        "{:?}",
        installed.post_commit
    );
    installed.orm
}

async fn product(tx: &mut Transaction, name: &str, kind: &str) -> i64 {
    tx.env()
        .records("product.product")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("type", Cell::Str(kind.into()))
                    .set_cell("invoice_policy", Cell::Str("order".into()))
                    .set_cell("is_storable", Cell::Bool(kind == "consu")),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

async fn order_with_product(
    tx: &mut Transaction,
    world: &World,
    salesperson: i64,
    product: i64,
    extra: RawVals,
) -> (i64, i64, String) {
    let quotation = order(
        tx,
        world,
        extra
            .set_cell("user_id", Cell::Id(salesperson))
            .set_commands(
                "order_line",
                [Command::create(
                    RawVals::new()
                        .set_cell("product_id", Cell::Id(product))
                        .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(3))),
                )],
            ),
    )
    .await;
    let order = quotation.ids()[0];
    let reference = text(tx, &quotation, "name").await;
    let line = tx
        .query(
            "SELECT id FROM sale_order_line WHERE order_id = $1",
            &[&order],
        )
        .await
        .unwrap()[0]
        .get(0);
    (order, line, reference)
}

async fn transfers(tx: &mut Transaction, origin: &str) -> i64 {
    tx.query(
        "SELECT count(*) FROM stock_picking WHERE origin = $1",
        &[&origin],
    )
    .await
    .unwrap()[0]
        .get(0)
}

#[test]
fn direct_delivery_confirms_saved_order_as_salesperson() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let installed = install_sales(&db, true).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        admin(&mut tx).await;
        let salesperson = user(&mut tx, "delivery-sales", "group_sale_salesman").await;
        let world = world(&mut tx).await;
        let alternate = tx
            .env()
            .records("stock.location")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Customer receiving bay".into()))
                        .set_cell("usage", Cell::Str("customer".into()))
                        .set_cell("company_id", Cell::Id(world.company)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let shipping = partner(
            &mut tx,
            RawVals::new()
                .set_cell("name", Cell::Str("Shipping desk".into()))
                .set_cell("parent_id", Cell::Id(world.customer))
                .set_cell("type", Cell::Str("delivery".into()))
                .set_cell("property_stock_customer", Cell::Id(alternate)),
        )
        .await;
        let goods = product(&mut tx, "Shipping cartons", "consu").await;
        let (quotation, line, reference) = order_with_product(
            &mut tx,
            &world,
            salesperson,
            goods,
            RawVals::new().set_cell("partner_shipping_id", Cell::Id(shipping)),
        )
        .await;
        let dozen = xmlid(&mut tx, "uom", "product_uom_dozen").await;
        let goods = tx
            .env()
            .records("product.product")
            .unwrap()
            .browse([goods])
            .unwrap();
        let goods_id = goods.ids()[0];
        write(
            &mut tx,
            &goods,
            RawVals::new().set_commands("uom_ids", [Command::link(id(dozen))]),
        )
        .await
        .unwrap();
        let source_line = tx
            .env()
            .records("sale.order.line")
            .unwrap()
            .browse([line])
            .unwrap();
        write(
            &mut tx,
            &source_line,
            RawVals::new().set_cell("product_uom_id", Cell::Id(dozen)),
        )
        .await
        .unwrap();
        let warehouse = tx
            .query(
                "SELECT out_type_id, lot_stock_id FROM stock_warehouse WHERE company_id = $1",
                &[&world.company],
            )
            .await
            .unwrap();
        let operation: i64 = warehouse[0].get(0);
        let source: i64 = warehouse[0].get(1);
        tx.commit().await.unwrap();
        drop(tx);

        let orm = policy_orm(&db, &installed).await;
        let mut tx = orm.begin(UserId::new(salesperson).unwrap()).await.unwrap();
        let orders = tx
            .env()
            .records("sale.order")
            .unwrap()
            .browse([quotation])
            .unwrap();
        orders
            .call::<(), ()>(&mut tx, "action_confirm", ())
            .await
            .unwrap();
        assert_eq!(text(&mut tx, &orders, "state").await, "sale");
        assert_eq!(transfers(&mut tx, &reference).await, 1);
        let rows = tx
            .query(
                "SELECT p.id, p.partner_id, p.company_id, p.picking_type_id, p.location_id, \
                        m.product_id, m.product_uom_qty, m.uom_id, m.state, l.sale_line_id, p.location_dest_id, m.product_qty \
                 FROM stock_picking p JOIN stock_move m ON m.picking_id = p.id \
                 JOIN sale_stock_delivery_move_link l ON l.move_id = m.id WHERE p.origin = $1",
                &[&reference],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        let picking: i64 = row.get(0);
        assert_eq!(row.get::<_, i64>(1), shipping);
        assert_eq!(row.get::<_, i64>(2), world.company);
        assert_eq!(row.get::<_, i64>(3), operation);
        assert_eq!(row.get::<_, i64>(4), source);
        assert_eq!(row.get::<_, i64>(5), goods_id);
        assert_eq!(row.get::<_, Decimal>(6), Decimal::from(3));
        let uom: i64 = tx
            .query(
                "SELECT product_uom_id FROM sale_order_line WHERE id = $1",
                &[&line],
            )
            .await
            .unwrap()[0]
            .get(0);
        assert_eq!(uom, dozen);
        assert_eq!(row.get::<_, i64>(7), uom);
        assert_eq!(row.get::<_, &str>(8), "confirmed");
        assert_eq!(row.get::<_, i64>(9), line);
        assert_eq!(row.get::<_, i64>(10), alternate);
        assert_eq!(row.get::<_, Decimal>(11), Decimal::from(36));

        let mut concurrent = orm.begin(UserId::new(salesperson).unwrap()).await.unwrap();
        let competing = concurrent
            .env()
            .records("sale.order")
            .unwrap()
            .browse([quotation])
            .unwrap();
        refused_exact(
            competing
                .call::<(), ()>(&mut concurrent, "action_confirm", ())
                .await,
            ErrorKind::Lock,
            "Cannot grab a lock on records",
        );
        concurrent.rollback().await.unwrap();

        let records = tx
            .env()
            .records("stock.picking")
            .unwrap()
            .browse([picking])
            .unwrap();
        assert!(!records.has_access(&mut tx, Operation::Write).await.unwrap());
        refused_exact(
            orders.call::<(), ()>(&mut tx, "action_confirm", ()).await,
            ErrorKind::Validation,
            &format!("Only a draft or sent quotation can be confirmed: {reference} (Sales Order)."),
        );
        refused_exact(
            orders.call::<(), ()>(&mut tx, "action_cancel", ()).await,
            ErrorKind::UserError,
            "Cancel the linked transfer in Inventory before cancelling its sales order. Completed deliveries require the a later release returns workflow.",
        );
        assert_eq!(text(&mut tx, &orders, "state").await, "sale");
        assert_eq!(transfers(&mut tx, &reference).await, 1);
        tx.commit().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn direct_delivery_rolls_back_after_the_final_sales_step_refuses() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install_sales(&db, true).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        admin(&mut tx).await;
        let manager = user(&mut tx, "delivery-manager", "group_sale_manager").await;
        let world = world(&mut tx).await;
        let goods = product(&mut tx, "Rollback test goods", "consu").await;
        let (order_id, _, reference) =
            order_with_product(&mut tx, &world, manager, goods, RawVals::new()).await;
        let key = erp_addon_sale::LOCK_CONFIRMED;
        set_setting(&mut tx, key, Value::Bool(true)).await;
        tx.execute(
            "UPDATE ir_config_setting_value SET value_text = '123' WHERE setting_key = $1",
            &[&key],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        let orm = policy_orm(&db, &installed).await;
        let mut tx = orm.begin(UserId::new(manager).unwrap()).await.unwrap();
        let order = tx
            .env()
            .records("sale.order")
            .unwrap()
            .browse([order_id])
            .unwrap();
        let before = order.cell(&mut tx, "date_order").await.unwrap();
        refused_exact(
            order.call::<(), ()>(&mut tx, "action_confirm", ()).await,
            ErrorKind::Validation,
            "Stored setting sale.lock_confirmed_sales is invalid.",
        );
        assert_eq!(text(&mut tx, &order, "state").await, "draft");
        assert_eq!(order.cell(&mut tx, "date_order").await.unwrap(), before);
        assert_eq!(transfers(&mut tx, &reference).await, 0);
        let links = tx.query(
            "SELECT (SELECT count(*) FROM sale_stock_delivery_link),                     (SELECT count(*) FROM sale_stock_delivery_move_link),                     (SELECT count(*) FROM stock_move WHERE origin = $1)",
            &[&reference],
        ).await.unwrap();
        for column in 0..3 {
            assert_eq!(links[0].get::<_, i64>(column), 0);
        }
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn direct_delivery_refuses_hidden_demand_and_a_foreign_warehouse() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install_sales(&db, true).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        admin(&mut tx).await;
        let salesperson = user(&mut tx, "restricted-delivery-sales", "group_sale_salesman").await;
        let world = world(&mut tx).await;
        let goods = product(&mut tx, "Visible delivery goods", "consu").await;
        let (protected_order, protected_line, protected_reference) =
            order_with_product(&mut tx, &world, salesperson, goods, RawVals::new()).await;
        let line_model = xmlid(&mut tx, "sale", "model_sale_order_line").await;
        tx.env()
            .records("ir.access")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Hide one source line".into()))
                        .set_cell("model_id", Cell::Id(line_model))
                        .set_cell("operation", Cell::Str("r".into()))
                        .set_cell(
                            "domain",
                            Cell::Str(format!("[('id', '!=', {protected_line})]").into()),
                        ),
                ],
            )
            .await
            .unwrap();
        let currency = tx
            .query(
                "SELECT currency_id FROM res_company WHERE id = $1",
                &[&world.company],
            )
            .await
            .unwrap()[0]
            .get::<_, i64>(0);
        let company = tx
            .env()
            .records("res.company")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Another shipping company".into()))
                        .set_cell("currency_id", Cell::Id(currency)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let warehouse: i64 = tx
            .query(
                "SELECT id FROM stock_warehouse WHERE company_id = $1",
                &[&company],
            )
            .await
            .unwrap()[0]
            .get(0);
        let (wrong_order, _, wrong_reference) =
            order_with_product(&mut tx, &world, salesperson, goods, RawVals::new()).await;
        // Represent a persisted legacy/imported choice, even if the ORM precludes such a write.
        tx.execute(
            "UPDATE sale_order SET warehouse_id = $1 WHERE id = $2",
            &[&warehouse, &wrong_order],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        let orm = policy_orm(&db, &installed).await;
        let mut tx = orm.begin(UserId::new(salesperson).unwrap()).await.unwrap();
        for (order, reference) in [
            (protected_order, protected_reference),
            (wrong_order, wrong_reference),
        ] {
            let order = tx
                .env()
                .records("sale.order")
                .unwrap()
                .browse([order])
                .unwrap();
            refused_exact(
                order.call::<(), ()>(&mut tx, "action_confirm", ()).await,
                ErrorKind::Access,
                "The source records for this sales delivery are not accessible.",
            );
            assert_eq!(text(&mut tx, &order, "state").await, "draft");
            assert_eq!(transfers(&mut tx, &reference).await, 0);
        }
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn service_only_and_sales_without_stock_bridge_keep_base_confirmation() {
    run(async {
        for (stock, kind) in [(false, "consu"), (true, "service"), (true, "consu")] {
            let db = TestDb::create().await.unwrap();
            let installed = install_sales(&db, stock).await;
            let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
            admin(&mut tx).await;
            let salesperson = user(&mut tx, "plain-order-sales", "group_sale_salesman").await;
            let world = world(&mut tx).await;
            let product = product(&mut tx, "Plain sale item", kind).await;
            let (order_id, _, reference) =
                order_with_product(&mut tx, &world, salesperson, product, RawVals::new()).await;
            if stock && kind == "consu" {
                // With no configured route, base confirmation also serves an ordinary goods order.
                tx.execute(
                    "UPDATE stock_warehouse SET delivery_route_id = NULL WHERE company_id = $1",
                    &[&world.company],
                )
                .await
                .unwrap();
            } else if stock {
                // Leave the configured route attached but unusable. Services never resolve it.
                tx.execute(
                    "UPDATE stock_rule SET active = false WHERE warehouse_id IN
                     (SELECT id FROM stock_warehouse WHERE company_id = $1)",
                    &[&world.company],
                )
                .await
                .unwrap();
            }
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
            order
                .call::<(), ()>(&mut tx, "action_confirm", ())
                .await
                .unwrap();
            assert_eq!(text(&mut tx, &order, "state").await, "sale");
            if stock {
                assert!(orm.registry().model("sale.stock.delivery.link").is_some());
                assert_eq!(transfers(&mut tx, &reference).await, 0);
            } else {
                assert!(orm.registry().model("sale.stock.delivery.link").is_none());
            }
            tx.rollback().await.unwrap();
            drop(tx);
            db.cleanup().await.unwrap();
        }
    });
}

#[test]
fn upgrading_installed_sales_and_stock_leaves_bridge_for_explicit_install() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install_sales(&db, true).await;
        assert!(
            installed
                .registry()
                .model("sale.stock.delivery.link")
                .is_some()
        );
        drop(installed);

        let system = system_with_stock(&db, true);
        let env = system.system_env(Context::new());
        let bridge = [ModuleName::new("sale_stock").unwrap()];
        let removed = Box::pin(system.button_immediate_uninstall(&env, &bridge))
            .await
            .unwrap();
        assert!(removed.post_commit.is_clean(), "{:?}", removed.post_commit);
        assert!(
            removed
                .orm
                .registry()
                .model("sale.stock.delivery.link")
                .is_none()
        );
        let mut tx = removed.orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let goods = product(&mut tx, "Existing installation item", "consu").await;
        let order = new_order(&mut tx, &world).await;
        let order_id = order.ids()[0];
        let reference = text(&mut tx, &order, "name").await;
        tx.env()
            .records("sale.order.line")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("order_id", Cell::Id(order_id))
                        .set_cell("product_id", Cell::Id(goods))
                        .set_cell("product_uom_qty", Cell::Decimal(Decimal::ONE)),
                ],
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        drop(removed);

        let modules = ["sale", "stock", "stock_account"].map(|name| ModuleName::new(name).unwrap());
        let upgraded = Box::pin(system.button_immediate_upgrade(&env, &modules))
            .await
            .unwrap();
        assert!(
            upgraded.post_commit.is_clean(),
            "{:?}",
            upgraded.post_commit
        );
        assert!(
            upgraded
                .orm
                .registry()
                .model("sale.stock.delivery.link")
                .is_none()
        );
        drop(upgraded);

        let installed = Box::pin(system.button_immediate_install(&env, &bridge))
            .await
            .unwrap();
        assert!(
            installed.post_commit.is_clean(),
            "{:?}",
            installed.post_commit
        );
        assert!(
            installed
                .orm
                .registry()
                .model("sale.stock.delivery.link")
                .is_some()
        );
        let mut tx = installed.orm.begin(SUPERUSER_ID).await.unwrap();
        let order = tx
            .env()
            .records("sale.order")
            .unwrap()
            .browse([order_id])
            .unwrap();
        assert_eq!(text(&mut tx, &order, "state").await, "draft");
        order
            .call::<(), ()>(&mut tx, "action_confirm", ())
            .await
            .unwrap();
        assert_eq!(transfers(&mut tx, &reference).await, 1);
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn salesperson_selects_the_warehouse_when_configured_routes_are_ambiguous() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install_sales(&db, true).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        admin(&mut tx).await;
        let salesperson = user(&mut tx, "delivery-warehouse-sales", "group_sale_salesman").await;
        let world = world(&mut tx).await;
        let original = tx
            .query(
                "SELECT id FROM stock_warehouse
             WHERE company_id = $1 AND active AND delivery_route_id IS NOT NULL",
                &[&world.company],
            )
            .await
            .unwrap()[0]
            .get::<_, i64>(0);
        let warehouse = tx
            .env()
            .with_company(world.company)
            .unwrap()
            .records("stock.warehouse")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Second Shipping Warehouse".into()))
                        .set_cell("code", Cell::Str("SHP2".into()))
                        .set_cell("company_id", Cell::Id(world.company)),
                ],
            )
            .await
            .unwrap();
        let warehouse_id = warehouse.ids()[0];
        let operation = many2one(&mut tx, &warehouse, "out_type_id").await.unwrap();
        let goods = product(&mut tx, "Warehouse choice goods", "consu").await;
        let (order_id, _, reference) =
            order_with_product(&mut tx, &world, salesperson, goods, RawVals::new()).await;
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
            ErrorKind::UserError,
            "Choose the sales order's warehouse when its company does not have exactly one configured delivery warehouse.",
        );
        assert_eq!(text(&mut tx, &order, "state").await, "draft");
        assert_eq!(transfers(&mut tx, &reference).await, 0);
        write(
            &mut tx,
            &order,
            RawVals::new().set_cell("warehouse_id", Cell::Id(warehouse_id)),
        )
        .await
        .unwrap();
        assert_eq!(
            many2one(&mut tx, &order, "warehouse_id").await,
            Some(warehouse_id)
        );
        order
            .call::<(), ()>(&mut tx, "action_confirm", ())
            .await
            .unwrap();
        refused_exact(
            order
                .write(
                    &mut tx,
                    RawVals::new().set_cell("warehouse_id", Cell::Id(original)),
                )
                .await,
            ErrorKind::UserError,
            "The sales warehouse can be changed only on an editable quotation without a delivery.",
        );
        assert_eq!(
            many2one(&mut tx, &order, "warehouse_id").await,
            Some(warehouse_id)
        );
        let row = tx
            .query(
                "SELECT picking_type_id FROM stock_picking WHERE origin = $1",
                &[&reference],
            )
            .await
            .unwrap();
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].get::<_, i64>(0), operation);
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn generated_shipments_ignore_saved_inventory_defaults() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install_sales(&db, true).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        admin(&mut tx).await;
        let salesperson = user(&mut tx, "delivery-default-sales", "group_sale_salesman").await;
        let inventory = user(&mut tx, "delivery-default-stock", "group_sale_salesman").await;
        let inventory_group = xmlid(&mut tx, "stock", "group_stock_user").await;
        let operator = tx
            .env()
            .records("res.users")
            .unwrap()
            .browse([inventory])
            .unwrap();
        write(
            &mut tx,
            &operator,
            RawVals::new().set_commands("group_ids", [Command::link(id(inventory_group))]),
        )
        .await
        .unwrap();
        let world = world(&mut tx).await;
        let owner = partner(
            &mut tx,
            RawVals::new().set_cell("name", Cell::Str("Default stock owner".into())),
        )
        .await;
        let goods = product(&mut tx, "Default boundary goods", "consu").await;
        let (order_id, _, reference) =
            order_with_product(&mut tx, &world, salesperson, goods, RawVals::new()).await;
        let row = tx
            .query(
                "SELECT w.lot_stock_id, t.id, t.default_location_dest_id
             FROM stock_warehouse w JOIN stock_picking_type t ON t.id = w.out_type_id
             WHERE w.company_id = $1",
                &[&world.company],
            )
            .await
            .unwrap();
        let stock_location = row[0].get::<_, i64>(0);
        let common = RawVals::new()
            .set_cell("company_id", Cell::Id(world.company))
            .set_cell("location_id", Cell::Id(stock_location))
            .set_cell("picking_type_id", Cell::Id(row[0].get(1)))
            .set_cell("location_dest_id", Cell::Id(row[0].get(2)));
        let move_vals = common
            .clone()
            .set_cell("product_id", Cell::Id(goods))
            .set_cell("product_uom_qty", Cell::Decimal(Decimal::ONE));
        let scope = tx.env().with_company(world.company).unwrap();
        let earlier_move = scope
            .records("stock.move")
            .unwrap()
            .create(&mut tx, vec![move_vals.clone()])
            .await
            .unwrap()
            .ids()[0];
        let package = scope
            .records("stock.package")
            .unwrap()
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", Cell::Str("Default result package".into()))],
            )
            .await
            .unwrap()
            .ids()[0];
        use erp_addon_stock::quant_engine::{QuantChange, QuantKey, apply_quant_changes};
        apply_quant_changes(
            &mut tx,
            &[QuantChange::new(
                QuantKey::new(world.company, goods, stock_location),
                Decimal::from(5),
                Decimal::ZERO,
            )],
        )
        .await
        .unwrap();
        for (model, field, value) in [
            ("stock.picking", "owner_id", Value::Int(owner)),
            (
                "stock.move",
                "origin_returned_move_id",
                Value::Int(earlier_move),
            ),
            ("stock.move", "restrict_partner_id", Value::Int(owner)),
            ("stock.move", "price_unit", Value::Int(17)),
            ("stock.move.line", "result_package_id", Value::Int(package)),
        ] {
            erp_orm::ir_default::set(
                &mut tx,
                &scope,
                model,
                field,
                &value,
                None,
                Some(id(world.company)),
                None,
            )
            .await
            .unwrap();
        }
        erp_orm::ir_default::set(
            &mut tx,
            &scope,
            "stock.move",
            "price_unit",
            &Value::Int(29),
            Some(id(salesperson)),
            Some(id(world.company)),
            None,
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
        order
            .call::<(), ()>(&mut tx, "action_confirm", ())
            .await
            .unwrap();
        let rows = tx
            .query(
                "SELECT p.owner_id, m.origin_returned_move_id, m.restrict_partner_id, m.price_unit,
                    l.quantity, l.result_package_id
             FROM stock_picking p JOIN stock_move m ON m.picking_id = p.id
             LEFT JOIN stock_move_line l ON l.move_id = m.id WHERE p.origin = $1",
                &[&reference],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, Option<i64>>(0), None, "shipment owner");
        assert_eq!(rows[0].get::<_, Option<i64>>(1), None, "return origin");
        assert_eq!(rows[0].get::<_, Option<i64>>(2), None, "owner restriction");
        assert_eq!(
            rows[0].get::<_, Option<Decimal>>(3),
            None,
            "manually set stock price"
        );
        assert_eq!(
            rows[0].get::<_, Option<Decimal>>(4),
            Some(Decimal::from(3)),
            "normal reservation"
        );
        assert_eq!(
            rows[0].get::<_, Option<i64>>(5),
            None,
            "reservation package"
        );
        tx.commit().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(UserId::new(inventory).unwrap()).await.unwrap();
        let scope = tx.env().with_company(world.company).unwrap();
        let picking = scope
            .records("stock.picking")
            .unwrap()
            .create(&mut tx, vec![common])
            .await
            .unwrap();
        assert_eq!(many2one(&mut tx, &picking, "owner_id").await, Some(owner));
        let movement = scope
            .records("stock.move")
            .unwrap()
            .create(
                &mut tx,
                vec![move_vals.set_cell("picking_id", Cell::Id(picking.ids()[0]))],
            )
            .await
            .unwrap();
        assert_eq!(
            many2one(&mut tx, &movement, "origin_returned_move_id").await,
            Some(earlier_move)
        );
        assert_eq!(
            many2one(&mut tx, &movement, "restrict_partner_id").await,
            Some(owner)
        );
        assert_eq!(
            movement.cell(&mut tx, "price_unit").await.unwrap(),
            Cell::Decimal(Decimal::from(17))
        );
        let line_defaults = scope
            .records("stock.move.line")
            .unwrap()
            .default_get(&mut tx, &["result_package_id"])
            .await
            .unwrap();
        assert_eq!(
            line_defaults.get("result_package_id"),
            Some(&Value::Int(package))
        );
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}
