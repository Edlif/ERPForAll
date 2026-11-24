// SPDX-License-Identifier: MIT OR Apache-2.0
//! The billed quantity, the quantity to bill and the billing status of purchase orders, the bill
//! control of products and its Purchase setting, the filters and the upgrade of existing orders.

use super::*;
use erp_addon_purchase::settings::BILL_CONTROL_SETTING;

/// The billed quantity and the quantity to bill of one line.
async fn billed(tx: &mut Transaction, line: &Records) -> (Decimal, Decimal) {
    (
        num(tx, line, "qty_invoiced").await,
        num(tx, line, "qty_to_invoice").await,
    )
}

async fn status(tx: &mut Transaction, order: &Records) -> String {
    match cell(tx, order, "invoice_status").await {
        Cell::Str(status) => status.to_string(),
        other => panic!("invoice_status is {other:?}"),
    }
}

async fn bill(tx: &mut Transaction, order: &Records, method: &'static str) -> i64 {
    let Value::Json(action) = order.call::<(), Value>(tx, method, ()).await.unwrap() else {
        panic!("expected the vendor bill form")
    };
    assert_eq!(action["res_model"], "account.move");
    action["res_id"].as_i64().unwrap()
}

/// The bill item of `bill` made from the purchase line `line`.
async fn item_of(tx: &mut Transaction, company: i64, bill: i64, line: &Records) -> Records {
    let id: i64 = tx
        .query(
            "SELECT bill_line_id FROM purchase_direct_bill_line WHERE bill_id=$1 AND purchase_line_id=$2",
            &[&bill, &line.ids()[0]],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.move.line")
        .unwrap()
        .browse([id])
        .unwrap()
}

async fn set_quantity(tx: &mut Transaction, item: &Records, quantity: &str) {
    item.write(
        tx,
        RawVals::new().set_cell("quantity", Cell::Decimal(dec(quantity))),
    )
    .await
    .unwrap();
}

fn d(value: &str) -> (Decimal, Decimal) {
    let (billed, to_bill) = value.split_once('/').unwrap();
    (dec(billed).normalize(), dec(to_bill).normalize())
}

#[test]
fn bill_control_drives_billed_quantities_the_status_and_the_bill_buttons() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_modules(&db, &["purchase_stock"]).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let system = xmlid(&mut tx, "base", "group_system").await;
        user(&mut tx, "billing-admin", &[system], &[company]).await;
        let purchase = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let accounting = xmlid(&mut tx, "account", "group_account_user").await;
        let clerk = user(
            &mut tx,
            "billing-clerk",
            &[purchase, accounting],
            &[company],
        )
        .await;
        let buyer = user(&mut tx, "billing-buyer", &[purchase], &[company]).await;
        bill_books::books(&mut tx, company).await;
        let k = catalog(&mut tx, company).await;
        let products = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("product.product")
            .unwrap();
        let service = products
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Installation"))
                        .set_cell("type", text("service"))
                        .set_cell("uom_id", Cell::Id(k.unit)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        // goods take the setting's default, services bill ordered quantities
        for (product, control) in [(k.bolt, "receive"), (service, "purchase")] {
            let product = products.browse([product]).unwrap();
            assert_eq!(
                cell(&mut tx, &product, "purchase_method").await,
                text(control)
            );
        }
        let po = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(k.vendor))
                .set_commands(
                    "order_line",
                    [
                        Command::Create(
                            line_vals(k.bolt)
                                .set_cell("product_qty", Cell::Decimal(dec("3")))
                                .set_cell("price_unit", Cell::Decimal(dec("10"))),
                        ),
                        Command::Create(
                            line_vals(service)
                                .set_cell("product_qty", Cell::Decimal(dec("2")))
                                .set_cell("price_unit", Cell::Decimal(dec("40"))),
                        ),
                    ],
                ),
        )
        .await;
        let [goods, work] = <[Records; 2]>::try_from(lines_of(&mut tx, &[company], &po).await)
            .unwrap_or_else(|_| panic!("two lines"));
        assert_eq!(status(&mut tx, &po).await, "no");
        assert_eq!(
            billed(&mut tx, &work).await,
            d("0/0"),
            "an RFQ has nothing to bill"
        );
        act(&mut tx, &po, "button_confirm").await.unwrap();
        assert_eq!(
            billed(&mut tx, &goods).await,
            d("0/0"),
            "nothing received yet"
        );
        assert_eq!(
            billed(&mut tx, &work).await,
            d("0/2"),
            "the service bills its order"
        );
        assert_eq!(status(&mut tx, &po).await, "to invoice");
        // receive two of the three bolts, no backorder
        let order_id = po.ids()[0];
        let movement=&tx.query("SELECT m.picking_id,m.id,m.picking_type_id FROM purchase_stock_receipt_move_link l JOIN stock_move m ON m.id=l.move_id JOIN purchase_order_line p ON p.id=l.purchase_line_id WHERE p.order_id=$1", &[&order_id]).await.unwrap()[0];
        let (receipt, mov, kind): (i64, i64, i64) =
            (movement.get(0), movement.get(1), movement.get(2));
        tx.env()
            .records("stock.picking.type")
            .unwrap()
            .browse([kind])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("create_backorder", text("never")),
            )
            .await
            .unwrap();
        let details = tx.env().records("stock.move.line").unwrap();
        let done = RawVals::new().set_cell("quantity", Cell::Decimal(dec("2")));
        let rows = tx
            .query("SELECT id FROM stock_move_line WHERE move_id=$1", &[&mov])
            .await
            .unwrap();
        if let Some(row) = rows.first() {
            details
                .browse([row.get(0)])
                .unwrap()
                .write(&mut tx, done)
                .await
                .unwrap();
        } else {
            details
                .create(&mut tx, vec![done.set_cell("move_id", Cell::Id(mov))])
                .await
                .unwrap();
        }
        returns::validate(&mut tx, receipt).await.unwrap();
        assert_eq!(billed(&mut tx, &goods).await, d("0/2"));
        tx.commit().await.unwrap();
        drop(tx);

        // a buyer without accounting rights reads the quantities and the status, not the bills
        let mut tx = orm.begin(buyer).await.unwrap();
        let mine = orders(&tx, &[company]).browse([order_id]).unwrap();
        assert_eq!(status(&mut tx, &mine).await, "to invoice");
        let line = lines(&tx, &[company]).browse(goods.ids()).unwrap();
        assert_eq!(billed(&mut tx, &line).await, d("0/2"));
        tx.rollback().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(clerk).await.unwrap();
        let po = orders(&tx, &[company]).browse([order_id]).unwrap();
        let goods = lines(&tx, &[company]).browse(goods.ids()).unwrap();
        let work = lines(&tx, &[company]).browse(work.ids()).unwrap();
        // the first bill takes each line's quantity to bill
        let first = bill(&mut tx, &po, "action_create_vendor_bill").await;
        let goods_item = item_of(&mut tx, company, first, &goods).await;
        let work_item = item_of(&mut tx, company, first, &work).await;
        assert_eq!(num(&mut tx, &goods_item, "quantity").await, dec("2"));
        assert_eq!(num(&mut tx, &work_item, "quantity").await, dec("2"));
        assert_eq!(billed(&mut tx, &goods).await, d("2/0"));
        assert_eq!(billed(&mut tx, &work).await, d("2/0"));
        assert_eq!(status(&mut tx, &po).await, "invoiced");
        // a draft edit in Accounting moves both numbers and the status
        set_quantity(&mut tx, &goods_item, "1.5").await;
        assert_eq!(billed(&mut tx, &goods).await, d("1.5/0.5"));
        assert_eq!(status(&mut tx, &po).await, "to invoice");
        // the remaining half bolt, then a cancelled and a deleted bill free their quantities
        let second = bill(&mut tx, &po, "action_bill_remaining").await;
        assert_ne!(second, first);
        let half = item_of(&mut tx, company, second, &goods).await;
        assert_eq!(num(&mut tx, &half, "quantity").await, dec("0.5"));
        assert_eq!(billed(&mut tx, &goods).await, d("2/0"));
        assert_eq!(status(&mut tx, &po).await, "invoiced");
        bill_books::moves(&tx, company)
            .browse([second])
            .unwrap()
            .call::<(), ()>(&mut tx, "button_cancel", ())
            .await
            .unwrap();
        assert_eq!(
            billed(&mut tx, &goods).await,
            d("1.5/0.5"),
            "a cancelled bill"
        );
        assert_eq!(status(&mut tx, &po).await, "to invoice");
        let third = bill(&mut tx, &po, "action_bill_remaining").await;
        assert_ne!(third, second);
        assert_eq!(billed(&mut tx, &goods).await, d("2/0"));
        bill_books::moves(&tx, company)
            .browse([third])
            .unwrap()
            .unlink(&mut tx)
            .await
            .unwrap();
        assert_eq!(
            billed(&mut tx, &goods).await,
            d("1.5/0.5"),
            "a deleted bill"
        );
        // a removed draft item counts nothing
        let removed = item_of(&mut tx, company, first, &work).await;
        bill_books::moves(&tx, company)
            .browse([first])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_commands(
                    "invoice_line_ids",
                    [Command::Delete(RecId::from_real(removed.ids()[0]).unwrap())],
                ),
            )
            .await
            .unwrap();
        assert_eq!(billed(&mut tx, &work).await, d("0/2"), "a removed item");
        // posting keeps the billed quantity
        bill_books::moves(&tx, company)
            .browse([first])
            .unwrap()
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
        let po = orders(&tx, &[company]).browse([order_id]).unwrap();
        let goods = lines(&tx, &[company]).browse(goods.ids()).unwrap();
        bill_books::moves(&tx, company)
            .browse([first])
            .unwrap()
            .call::<(), ()>(&mut tx, "action_post", ())
            .await
            .unwrap();
        assert_eq!(billed(&mut tx, &goods).await, d("1.5/0.5"), "a posted bill");
        // a fourth bill above the received quantity: the line and the button agree
        let fourth = bill(&mut tx, &po, "action_bill_remaining").await;
        let over = item_of(&mut tx, company, fourth, &goods).await;
        set_quantity(&mut tx, &over, "1.5").await;
        assert_eq!(
            billed(&mut tx, &goods).await,
            d("3/-1"),
            "billed above receipt"
        );
        assert_eq!(status(&mut tx, &po).await, "to invoice");
        let over_billed = po.clone();
        refused(
            tx.savepoint(async move |tx| {
                over_billed
                    .call::<(), Value>(tx, "action_bill_remaining", ())
                    .await
            })
            .await,
            ErrorKind::UserError,
            &["exceed"],
        );
        set_quantity(&mut tx, &over, "0.5").await;
        assert_eq!(billed(&mut tx, &goods).await, d("2/0"));
        // a linked vendor credit note subtracts its quantity
        let credit = bill_books::moves(&tx, company)
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("move_type", text("in_refund"))
                        .set_cell("company_id", Cell::Id(company))
                        .set_cell("partner_id", Cell::Id(k.vendor))
                        .set_commands(
                            "invoice_line_ids",
                            [Command::Create(
                                RawVals::new()
                                    .set_cell("product_id", Cell::Id(k.bolt))
                                    .set_cell("quantity", Cell::Decimal(dec("0.5")))
                                    .set_cell("price_unit", Cell::Decimal(dec("10"))),
                            )],
                        ),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let credit_item: i64 = tx
            .query(
                "SELECT id FROM account_move_line WHERE move_id=$1 AND display_type='product'",
                &[&credit],
            )
            .await
            .unwrap()[0]
            .get(0);
        tx.env()
            .sudo(true)
            .records("purchase.direct.bill.line")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("company_id", Cell::Id(company))
                        .set_cell("order_id", Cell::Id(order_id))
                        .set_cell("bill_id", Cell::Id(credit))
                        .set_cell("purchase_line_id", Cell::Id(goods.ids()[0]))
                        .set_cell("bill_line_id", Cell::Id(credit_item)),
                ],
            )
            .await
            .unwrap();
        assert_eq!(billed(&mut tx, &goods).await, d("1.5/0.5"), "a credit note");
        // the two filters of the order search
        let waiting = orders(&tx, &[company])
            .search(
                &mut tx,
                &Domain::condition("invoice_status", Operator::Eq, "to invoice").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(waiting.ids(), [order_id]);
        let full = orders(&tx, &[company])
            .search(
                &mut tx,
                &Domain::condition("invoice_status", Operator::Eq, "invoiced").unwrap(),
            )
            .await
            .unwrap();
        assert!(full.is_empty());
        for (filter, value) in [
            ("waiting_bills", "to invoice"),
            ("fully_billed", "invoiced"),
        ] {
            let found: i64 = tx
                .query(
                    "SELECT count(*) FROM ir_ui_view v JOIN ir_model_data d ON d.res_id = v.id
                     AND d.model = 'ir.ui.view' AND d.module = 'purchase' AND d.name = 'purchase_order_search'
                     WHERE v.arch_db ~ $1 AND v.arch_db ~ $2",
                    &[&format!("name=.{filter}."), &format!("invoice_status.*{value}")],
                )
                .await
                .unwrap()[0]
                .get(0);
            assert_eq!(found, 1, "the search view filters on {value} as {filter}");
        }
        tx.commit().await.unwrap();
        drop(tx);

        // the buyer still reads the numbers, and still cannot read any bill
        let mut tx = orm.begin(buyer).await.unwrap();
        let line = lines(&tx, &[company]).browse(goods.ids()).unwrap();
        assert_eq!(billed(&mut tx, &line).await, d("1.5/0.5"));
        let denied = bill_books::moves(&tx, company)
            .browse([first])
            .unwrap()
            .cell(&mut tx, "state")
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Access, "{denied:?}");
        tx.rollback().await.unwrap();
    })
}

#[test]
fn the_setting_decides_new_products_and_an_upgrade_fills_existing_orders() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = xmlid(&mut tx, "base", "main_company").await;
        let system = xmlid(&mut tx, "base", "group_system").await;
        let admin = user(&mut tx, "bill-control-admin", &[system], &[company]).await;
        let purchase = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let accounting = xmlid(&mut tx, "account", "group_account_user").await;
        let clerk = user(
            &mut tx,
            "bill-control-clerk",
            &[purchase, accounting],
            &[company],
        )
        .await;
        bill_books::books(&mut tx, company).await;
        let k = catalog(&mut tx, company).await;
        tx.commit().await.unwrap();
        drop(tx);

        // the administrator sets new products to bill ordered quantities
        let mut tx = orm.begin(admin).await.unwrap();
        let env = tx.env().clone();
        let keys = vec![BILL_CONTROL_SETTING.to_owned()];
        let read = read_values(&env, &mut tx, SettingsScope::Global {}, keys)
            .await
            .unwrap();
        let revision = erp_sdk::serde_json::to_value(&read).unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        write_values(
            &env,
            &mut tx,
            SettingsScope::Global {},
            revision,
            vec![(BILL_CONTROL_SETTING.to_owned(), Value::str("purchase"))],
        )
        .await
        .unwrap();
        // a product created after the change takes the new default
        let gasket = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("product.product")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text("Gasket"))
                        .set_cell("uom_id", Cell::Id(k.unit)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.commit().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(clerk).await.unwrap();
        let products = tx
            .env()
            .with_company(company)
            .unwrap()
            .records("product.product")
            .unwrap();
        let gasket = products.browse([gasket]).unwrap();
        assert_eq!(
            cell(&mut tx, &gasket, "purchase_method").await,
            text("purchase")
        );
        let bolt = products.browse([k.bolt]).unwrap();
        assert_eq!(
            cell(&mut tx, &bolt, "purchase_method").await,
            text("receive")
        );
        let po = order(
            &mut tx,
            &[company],
            RawVals::new()
                .set_cell("partner_id", Cell::Id(k.vendor))
                .set_commands(
                    "order_line",
                    [
                        Command::Create(
                            line_vals(gasket.ids()[0])
                                .set_cell("product_qty", Cell::Decimal(dec("4")))
                                .set_cell("price_unit", Cell::Decimal(dec("5"))),
                        ),
                        Command::Create(
                            line_vals(k.bolt).set_cell("product_qty", Cell::Decimal(dec("2"))),
                        ),
                    ],
                ),
        )
        .await;
        let [ordered, received] =
            <[Records; 2]>::try_from(lines_of(&mut tx, &[company], &po).await)
                .unwrap_or_else(|_| panic!("two lines"));
        // the computed fields are never written by a client
        refused(
            write(
                &mut tx,
                &ordered,
                RawVals::new().set_cell("qty_invoiced", Cell::Decimal(dec("1"))),
            )
            .await,
            ErrorKind::Validation,
            &["qty_invoiced"],
        );
        refused(
            write(
                &mut tx,
                &po,
                RawVals::new().set_cell("invoice_status", text("invoiced")),
            )
            .await,
            ErrorKind::Validation,
            &["invoice_status"],
        );
        act(&mut tx, &po, "button_confirm").await.unwrap();
        // without receipts: ordered quantities bill at once, received quantities wait
        assert_eq!(billed(&mut tx, &ordered).await, d("0/4"));
        assert_eq!(billed(&mut tx, &received).await, d("0/0"));
        let first = bill(&mut tx, &po, "action_create_vendor_bill").await;
        let rows = tx
            .query(
                "SELECT purchase_line_id FROM purchase_direct_bill_line WHERE bill_id=$1",
                &[&first],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "only the line with a quantity to bill");
        let item = item_of(&mut tx, company, first, &ordered).await;
        assert_eq!(num(&mut tx, &item, "quantity").await, dec("4"));
        assert_eq!(status(&mut tx, &po).await, "invoiced");
        set_quantity(&mut tx, &item, "3").await;
        assert_eq!(billed(&mut tx, &ordered).await, d("3/1"));
        assert_eq!(status(&mut tx, &po).await, "to invoice");
        let order_id = po.ids()[0];
        tx.commit().await.unwrap();
        drop(tx);

        // a database of the previous release: none of the new columns, purchase at 1.3.0
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        tx.execute(
            "ALTER TABLE purchase_order_line DROP COLUMN qty_invoiced, DROP COLUMN qty_to_invoice",
            &[],
        )
        .await
        .unwrap();
        tx.execute("ALTER TABLE purchase_order DROP COLUMN invoice_status", &[])
            .await
            .unwrap();
        tx.execute(
            "ALTER TABLE product_template DROP COLUMN purchase_method",
            &[],
        )
        .await
        .unwrap();
        tx.execute(
            "UPDATE ir_module_module SET latest_version='1.3.0' WHERE name='purchase'",
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
        let orm = purchase_policy_orm(&db, &upgraded.orm, &[]).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let empty: i64 = tx
            .query(
                "SELECT (SELECT count(*) FROM purchase_order_line WHERE qty_invoiced IS NULL OR qty_to_invoice IS NULL)
                      + (SELECT count(*) FROM purchase_order WHERE invoice_status IS NULL)
                      + (SELECT count(*) FROM product_template WHERE purchase_method IS NULL)",
                &[],
            )
            .await
            .unwrap()[0]
            .get(0);
        assert_eq!(empty, 0, "the upgrade fills every new column");
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(clerk).await.unwrap();
        let po = orders(&tx, &[company]).browse([order_id]).unwrap();
        let ordered = lines(&tx, &[company]).browse(ordered.ids()).unwrap();
        let received = lines(&tx, &[company]).browse(received.ids()).unwrap();
        // products without a stored control take the setting's (now ordered quantities)
        assert_eq!(billed(&mut tx, &ordered).await, d("3/1"));
        assert_eq!(billed(&mut tx, &received).await, d("0/2"));
        assert_eq!(status(&mut tx, &po).await, "to invoice");
        tx.rollback().await.unwrap();
    })
}
