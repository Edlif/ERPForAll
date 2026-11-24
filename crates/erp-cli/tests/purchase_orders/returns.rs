// SPDX-License-Identifier: MIT OR Apache-2.0
//! Return an actual vendor receipt through installed Purchase and Stock actions.

use super::{Catalog, act, catalog, dec, install_module, line_vals, order, run, text, user, xmlid};
use erp_orm::{FieldAccessImpl, FieldOp, Operation, SUPERUSER_ID};
use erp_sdk::prelude::*;
use erp_sdk::{Decimal, ErrorKind};
use erp_testkit::TestDb;

#[test]
fn actual_vendor_receipt_returns_only_its_own_completed_quantity() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "purchase_stock").await;
        let mut setup = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut setup, "base", "main_company").await;
        let catalog = catalog(&mut setup, company).await;
        let product = setup
            .env()
            .records("product.product")
            .unwrap()
            .browse([catalog.bolt])
            .unwrap();
        product
            .write(
                &mut setup,
                RawVals::new().set_cell("is_storable", Cell::Bool(true)),
            )
            .await
            .unwrap();
        let system_group = xmlid(&mut setup, "base", "group_system").await;
        user(
            &mut setup,
            "return-fixture-admin",
            &[system_group],
            &[company],
        )
        .await;
        let currency: i64 = setup
            .query(
                "SELECT currency_id FROM res_company WHERE id=$1",
                &[&company],
            )
            .await
            .unwrap()[0]
            .get(0);
        let other_company = setup
            .env()
            .records("res.company")
            .unwrap()
            .create(
                &mut setup,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Other Return Company"))
                        .set_cell("currency_id", Cell::Id(currency)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let group = xmlid(&mut setup, "stock", "group_stock_user").await;
        let clerk = user(&mut setup, "return-stock-clerk", &[group], &[company]).await;
        let second_clerk = user(
            &mut setup,
            "second-return-stock-clerk",
            &[group],
            &[company],
        )
        .await;
        let outsider = user(&mut setup, "other-return-clerk", &[group], &[other_company]).await;
        let receipts = [
            receive(&mut setup, company, &catalog, "2", true).await,
            receive(&mut setup, company, &catalog, "1", false).await,
        ];
        setup.commit().await.unwrap();
        drop(setup);

        let mut tx = orm.begin(clerk).await.unwrap();
        let original = picking(&tx, receipts[0]);
        let row = &tx
            .query(
                "SELECT p.location_id,p.location_dest_id,t.return_picking_type_id,m.id
               FROM stock_picking p JOIN stock_picking_type t ON t.id=p.picking_type_id
               JOIN stock_move m ON m.picking_id=p.id WHERE p.id=$1 AND m.is_scrap IS NOT TRUE",
                &[&receipts[0]],
            )
            .await
            .unwrap()[0];
        let (vendor_location, stock, return_type, original_move): (i64, i64, i64, i64) =
            (row.get(0), row.get(1), row.get(2), row.get(3));
        assert_eq!(
            on_hand(&mut tx, company, stock, catalog.bolt).await,
            dec("36")
        );

        let partial = new_return(&mut tx, &original).await;

        let mut foreign = orm.begin(outsider).await.unwrap();
        let denied = picking(&foreign, receipts[0])
            .call::<(), Value>(&mut foreign, "action_return", ())
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Access, "{denied:?}");
        foreign.rollback().await.unwrap();
        drop(foreign);

        let moves = tx.query(
            "SELECT p.state,p.return_id,p.location_id,p.location_dest_id,p.picking_type_id,p.partner_id,
                    m.origin_returned_move_id,m.uom_id,m.product_uom_qty, EXISTS (
                        SELECT 1 FROM stock_move_move_rel link
                         WHERE link.move_dest_id=m.id AND link.move_orig_id=m.origin_returned_move_id
                    )
               FROM stock_picking p JOIN stock_move m ON m.picking_id=p.id WHERE p.id=$1",
            &[&partial],
        ).await.unwrap();
        assert_eq!(moves.len(), 1, "a completed scrap move is not returnable");
        let row = &moves[0];
        assert_eq!(row.get::<_, String>(0), "draft");
        assert_eq!(row.get::<_, i64>(1), receipts[0]);
        assert_eq!(row.get::<_, i64>(2), stock);
        assert_eq!(row.get::<_, i64>(3), vendor_location);
        assert_eq!(row.get::<_, i64>(4), return_type);
        assert_eq!(row.get::<_, i64>(5), catalog.vendor);
        assert_eq!(row.get::<_, i64>(6), original_move);
        assert_eq!(row.get::<_, i64>(7), catalog.dozen);
        assert_eq!(row.get::<_, Decimal>(8), dec("0"));
        assert!(row.get::<_, bool>(9));

        let partial_picking = picking(&tx, partial);
        partial_picking
            .call::<(), ()>(&mut tx, "action_return_all", ())
            .await
            .unwrap();
        assert_eq!(demand(&mut tx, partial).await, dec("2"));
        partial_picking
            .call::<(), ()>(&mut tx, "action_clear_return", ())
            .await
            .unwrap();
        assert_eq!(demand(&mut tx, partial).await, dec("0"));
        set_quantity(&mut tx, partial, "1").await;
        partial_picking
            .call::<(), ()>(&mut tx, "action_assign", ())
            .await
            .unwrap();
        let exchange = transfer_action(&mut tx, &partial_picking, "action_exchange").await;
        let row = &tx
            .query(
                "SELECT p.return_id, p.location_id, p.location_dest_id, m.state, m.uom_id,
                    m.origin_returned_move_id, m.product_uom_qty, EXISTS (
                        SELECT 1 FROM stock_move_move_rel r WHERE r.move_dest_id=m.id
                    )
               FROM stock_picking p JOIN stock_move m ON m.picking_id=p.id WHERE p.id=$1",
                &[&exchange],
            )
            .await
            .unwrap()[0];
        assert_eq!(row.get::<_, i64>(0), partial);
        assert_eq!(row.get::<_, i64>(1), vendor_location);
        assert_eq!(row.get::<_, i64>(2), stock);
        assert_eq!(row.get::<_, String>(3), "assigned");
        assert_eq!(row.get::<_, i64>(4), catalog.dozen);
        assert_eq!(row.get::<_, Option<i64>>(5), None);
        assert_eq!(row.get::<_, Decimal>(6), dec("1"));
        assert!(!row.get::<_, bool>(7));
        assert_eq!(
            original.cell(&mut tx, "return_count").await.unwrap(),
            Cell::Int(1)
        );
        assert_eq!(
            partial_picking.cell(&mut tx, "return_count").await.unwrap(),
            Cell::Int(1)
        );
        validate(&mut tx, partial).await.unwrap();
        assert_eq!(
            on_hand(&mut tx, company, stock, catalog.bolt).await,
            dec("24")
        );

        let first = new_return(&mut tx, &original).await;
        let second = new_return(&mut tx, &original).await;
        let first_picking = picking(&tx, first);
        first_picking
            .call::<(), ()>(&mut tx, "action_return_all", ())
            .await
            .unwrap();
        assert_eq!(demand(&mut tx, first).await, dec("1"));
        set_quantity(&mut tx, second, "1").await;
        tx.commit().await.unwrap();
        drop(tx);

        // The overlap is between two different return transfers. Their only shared origin is
        // the first receipt. The unrelated receipt leaves physical headroom for a wrong double return.
        let mut later = orm.begin(clerk).await.unwrap();
        assert_eq!(
            on_hand(&mut later, company, stock, catalog.bolt).await,
            dec("24")
        );
        let mut earlier = orm.begin(second_clerk).await.unwrap();
        validate(&mut earlier, first).await.unwrap();
        earlier.commit().await.unwrap();
        drop(earlier);
        let stale = validate(&mut later, second).await.unwrap_err();
        assert_eq!(stale.kind(), ErrorKind::Concurrency, "{stale:?}");
        assert_eq!(stale.sqlstate(), Some("40001"));
        later.rollback().await.unwrap();
        drop(later);

        let mut retry = orm.begin(clerk).await.unwrap();
        let error = validate(&mut retry, second).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(
            error.message(),
            "The return exceeds the quantity completed on its source transfer."
        );
        assert_eq!(
            on_hand(&mut retry, company, stock, catalog.bolt).await,
            dec("12")
        );
        let done: Vec<String> = retry
            .query(
                "SELECT p.state FROM stock_picking p WHERE p.return_id=$1 ORDER BY p.id",
                &[&receipts[0]],
            )
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get(0))
            .collect();
        assert_eq!(done, ["done", "done", "draft"]);
        validate(&mut retry, exchange).await.unwrap();
        assert_eq!(
            on_hand(&mut retry, company, stock, catalog.bolt).await,
            dec("24")
        );
        let downstream = reserve_successor(&mut retry, receipts[1], return_type, &catalog).await;
        assert_eq!(
            downstream.cell(&mut retry, "quantity").await.unwrap(),
            Cell::Decimal(dec("1"))
        );
        let remaining_receipt = picking(&retry, receipts[1]);
        new_return(&mut retry, &remaining_receipt).await;
        assert_eq!(
            downstream.cell(&mut retry, "quantity").await.unwrap(),
            Cell::Decimal(dec("0"))
        );
        assert_eq!(
            on_hand(&mut retry, company, stock, catalog.bolt).await,
            dec("24")
        );
        retry.rollback().await.unwrap();
        drop(retry);
        db.cleanup().await.unwrap();
    });
}

// The installed policy compiler accepts the same field restrictions as an addon extension.
static HIDE_RETURN_ROUTE: FieldAccessImpl = FieldAccessImpl {
    addon: "stock",
    model: "stock.picking.type",
    method: "hide_return_route",
    context: &[],
    run: |ctx, op, mask, sup| {
        let mut mask = sup.call(ctx, op, mask)?;
        mask.deny(ctx.field("return_picking_type_id")?, FieldOp::Read)?;
        Ok(mask)
    },
    file: file!(),
    line: line!(),
};

#[derive(Clone, Copy)]
enum HiddenRoute {
    Model,
    Record,
    Field,
}

#[test]
fn return_route_refuses_unreadable_operation_type_model() {
    run(check_hidden_route(HiddenRoute::Model));
}

#[test]
fn return_route_refuses_unreadable_original_type() {
    run(check_hidden_route(HiddenRoute::Record));
}

#[test]
fn return_route_refuses_unreadable_configuration_field() {
    run(check_hidden_route(HiddenRoute::Field));
}

async fn check_hidden_route(hidden: HiddenRoute) {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let installed = install_module(&db, "purchase_stock").await;
    let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
    let company = xmlid(&mut tx, "base", "main_company").await;
    let catalog = catalog(&mut tx, company).await;
    let env = tx.env();
    env.records("product.product")
        .unwrap()
        .browse([catalog.bolt])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("is_storable", Cell::Bool(true)),
        )
        .await
        .unwrap();
    let admin = xmlid(&mut tx, "base", "group_system").await;
    user(&mut tx, "return-route-admin", &[admin], &[company]).await;
    let group = xmlid(&mut tx, "stock", "group_stock_user").await;
    let clerk = user(&mut tx, "return-route-clerk", &[group], &[company]).await;
    let receipt = receive(&mut tx, company, &catalog, "1", false).await;
    let row = &tx
        .query(
            "SELECT t.id,t.return_picking_type_id,r.sequence_id FROM stock_picking p
           JOIN stock_picking_type t ON t.id=p.picking_type_id
           JOIN stock_picking_type r ON r.id=t.return_picking_type_id WHERE p.id=$1",
            &[&receipt],
        )
        .await
        .unwrap()[0];
    let (original, target, sequence): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
    assert_ne!(original, target);
    let downstream = reserve_successor(&mut tx, receipt, target, &catalog)
        .await
        .ids()[0];
    let access = env.records("ir.access").unwrap();
    match hidden {
        HiddenRoute::Model => {
            let grant = xmlid(&mut tx, "stock", "access_stock_picking_type_employee").await;
            access
                .browse([grant])
                .unwrap()
                .write(
                    &mut tx,
                    RawVals::new().set_cell("active", Cell::Bool(false)),
                )
                .await
                .unwrap();
        }
        HiddenRoute::Record => {
            let model = xmlid(&mut tx, "stock", "model_stock_picking_type").await;
            access
                .create(
                    &mut tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", text("Hide the receipt operation type"))
                            .set_cell("model_id", Cell::Id(model))
                            .set_cell("operation", text("r"))
                            .set_cell("domain", text(&format!("[('id', '!=', {original})]"))),
                    ],
                )
                .await
                .unwrap();
        }
        HiddenRoute::Field => {}
    }
    tx.commit().await.unwrap();
    drop(tx);
    let overrides: &[&FieldAccessImpl] = if matches!(hidden, HiddenRoute::Field) {
        &[&HIDE_RETURN_ROUTE]
    } else {
        &[]
    };
    let orm = super::purchase_policy_orm(&db, &installed, overrides).await;
    let mut tx = orm.begin(clerk).await.unwrap();
    let types = tx.env().records("stock.picking.type").unwrap();
    if !matches!(hidden, HiddenRoute::Model) {
        types
            .browse([target])
            .unwrap()
            .check_access(&mut tx, Operation::Read)
            .await
            .unwrap();
    }
    let source_type = types.browse([original]).unwrap();
    assert_eq!(
        source_type
            .cell(&mut tx, "return_picking_type_id")
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Access
    );
    // A standard reference sequence advances outside transaction rollback.
    let sequence_state = format!("SELECT last_value,is_called FROM ir_sequence_{sequence:03}");
    let before = &tx.query(&sequence_state, &[]).await.unwrap()[0];
    let result = picking(&tx, receipt)
        .call::<(), Value>(&mut tx, "action_return", ())
        .await;
    assert_eq!(result.unwrap_err().kind(), ErrorKind::Access);
    let after = &tx.query(&sequence_state, &[]).await.unwrap()[0];
    assert_eq!(
        (before.get::<_, i64>(0), before.get::<_, bool>(1)),
        (after.get::<_, i64>(0), after.get::<_, bool>(1))
    );
    let effects = &tx.query(
        "SELECT (SELECT count(*) FROM stock_picking WHERE return_id=$1),
                (SELECT count(*) FROM stock_move m JOIN stock_move src ON src.id=m.origin_returned_move_id WHERE src.picking_id=$1),
                (SELECT quantity FROM stock_move WHERE id=$2)",
        &[&receipt,&downstream],
    ).await.unwrap()[0];
    assert_eq!(effects.get::<_, i64>(0), 0);
    assert_eq!(effects.get::<_, i64>(1), 0);
    assert_eq!(effects.get::<_, Decimal>(2), dec("1"));
    tx.rollback().await.unwrap();
    drop(tx);
    db.cleanup().await.unwrap();
}

async fn reserve_successor(
    tx: &mut Transaction,
    receipt: i64,
    operation: i64,
    catalog: &Catalog,
) -> Records {
    let row = &tx
        .query(
            "SELECT m.id, t.default_location_src_id, t.default_location_dest_id
           FROM stock_move m CROSS JOIN stock_picking_type t
          WHERE m.picking_id=$1 AND m.is_scrap IS NOT TRUE AND t.id=$2",
            &[&receipt, &operation],
        )
        .await
        .unwrap()[0];
    let parent: i64 = row.get(0);
    let outgoing = tx
        .env()
        .records("stock.picking")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("picking_type_id", Cell::Id(operation))
                    .set_cell("location_id", Cell::Id(row.get(1)))
                    .set_cell("location_dest_id", Cell::Id(row.get(2))),
            ],
        )
        .await
        .unwrap();
    let movement = tx
        .env()
        .records("stock.move")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("picking_id", Cell::Id(outgoing.ids()[0]))
                    .set_cell("product_id", Cell::Id(catalog.bolt))
                    .set_cell("uom_id", Cell::Id(catalog.dozen))
                    .set_cell("product_uom_qty", Cell::Decimal(dec("1"))),
            ],
        )
        .await
        .unwrap();
    outgoing
        .call::<(), ()>(tx, "action_assign", ())
        .await
        .unwrap();
    movement
        .write(
            tx,
            RawVals::new().set_commands(
                "move_orig_ids",
                [Command::Link(RecId::from_real(parent).unwrap())],
            ),
        )
        .await
        .unwrap();
    movement
}

async fn receive(
    tx: &mut Transaction,
    company: i64,
    catalog: &Catalog,
    quantity: &str,
    with_scrap: bool,
) -> i64 {
    let source = order(
        tx,
        &[company],
        RawVals::new()
            .set_cell("partner_id", Cell::Id(catalog.vendor))
            .set_commands(
                "order_line",
                [Command::Create(
                    line_vals(catalog.bolt)
                        .set_cell("uom_id", Cell::Id(catalog.dozen))
                        .set_cell("product_qty", Cell::Decimal(dec(quantity))),
                )],
            ),
    )
    .await;
    act(tx, &source, "button_confirm").await.unwrap();
    let picking_id = tx
        .query(
            "SELECT picking_id FROM purchase_stock_receipt_link WHERE order_id=$1",
            &[&source.ids()[0]],
        )
        .await
        .unwrap()[0]
        .get(0);
    let scrap = if with_scrap {
        Some(add_receipt_scrap(tx, company, catalog, picking_id).await)
    } else {
        None
    };
    let purchase_line = super::lines_of(tx, &[company], &source).await.remove(0);
    assert_eq!(
        purchase_line.cell(tx, "qty_received_method").await.unwrap(),
        text("stock_moves")
    );
    assert_eq!(
        purchase_line.cell(tx, "qty_received").await.unwrap(),
        Cell::Decimal(dec("0"))
    );
    validate(tx, picking_id).await.unwrap();
    assert_eq!(
        purchase_line.cell(tx, "qty_received").await.unwrap(),
        Cell::Decimal(dec(quantity))
    );
    let replay = validate(tx, picking_id).await.unwrap_err();
    assert!(replay.message().contains("validated earlier"), "{replay:?}");
    assert_eq!(
        purchase_line.cell(tx, "qty_received").await.unwrap(),
        Cell::Decimal(dec(quantity))
    );
    if let Some(scrap) = scrap {
        assert_eq!(scrap.cell(tx, "state").await.unwrap(), text("done"));
    }
    picking_id
}

async fn add_receipt_scrap(
    tx: &mut Transaction,
    company: i64,
    catalog: &Catalog,
    picking: i64,
) -> Records {
    let scrap_location = tx
        .env()
        .records("stock.location")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Receipt Scrap"))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("usage", text("inventory")),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    let scrap = tx
        .env()
        .records("stock.move")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("picking_id", Cell::Id(picking))
                    .set_cell("product_id", Cell::Id(catalog.bolt))
                    .set_cell("uom_id", Cell::Id(catalog.dozen))
                    .set_cell("product_uom_qty", Cell::Decimal(dec("1")))
                    .set_cell("location_dest_id", Cell::Id(scrap_location))
                    .set_cell("is_scrap", Cell::Bool(true)),
            ],
        )
        .await
        .unwrap();
    tx.env()
        .records("stock.move.line")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("move_id", Cell::Id(scrap.ids()[0]))
                    .set_cell("quantity", Cell::Decimal(dec("1"))),
            ],
        )
        .await
        .unwrap();
    scrap
}

fn picking(tx: &Transaction, id: i64) -> Records {
    tx.env()
        .records("stock.picking")
        .unwrap()
        .browse([id])
        .unwrap()
}

async fn new_return(tx: &mut Transaction, original: &Records) -> i64 {
    transfer_action(tx, original, "action_return").await
}

async fn transfer_action(tx: &mut Transaction, source: &Records, method: &'static str) -> i64 {
    let action = source.call::<(), Value>(tx, method, ()).await.unwrap();
    let Value::Map(action) = action else {
        panic!("expected a native transfer window action")
    };
    assert_eq!(action["type"], Value::str("ir.actions.act_window"));
    assert_eq!(action["res_model"], Value::str("stock.picking"));
    match action.get("res_id") {
        Some(Value::Int(id)) => *id,
        _ => panic!("the native transfer action must contain a record ID"),
    }
}

pub(super) async fn validate(tx: &mut Transaction, id: i64) -> Result<()> {
    picking(tx, id).call(tx, "button_validate", ()).await
}

async fn demand(tx: &mut Transaction, picking_id: i64) -> Decimal {
    tx.query(
        "SELECT product_uom_qty FROM stock_move WHERE picking_id=$1",
        &[&picking_id],
    )
    .await
    .unwrap()[0]
        .get(0)
}

async fn set_quantity(tx: &mut Transaction, picking_id: i64, value: &str) {
    let id: i64 = tx
        .query(
            "SELECT id FROM stock_move WHERE picking_id=$1",
            &[&picking_id],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.env()
        .records("stock.move")
        .unwrap()
        .browse([id])
        .unwrap()
        .write(
            tx,
            RawVals::new().set_cell("product_uom_qty", Cell::Decimal(dec(value))),
        )
        .await
        .unwrap();
}

async fn on_hand(tx: &mut Transaction, company: i64, location: i64, product: i64) -> Decimal {
    tx.query(
        "SELECT COALESCE(SUM(quantity),0) FROM stock_quant WHERE company_id=$1 AND location_id=$2 AND product_id=$3",
        &[&company, &location, &product],
    ).await.unwrap()[0].get(0)
}

#[test]
fn a_short_direct_receipt_counts_completed_goods_in_the_purchase_unit() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "purchase_stock").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let catalog = catalog(&mut tx, company).await;
        let admin_group = xmlid(&mut tx, "base", "group_system").await;
        user(&mut tx, "quantity-admin", &[admin_group], &[company]).await;
        let stock_group = xmlid(&mut tx, "stock", "group_stock_user").await;
        let clerk = user(&mut tx, "quantity-clerk", &[stock_group], &[company]).await;
        let source = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(catalog.vendor))
                .set_commands(
                    "order_line",
                    [Command::Create(
                        line_vals(catalog.bolt)
                            .set_cell("uom_id", Cell::Id(catalog.dozen))
                            .set_cell("product_qty", Cell::Decimal(dec("2"))),
                    )],
                ),
        )
        .await;
        act(&mut tx, &source, "button_confirm").await.unwrap();
        let line = super::lines_of(&mut tx, &[company], &source)
            .await
            .remove(0);
        let row = &tx
            .query(
                "SELECT m.picking_id, m.id, m.picking_type_id
             FROM purchase_stock_receipt_move_link ml JOIN stock_move m ON m.id=ml.move_id
             WHERE ml.purchase_line_id=$1",
                &[&line.ids()[0]],
            )
            .await
            .unwrap()[0];
        let (receipt, mov, operation): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
        let move_lines = tx.env().records("stock.move.line").unwrap();
        let existing = tx
            .query("SELECT id FROM stock_move_line WHERE move_id=$1", &[&mov])
            .await
            .unwrap();
        let quantity = RawVals::new().set_cell("quantity", Cell::Decimal(dec("1")));
        if let Some(row) = existing.first() {
            assert_eq!(existing.len(), 1);
            move_lines
                .browse([row.get(0)])
                .unwrap()
                .write(&mut tx, quantity)
                .await
                .unwrap();
        } else {
            move_lines
                .create(&mut tx, vec![quantity.set_cell("move_id", Cell::Id(mov))])
                .await
                .unwrap();
        }
        let id = line.ids()[0];
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(clerk).await.unwrap();
        let err = validate(&mut tx, receipt).await.unwrap_err();
        assert!(
            err.message().to_lowercase().contains("backorder"),
            "{err:?}"
        );
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let line = super::lines(&tx, &[company]).browse([id]).unwrap();
        assert_eq!(
            line.cell(&mut tx, "qty_received").await.unwrap(),
            Cell::Decimal(dec("0"))
        );
        tx.env()
            .records("stock.picking.type")
            .unwrap()
            .browse([operation])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("create_backorder", text("never")),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(clerk).await.unwrap();
        validate(&mut tx, receipt).await.unwrap();
        tx.commit().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let line = super::lines(&tx, &[company]).browse([id]).unwrap();
        assert_eq!(
            line.cell(&mut tx, "product_qty").await.unwrap(),
            Cell::Decimal(dec("2"))
        );
        assert_eq!(
            line.cell(&mut tx, "qty_received").await.unwrap(),
            Cell::Decimal(dec("1"))
        );
        // received goods keep the line's unit
        let unit = super::write(
            &mut tx,
            &line,
            RawVals::new().set_cell("uom_id", Cell::Id(catalog.unit)),
        )
        .await
        .unwrap_err();
        assert_eq!(unit.kind(), ErrorKind::UserError, "{unit:?}");
        assert!(
            unit.message()
                .contains("product and unit can no longer change"),
            "{unit:?}"
        );
        // Model the previous installed version: its completed receipt exists but the quantity
        // was always zero. Upgrade repeatedly through normal module lifecycle.
        tx.execute("UPDATE purchase_order_line SET qty_received=0, qty_received_method='manual' WHERE id=$1", &[&id]).await.unwrap();
        tx.execute(
            "UPDATE ir_module_module SET latest_version='1.1.1' WHERE name='purchase_stock'",
            &[],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        drop(orm);

        let system = super::purchase_module_system(&db);
        let modules = [super::ModuleName::new("purchase_stock").unwrap()];
        for _ in 0..2 {
            let upgraded = Box::pin(
                system
                    .button_immediate_upgrade(&system.system_env(super::Context::new()), &modules),
            )
            .await
            .unwrap();
            assert!(
                upgraded.post_commit.is_clean(),
                "{:?}",
                upgraded.post_commit
            );
            let mut tx = upgraded.orm.begin(SUPERUSER_ID).await.unwrap();
            let line = tx
                .env()
                .records("purchase.order.line")
                .unwrap()
                .browse([id])
                .unwrap();
            assert_eq!(
                line.cell(&mut tx, "qty_received_method").await.unwrap(),
                text("stock_moves")
            );
            assert_eq!(
                line.cell(&mut tx, "qty_received").await.unwrap(),
                Cell::Decimal(dec("1"))
            );
            tx.rollback().await.unwrap();
        }
        drop(system);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn a_direct_vendor_bill_traces_the_received_purchase_line() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "purchase_stock").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let system = xmlid(&mut tx, "base", "group_system").await;
        user(&mut tx, "bill-admin", &[system], &[company]).await;
        let buyers = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let buyer = user(&mut tx, "bill-buyer", &[buyers], &[company]).await;
        let accounting = xmlid(&mut tx, "account", "group_account_user").await;
        let accountant = user(&mut tx, "bill-accountant", &[accounting], &[company]).await;
        let clerk = user(
            &mut tx,
            "purchase-accounting-clerk",
            &[buyers, accounting],
            &[company],
        )
        .await;
        let books = super::bill_books::books(&mut tx, company).await;
        let k = catalog(&mut tx, company).await;
        let item = |qty: &str, price: &str, taxes: Vec<RecId>| {
            Command::Create(
                line_vals(k.bolt)
                    .set_cell("product_qty", Cell::Decimal(dec(qty)))
                    .set_cell("price_unit", Cell::Decimal(dec(price)))
                    .set_cell("uom_id", Cell::Id(k.dozen))
                    .set_commands("tax_ids", [Command::Set(taxes)]),
            )
        };
        let order = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(k.vendor))
                .set_commands(
                    "order_line",
                    [item("2", "24", Vec::new()), item("1", "36", vec![k.tax])],
                ),
        )
        .await;
        let po = order.ids()[0];
        let draft = order
            .call::<(), Value>(&mut tx, "action_create_vendor_bill", ())
            .await
            .unwrap_err();
        assert_eq!(draft.kind(), ErrorKind::UserError);
        assert!(draft.message().contains("approve"), "{draft:?}");
        act(&mut tx, &order, "button_confirm").await.unwrap();
        let empty = order
            .call::<(), Value>(&mut tx, "action_create_vendor_bill", ())
            .await
            .unwrap_err();
        assert_eq!(empty.kind(), ErrorKind::UserError);
        assert!(empty.message().contains("Receive goods"), "{empty:?}");
        let picking: i64 = tx
            .query(
                "SELECT picking_id FROM purchase_stock_receipt_link WHERE order_id=$1",
                &[&po],
            )
            .await
            .unwrap()[0]
            .get(0);
        validate(&mut tx, picking).await.unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(buyer).await.unwrap();
        let denied = super::orders(&tx, &[company])
            .browse([po])
            .unwrap()
            .call::<(), Value>(&mut tx, "action_create_vendor_bill", ())
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Access, "{denied:?}");
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let po = super::orders(&tx, &[company]).browse([po]).unwrap();
        let parameters = tx.env().records("ir.config_parameter").unwrap();
        let _: i64 = parameters
            .call(
                &mut tx,
                "set_int",
                (
                    "purchase.max_direct_bill_source_rows".to_owned(),
                    Some(1_i64),
                ),
            )
            .await
            .unwrap();
        let oversized = po
            .call::<(), Value>(&mut tx, "action_create_vendor_bill", ())
            .await
            .unwrap_err();
        assert_eq!(oversized.kind(), ErrorKind::Validation);
        assert!(
            oversized
                .message()
                .contains("purchase.max_direct_bill_source_rows"),
            "{oversized:?}"
        );
        let _: i64 = parameters
            .call(
                &mut tx,
                "set_int",
                (
                    "purchase.max_direct_bill_source_rows".to_owned(),
                    Some(2_i64),
                ),
            )
            .await
            .unwrap();
        let order_id = po.ids()[0];
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(clerk).await.unwrap();
        let po = super::orders(&tx, &[company]).browse([order_id]).unwrap();
        let mut purchased = super::lines_of(&mut tx, &[company], &po).await;
        purchased.sort_by_key(|record| record.ids()[0]);
        let [line, second] = purchased.as_slice() else {
            panic!("expected two purchased items");
        };
        let answer: Value = po
            .call(&mut tx, "action_create_vendor_bill", ())
            .await
            .unwrap();
        let env = tx.env().clone();
        let normalized = erp_addon_web::metadata_test_support::test_normalize_action_result(
            &env,
            &mut tx,
            answer.clone(),
        )
        .await
        .unwrap();
        let wire = serde_json::to_value(normalized).unwrap();
        assert_eq!(wire["result"]["kind"], "window");
        assert_eq!(wire["result"]["views"][0]["type"], "form");
        assert_eq!(
            wire["result"]["views"][0]["id"],
            xmlid(&mut tx, "account", "view_invoice_form").await
        );
        let Value::Json(action) = &answer else {
            panic!("expected document action");
        };
        let mut other = orm.begin(SUPERUSER_ID).await.unwrap();
        let collision = super::orders(&other, &[company])
            .browse(po.ids())
            .unwrap()
            .call::<(), Value>(&mut other, "action_create_vendor_bill", ())
            .await
            .unwrap_err();
        assert_eq!(collision.kind(), ErrorKind::Lock, "{collision:?}");
        other.rollback().await.unwrap();
        drop(other);
        let bill = action["res_id"].as_i64().unwrap();
        assert_eq!(action["res_model"], "account.move");
        let document = super::bill_books::moves(&tx, company)
            .browse([bill])
            .unwrap();
        assert_eq!(
            document.cell(&mut tx, "state").await.unwrap(),
            text("draft")
        );
        assert_eq!(
            document.cell(&mut tx, "move_type").await.unwrap(),
            text("in_invoice")
        );
        assert_eq!(
            document.cell(&mut tx, "partner_id").await.unwrap(),
            Cell::Id(k.vendor)
        );
        let rows = tx.query("SELECT l.quantity,l.product_id,l.product_uom_id,l.account_id,l.price_unit,l.discount
            FROM purchase_direct_bill_line src JOIN account_move_line l ON l.id=src.bill_line_id
            WHERE src.order_id=$1 AND src.purchase_line_id=$2 AND src.bill_id=$3",
            &[&po.ids()[0], &line.ids()[0], &bill]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, Decimal>(0), dec("2"));
        assert_eq!(rows[0].get::<_, i64>(1), k.bolt);
        assert_eq!(rows[0].get::<_, i64>(2), k.dozen);
        assert_eq!(rows[0].get::<_, i64>(3), books.expense);
        assert_eq!(
            line.cell(&mut tx, "price_unit").await.unwrap(),
            Cell::Decimal(rows[0].get(4))
        );
        assert_eq!(
            line.cell(&mut tx, "discount").await.unwrap(),
            Cell::Decimal(rows[0].get(5))
        );
        let other_item=&tx.query("SELECT l.quantity,l.price_unit FROM purchase_direct_bill_line s JOIN account_move_line l ON l.id=s.bill_line_id WHERE s.purchase_line_id=$1 AND s.bill_id=$2", &[&second.ids()[0], &bill]).await.unwrap()[0];
        assert_eq!(other_item.get::<_, Decimal>(0), dec("1"));
        assert_eq!(other_item.get::<_, Decimal>(1), dec("36"));
        let billed_taxes=tx.query("SELECT rel.account_tax_id FROM purchase_direct_bill_line s JOIN account_move_line_account_tax_rel rel ON rel.account_move_line_id=s.bill_line_id WHERE s.order_id=$1 ORDER BY s.purchase_line_id", &[&po.ids()[0]]).await.unwrap();
        assert_eq!(billed_taxes.len(), 1);
        assert_eq!(
            billed_taxes[0].get::<_, i64>(0),
            k.tax.as_real().unwrap().get()
        );
        let first_id: i64 = tx
            .query(
                "SELECT bill_line_id FROM purchase_direct_bill_line WHERE purchase_line_id=$1",
                &[&line.ids()[0]],
            )
            .await
            .unwrap()[0]
            .get(0);
        let bill_item = po
            .env()
            .records("account.move.line")
            .unwrap()
            .browse([first_id])
            .unwrap();
        assert_eq!(
            bill_item
                .cell(&mut tx, "purchase_source_line_id")
                .await
                .unwrap(),
            Cell::Id(line.ids()[0])
        );
        assert_eq!(
            bill_item
                .cell(&mut tx, "purchase_source_order_id")
                .await
                .unwrap(),
            Cell::Id(po.ids()[0])
        );
        for (comparison, source) in [
            ("purchase_ordered_qty", "product_qty"),
            ("purchase_received_qty", "qty_received"),
            ("purchase_agreed_price", "price_unit"),
            ("purchase_agreed_discount", "discount"),
        ] {
            assert_eq!(
                bill_item.cell(&mut tx, comparison).await.unwrap(),
                line.cell(&mut tx, source).await.unwrap()
            );
        }
        let source_id = line.ids()[0];
        let original_id = po.ids()[0];
        tx.commit().await.unwrap();
        drop(tx);
        let mut accounting_tx = orm.begin(accountant).await.unwrap();
        let item = accounting_tx
            .env()
            .with_company(company)
            .unwrap()
            .records("account.move.line")
            .unwrap()
            .browse([first_id])
            .unwrap();
        assert_eq!(
            item.cell(&mut accounting_tx, "quantity").await.unwrap(),
            Cell::Decimal(dec("2"))
        );
        assert!(
            !accounting_tx
                .has_field_access(
                    item.env(),
                    "account.move.line",
                    "purchase_received_qty",
                    erp_sdk::FieldOp::Read
                )
                .await
                .unwrap()
        );
        accounting_tx.rollback().await.unwrap();
        drop(accounting_tx);
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let document = super::bill_books::moves(&tx, company)
            .browse([bill])
            .unwrap();
        let reassigned = document
            .write(
                &mut tx,
                RawVals::new().set_cell("partner_id", Cell::Id(books.customer)),
            )
            .await
            .unwrap_err();
        assert_eq!(reassigned.kind(), ErrorKind::Validation);
        assert!(
            reassigned.message().contains("purchase order"),
            "{reassigned:?}"
        );
        assert_eq!(
            document.cell(&mut tx, "partner_id").await.unwrap(),
            Cell::Id(k.vendor)
        );
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let changed_unit = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("account.move.line")
            .unwrap()
            .browse([first_id])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("product_uom_id", Cell::Id(k.unit)),
            )
            .await
            .unwrap_err();
        assert_eq!(changed_unit.kind(), ErrorKind::Validation);
        assert!(
            changed_unit.message().contains("purchase order"),
            "{changed_unit:?}"
        );
        tx.rollback().await.unwrap();
        drop(tx);
        // Re-selecting the same product alone also runs its default-unit logic.
        for (change, answer) in [
            (
                RawVals::new().set_cell("uom_id", Cell::Id(k.unit)),
                ErrorKind::UserError,
            ),
            (
                RawVals::new().set_cell("product_id", Cell::Id(k.bolt)),
                ErrorKind::Validation,
            ),
        ] {
            let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
            let source = tx
                .env()
                .with_company(company)
                .unwrap()
                .records("purchase.order.line")
                .unwrap()
                .browse([source_id])
                .unwrap();
            let changed_source = source
                .write(
                    &mut tx,
                    change.set_cell("price_unit", Cell::Decimal(dec("24"))),
                )
                .await
                .unwrap_err();
            // a new unit is refused by the received-goods guard of the line (a user error); a new
            // product, with its default unit, by the bill-link check (a validation error)
            assert_eq!(changed_source.kind(), answer, "{changed_source:?}");
            assert!(
                changed_source.message().contains("purchase order"),
                "{changed_source:?}"
            );
            assert_eq!(
                source.cell(&mut tx, "uom_id").await.unwrap(),
                Cell::Id(k.dozen)
            );
            tx.rollback().await.unwrap();
            drop(tx);
        }
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let po = super::orders(&tx, &[company])
            .browse([original_id])
            .unwrap();
        assert_eq!(
            po.call::<(), Value>(&mut tx, "action_create_vendor_bill", ())
                .await
                .unwrap(),
            answer
        );
    });
}

async fn cancel_blocked(tx: &mut Transaction, order: &Records) -> bool {
    tx.invalidate_records(order, Some(&["cancel_blocked"]))
        .await
        .unwrap();
    order.cell(tx, "cancel_blocked").await.unwrap() == Cell::Bool(true)
}

/// A purchase whose receipt is done keeps what was received: the form hides Cancel and the server
/// refuses it by name, and its line keeps its product, its unit and at least the received
/// quantity.
#[test]
fn received_purchases_keep_what_happened_downstream() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "purchase_stock").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let catalog = catalog(&mut tx, company).await;
        let source = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(catalog.vendor))
                .set_commands(
                    "order_line",
                    [Command::Create(
                        line_vals(catalog.bolt).set_cell("product_qty", Cell::Decimal(dec("2"))),
                    )],
                ),
        )
        .await;
        assert!(
            !cancel_blocked(&mut tx, &source).await,
            "a request for quotation"
        );
        act(&mut tx, &source, "button_confirm").await.unwrap();
        assert!(
            !cancel_blocked(&mut tx, &source).await,
            "an open receipt is cancelled with its order"
        );
        let line = super::lines_of(&mut tx, &[company], &source)
            .await
            .remove(0);
        let row = &tx
            .query(
                "SELECT m.picking_id, m.id
             FROM purchase_stock_receipt_move_link ml JOIN stock_move m ON m.id=ml.move_id
             WHERE ml.purchase_line_id=$1",
                &[&line.ids()[0]],
            )
            .await
            .unwrap()[0];
        let (receipt, mov): (i64, i64) = (row.get(0), row.get(1));
        let move_lines = tx.env().records("stock.move.line").unwrap();
        let existing = tx
            .query("SELECT id FROM stock_move_line WHERE move_id=$1", &[&mov])
            .await
            .unwrap();
        let quantity = RawVals::new().set_cell("quantity", Cell::Decimal(dec("2")));
        if let Some(row) = existing.first() {
            move_lines
                .browse([row.get(0)])
                .unwrap()
                .write(&mut tx, quantity)
                .await
                .unwrap();
        } else {
            move_lines
                .create(&mut tx, vec![quantity.set_cell("move_id", Cell::Id(mov))])
                .await
                .unwrap();
        }
        validate(&mut tx, receipt).await.unwrap();

        assert!(cancel_blocked(&mut tx, &source).await, "a done receipt");
        let Cell::Str(reference) = source.cell(&mut tx, "name").await.unwrap() else {
            panic!("an order reference");
        };
        let reference = reference.to_string();
        let refused = act(&mut tx, &source, "button_cancel").await.unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::UserError, "{refused:?}");
        assert!(
            refused.message().contains(reference.as_str())
                && refused.message().contains("its receipt")
                && refused.message().contains("is done"),
            "{refused:?}"
        );
        for (vals, expected) in [
            (
                RawVals::new().set_cell("product_qty", Cell::Decimal(dec("1"))),
                "cannot go below",
            ),
            (
                RawVals::new().set_cell("product_id", Cell::Id(catalog.nut)),
                "product and unit can no longer change",
            ),
            (
                RawVals::new().set_cell("uom_id", Cell::Id(catalog.dozen)),
                "product and unit can no longer change",
            ),
        ] {
            let err = super::write(&mut tx, &line, vals).await.unwrap_err();
            assert_eq!(err.kind(), ErrorKind::UserError, "{err:?}");
            assert!(err.message().contains(expected), "{err:?}");
            assert!(err.message().contains(reference.as_str()), "{err:?}");
        }
        // the received quantity itself passes
        super::write(
            &mut tx,
            &line,
            RawVals::new().set_cell("product_qty", Cell::Decimal(dec("2"))),
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        db.cleanup().await.unwrap();
    });
}
