// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

static HIDE_BILLED_QUANTITY: erp_orm::FieldAccessImpl = erp_orm::FieldAccessImpl {
    addon: "account",
    model: "account.move.line",
    method: "hide_billed_quantity",
    context: &[],
    run: |ctx, op, mask, sup| {
        let mut mask = sup.call(ctx, op, mask)?;
        mask.deny(ctx.field("quantity")?, erp_orm::FieldOp::Read)?;
        Ok(mask)
    },
    file: file!(),
    line: line!(),
};

async fn bill_id(tx: &mut Transaction, order: &Records, method: &'static str) -> i64 {
    let Value::Json(action) = order.call::<(), Value>(tx, method, ()).await.unwrap() else {
        panic!("expected the vendor bill form")
    };
    assert_eq!(action["res_model"], "account.move");
    action["res_id"].as_i64().unwrap()
}

#[test]
fn a_short_receipt_is_billed_in_parts_and_a_retry_reopens_the_latest_bill() {
    short_receipt(false);
}

#[test]
fn partial_receipt_backorder_bills_only_the_next_delivery() {
    short_receipt(true);
}

fn short_receipt(always: bool) {
    run(async move {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_modules(&db, &["purchase_stock", "purchase_three_way_match"]).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let system = xmlid(&mut tx, "base", "group_system").await;
        user(&mut tx, "remaining-admin", &[system], &[company]).await;
        let purchase = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let accounting = xmlid(&mut tx, "account", "group_account_user").await;
        let clerk = user(
            &mut tx,
            "remaining-buyer",
            &[purchase, accounting],
            &[company],
        )
        .await;
        let buyer = user(&mut tx, "purchase-only-remainder", &[purchase], &[company]).await;
        bill_books::books(&mut tx, company).await;
        let k = catalog(&mut tx, company).await;
        let po = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(k.vendor))
                .set_commands(
                    "order_line",
                    [Command::Create(
                        line_vals(k.bolt)
                            .set_cell("uom_id", Cell::Id(k.dozen))
                            .set_cell("product_qty", Cell::Decimal(dec("3")))
                            .set_cell("price_unit", Cell::Decimal(dec("24"))),
                    )],
                ),
        )
        .await;
        act(&mut tx, &po, "button_confirm").await.unwrap();
        let parent = po.ids()[0];
        let movement=&tx.query("SELECT m.picking_id,m.id,m.picking_type_id FROM purchase_stock_receipt_move_link l JOIN stock_move m ON m.id=l.move_id JOIN purchase_order_line p ON p.id=l.purchase_line_id WHERE p.order_id=$1", &[&parent]).await.unwrap()[0];
        let (receipt, mov, kind): (i64, i64, i64) =
            (movement.get(0), movement.get(1), movement.get(2));
        tx.env()
            .records("stock.picking.type")
            .unwrap()
            .browse([kind])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell(
                    "create_backorder",
                    text(if always { "always" } else { "never" }),
                ),
            )
            .await
            .unwrap();
        let moves = tx.env().records("stock.move.line").unwrap();
        let quantity = RawVals::new().set_cell("quantity", Cell::Decimal(dec("2")));
        let rows = tx
            .query("SELECT id FROM stock_move_line WHERE move_id=$1", &[&mov])
            .await
            .unwrap();
        if let Some(row) = rows.first() {
            moves
                .browse([row.get(0)])
                .unwrap()
                .write(&mut tx, quantity)
                .await
                .unwrap();
        } else {
            moves
                .create(&mut tx, vec![quantity.set_cell("move_id", Cell::Id(mov))])
                .await
                .unwrap();
        }
        returns::validate(&mut tx, receipt).await.unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(buyer).await.unwrap();
        let denied = orders(&tx, &[company])
            .browse([parent])
            .unwrap()
            .call::<(), Value>(&mut tx, "action_bill_remaining", ())
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Access, "{denied:?}");
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(clerk).await.unwrap();
        let po = orders(&tx, &[company]).browse([parent]).unwrap();
        assert_eq!(
            po.cell(&mut tx, "invoice_count").await.unwrap(),
            Cell::Int(0)
        );
        let first = bill_id(&mut tx, &po, "action_create_vendor_bill").await;
        let row=&tx.query("SELECT l.id,l.quantity FROM purchase_direct_bill_line s JOIN account_move_line l ON l.id=s.bill_line_id WHERE s.bill_id=$1",&[&first]).await.unwrap()[0];
        assert_eq!(row.get::<_, Decimal>(1), dec("2"));
        if always {
            let remainder = tx.query("SELECT p.id,m.product_uom_qty FROM stock_picking p JOIN stock_move m ON m.picking_id=p.id WHERE p.backorder_id=$1", &[&receipt]).await.unwrap();
            assert_eq!(remainder.len(), 1);
            let child: i64 = remainder[0].get(0);
            assert_eq!(remainder[0].get::<_, Decimal>(1), dec("1"));
            tx.commit().await.unwrap();
            drop(tx);
            let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
            let backorder = tx
                .env()
                .records("stock.picking")
                .unwrap()
                .browse([child])
                .unwrap();
            assert_eq!(
                backorder.cell(&mut tx, "purchase_order_id").await.unwrap(),
                Cell::Id(parent)
            );
            returns::validate(&mut tx, child).await.unwrap();
            let retry = returns::validate(&mut tx, receipt).await.unwrap_err();
            assert!(retry.message().contains("validated earlier"), "{retry}");
            tx.commit().await.unwrap();
            drop(tx);
            let mut tx = orm.begin(clerk).await.unwrap();
            let po = orders(&tx, &[company]).browse([parent]).unwrap();
            let next = bill_id(&mut tx, &po, "action_bill_remaining").await;
            assert_ne!(first, next);
            let items=tx.query("SELECT l.quantity FROM purchase_direct_bill_line s JOIN account_move_line l ON l.id=s.bill_line_id WHERE s.bill_id=$1",&[&next]).await.unwrap();
            assert_eq!(items[0].get::<_, Decimal>(0), dec("1"));
            assert_eq!(bill_id(&mut tx, &po, "action_bill_remaining").await, next);
            // The count button: two bills for the clerk, none for a buyer without Accounting.
            assert_eq!(
                po.cell(&mut tx, "invoice_count").await.unwrap(),
                Cell::Int(2)
            );
            let shown: Value = po.call(&mut tx, "action_view_invoice", ()).await.unwrap();
            let shown: Value = po
                .call(&mut tx, "erp_normalize_action_result", (1_u64, shown))
                .await
                .expect("the web action normalizer accepts the vendor bills action");
            let Value::Json(shown) = shown else {
                panic!("a web action envelope")
            };
            assert_eq!(shown["result"]["kind"], "window");
            assert_eq!(shown["result"]["model"], "account.move");
            assert_eq!(shown["result"]["views"][0]["type"], "list");
            assert_eq!(shown["result"]["views"][1]["type"], "form");
            // The way back from a bill to its order.
            let bill = bill_books::moves(&tx, company).browse([next]).unwrap();
            assert_eq!(
                bill.cell(&mut tx, "purchase_order_count").await.unwrap(),
                Cell::Int(1)
            );
            let back: Value = bill
                .call(&mut tx, "action_view_purchase_orders", ())
                .await
                .unwrap();
            let back: Value = bill
                .call(&mut tx, "erp_normalize_action_result", (1_u64, back))
                .await
                .expect("the web action normalizer accepts the purchase orders action");
            let Value::Json(back) = back else {
                panic!("a web action envelope")
            };
            assert_eq!(back["result"]["model"], "purchase.order");
            tx.commit().await.unwrap();
            drop(tx);
            let mut tx = orm.begin(buyer).await.unwrap();
            let buyer_po = orders(&tx, &[company]).browse([parent]).unwrap();
            assert_eq!(
                buyer_po.cell(&mut tx, "invoice_count").await.unwrap(),
                Cell::Int(0)
            );
            let none: Value = buyer_po
                .call(&mut tx, "action_view_invoice", ())
                .await
                .unwrap();
            let Value::Map(none) = none else {
                panic!("an action dictionary")
            };
            assert!(!format!("{:?}", none["domain"]).contains("Int("), "{none:?}");
            tx.rollback().await.unwrap();
            return;
        }
        let item = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("account.move.line")
            .unwrap()
            .browse([row.get(0)])
            .unwrap();
        item.write(
            &mut tx,
            RawVals::new().set_cell("quantity", Cell::Decimal(dec("0.75"))),
        )
        .await
        .unwrap();
        let first_doc = bill_books::moves(&tx, company).browse([first]).unwrap();
        first_doc
            .write(
                &mut tx,
                RawVals::new()
                    .set_cell("invoice_date", Cell::Date(bill_books::d(2024, 6, 3)))
                    .set_cell("date", Cell::Date(bill_books::d(2024, 6, 3))),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        bill_books::periods(&orm, company).await;
        let mut tx = orm.begin(clerk).await.unwrap();
        bill_books::moves(&tx, company)
            .browse([first])
            .unwrap()
            .call::<(), ()>(&mut tx, "action_post", ())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        // Recreate the shipped 1.1 constraint around this actual billed purchase line.
        tx.execute("ALTER TABLE purchase_direct_bill_line DROP CONSTRAINT purchase_direct_bill_line_source,
            ADD CONSTRAINT purchase_direct_bill_line_source UNIQUE (purchase_line_id)", &[]).await.unwrap();
        tx.execute("COMMENT ON CONSTRAINT purchase_direct_bill_line_source ON purchase_direct_bill_line IS 'UNIQUE (purchase_line_id)'",&[]).await.unwrap();
        tx.execute(
            "UPDATE ir_module_module SET latest_version='1.1.0' WHERE name='purchase'",
            &[],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        drop(orm);
        let system = purchase_module_system(&db);
        let upgraded = Box::pin(system.button_immediate_upgrade(
            &system.system_env(Context::new()),
            &[ModuleName::new("purchase").unwrap()],
        ))
        .await
        .unwrap();
        assert!(
            upgraded.post_commit.is_clean(),
            "{:?}",
            upgraded.post_commit
        );
        let restricted = purchase_policy_orm(&db, &upgraded.orm, &[&HIDE_BILLED_QUANTITY]).await;
        let mut tx = restricted.begin(clerk).await.unwrap();
        let hidden = orders(&tx, &[company])
            .browse([parent])
            .unwrap()
            .call::<(), Value>(&mut tx, "action_bill_remaining", ())
            .await
            .unwrap_err();
        assert_eq!(hidden.kind(), ErrorKind::Access, "{hidden:?}");
        assert!(hidden.message().contains("fields"), "{hidden:?}");
        let advisory = bill_books::moves(&tx, company)
            .browse([first])
            .unwrap()
            .cell(&mut tx, "purchase_match_status")
            .await
            .unwrap();
        assert_eq!(
            advisory,
            text("Purchase match unavailable with your access")
        );
        tx.rollback().await.unwrap();
        drop(tx);
        drop(restricted);
        let orm = purchase_policy_orm(&db, &upgraded.orm, &[]).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let configuration = tx.env().records("ir.config_parameter").unwrap();
        let _: i64 = configuration
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
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(clerk).await.unwrap();
        let po = orders(&tx, &[company]).browse([parent]).unwrap();
        let oversized = po
            .call::<(), Value>(&mut tx, "action_bill_remaining", ())
            .await
            .unwrap_err();
        assert_eq!(oversized.kind(), ErrorKind::Validation, "{oversized:?}");
        assert!(
            oversized
                .message()
                .contains("purchase.max_direct_bill_source_rows"),
            "{oversized:?}"
        );
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let configuration = tx.env().records("ir.config_parameter").unwrap();
        let _: i64 = configuration
            .call(
                &mut tx,
                "set_int",
                (
                    "purchase.max_direct_bill_source_rows".to_owned(),
                    Some(12_i64),
                ),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(clerk).await.unwrap();
        let po = orders(&tx, &[company]).browse([parent]).unwrap();
        let first_doc = bill_books::moves(&tx, company).browse([first]).unwrap();
        assert_eq!(
            first_doc
                .cell(&mut tx, "purchase_match_status")
                .await
                .unwrap(),
            text("Matched")
        );
        let second = bill_id(&mut tx, &po, "action_bill_remaining").await;
        assert_ne!(first, second);
        let qty=tx.query("SELECT l.quantity FROM purchase_direct_bill_line s JOIN account_move_line l ON l.id=s.bill_line_id WHERE s.bill_id=$1",&[&second]).await.unwrap()[0].get::<_,Decimal>(0);
        assert_eq!(qty, dec("1.25"));
        let doc = bill_books::moves(&tx, company).browse([second]).unwrap();
        let result = doc.cell(&mut tx, "purchase_match_status").await.unwrap();
        assert_eq!(result, text("Matched"));
        let second_id: i64 = tx
            .query(
                "SELECT bill_line_id FROM purchase_direct_bill_line WHERE bill_id=$1",
                &[&second],
            )
            .await
            .unwrap()[0]
            .get(0);
        let current = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("account.move.line")
            .unwrap()
            .browse([second_id])
            .unwrap();
        assert_eq!(
            current
                .cell(&mut tx, "purchase_match_status")
                .await
                .unwrap(),
            text("Matched")
        );
        current
            .write(
                &mut tx,
                RawVals::new().set_cell("price_unit", Cell::Decimal(dec("25"))),
            )
            .await
            .unwrap();
        assert_eq!(
            current
                .cell(&mut tx, "purchase_match_status")
                .await
                .unwrap(),
            text("Review price or discount")
        );
        assert_eq!(
            doc.cell(&mut tx, "purchase_match_status").await.unwrap(),
            text("Review purchase items")
        );
        current
            .write(
                &mut tx,
                RawVals::new()
                    .set_cell("price_unit", Cell::Decimal(dec("24")))
                    .set_cell("quantity", Cell::Decimal(dec("1.5"))),
            )
            .await
            .unwrap();
        assert_eq!(
            current
                .cell(&mut tx, "purchase_match_status")
                .await
                .unwrap(),
            text("Billed above received quantity")
        );
        assert_eq!(
            first_doc
                .cell(&mut tx, "purchase_match_status")
                .await
                .unwrap(),
            text("Review purchase items")
        );
        current
            .write(
                &mut tx,
                RawVals::new().set_cell("quantity", Cell::Decimal(dec("1.25"))),
            )
            .await
            .unwrap();
        let mut other = orm.begin(clerk).await.unwrap();
        let collision = orders(&other, &[company])
            .browse([parent])
            .unwrap()
            .call::<(), Value>(&mut other, "action_bill_remaining", ())
            .await
            .unwrap_err();
        assert_eq!(collision.kind(), ErrorKind::Lock, "{collision:?}");
        other.rollback().await.unwrap();
        drop(other);
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(clerk).await.unwrap();
        let po = orders(&tx, &[company]).browse([parent]).unwrap();
        assert_eq!(
            bill_id(&mut tx, &po, "action_create_vendor_bill").await,
            first
        );
        assert_eq!(bill_id(&mut tx, &po, "action_bill_remaining").await, second);
        let document = bill_books::moves(&tx, company).browse([second]).unwrap();
        document
            .call::<(), ()>(&mut tx, "button_cancel", ())
            .await
            .unwrap();
        assert_eq!(
            document
                .cell(&mut tx, "purchase_match_status")
                .await
                .unwrap(),
            text("Cancelled bill")
        );
        let third = bill_id(&mut tx, &po, "action_bill_remaining").await;
        assert_ne!(third, second);
        assert_ne!(third, first);
        assert_eq!(bill_id(&mut tx, &po, "action_bill_remaining").await, third);
        let total=tx.query("SELECT sum(l.quantity) FROM purchase_direct_bill_line s JOIN account_move_line l ON l.id=s.bill_line_id JOIN account_move m ON m.id=s.bill_id WHERE m.state<>'cancel' AND s.order_id=$1",&[&parent]).await.unwrap()[0].get::<_,Decimal>(0);
        assert_eq!(total, dec("2"));
        let remove_id: i64 = tx
            .query(
                "SELECT bill_line_id FROM purchase_direct_bill_line WHERE bill_id=$1",
                &[&third],
            )
            .await
            .unwrap()[0]
            .get(0);
        let draft = bill_books::moves(&tx, company).browse([third]).unwrap();
        draft
            .write(
                &mut tx,
                RawVals::new().set_commands(
                    "invoice_line_ids",
                    [Command::Delete(RecId::from_real(remove_id).unwrap())],
                ),
            )
            .await
            .unwrap();
        let fourth = bill_id(&mut tx, &po, "action_bill_remaining").await;
        assert!(fourth > third);
        assert_eq!(tx.query("SELECT l.quantity FROM purchase_direct_bill_line s JOIN account_move_line l ON l.id=s.bill_line_id WHERE s.bill_id=$1", &[&fourth]).await.unwrap()[0].get::<_,Decimal>(0),dec("1.25"));
    })
}

#[test]
fn only_purchase_users_see_match_evidence_and_manual_items_stay_unlinked() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "purchase_three_way_match").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = bill_books::main_company(&mut tx).await;
        let system = xmlid(&mut tx, "base", "group_system").await;
        user(&mut tx, "match-admin", &[system], &[company]).await;
        let purchase = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let accounting = xmlid(&mut tx, "account", "group_account_user").await;
        let reviewer = user(
            &mut tx,
            "match-reviewer",
            &[accounting, purchase],
            &[company],
        )
        .await;
        let accountant = user(&mut tx, "payables-only", &[accounting], &[company]).await;
        let books = bill_books::books(&mut tx, company).await;
        tx.commit().await.unwrap();
        drop(tx);
        bill_books::periods(&orm, company).await;
        let mut tx = orm.begin(reviewer).await.unwrap();
        let manual = bill_books::posted_invoice(
            &mut tx,
            &books,
            "in_invoice",
            books.vendor,
            "17",
            bill_books::d(2024, 6, 7),
        )
        .await;
        assert_eq!(
            manual.cell(&mut tx, "purchase_match_status").await.unwrap(),
            text("Review purchase items")
        );
        let manual_id: i64 = tx
            .query(
                "SELECT id FROM account_move_line WHERE move_id=$1 AND display_type='product'",
                &[&manual.ids()[0]],
            )
            .await
            .unwrap()[0]
            .get(0);
        let manual_item = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("account.move.line")
            .unwrap()
            .browse([manual_id])
            .unwrap();
        assert_eq!(
            manual_item
                .cell(&mut tx, "purchase_match_status")
                .await
                .unwrap(),
            text("No linked purchase item")
        );

        let sale = bill_books::posted_invoice(
            &mut tx,
            &books,
            "out_invoice",
            books.customer,
            "11",
            bill_books::d(2024, 6, 8),
        )
        .await;
        assert_eq!(
            sale.cell(&mut tx, "purchase_match_status").await.unwrap(),
            Cell::Null
        );
        let manual_id = manual.ids()[0];
        tx.commit().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(accountant).await.unwrap();
        let doc = bill_books::moves(&tx, company).browse([manual_id]).unwrap();
        assert_eq!(doc.cell(&mut tx, "state").await.unwrap(), text("posted"));
        assert_eq!(
            doc.cell(&mut tx, "purchase_match_status")
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Access
        );
    })
}
