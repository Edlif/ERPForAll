// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

async fn document(
    tx: &mut Transaction,
    company: i64,
    po: i64,
    method: &'static str,
) -> Result<i64> {
    let result = orders(tx, &[company])
        .browse([po])?
        .call::<(), Value>(tx, method, ())
        .await?;
    let Value::Json(action) = result else {
        panic!("expected bill form")
    };
    Ok(action["res_id"].as_i64().unwrap())
}

async fn compete(orm: &Orm, actor: UserId, company: i64, po: i64, method: &'static str) -> i64 {
    let mut stale = orm.begin(actor).await.unwrap();
    let pin = stale
        .query(
            "SELECT current_setting('transaction_isolation'), id FROM purchase_order WHERE id=$1",
            &[&po],
        )
        .await
        .unwrap();
    assert_eq!(pin[0].get::<_, String>(0), "repeatable read");
    assert_eq!(pin[0].get::<_, i64>(1), po);
    let mut writer = orm.begin(actor).await.unwrap();
    let winner = document(&mut writer, company, po, method).await.unwrap();
    writer.commit().await.unwrap();
    drop(writer);
    let loser = document(&mut stale, company, po, method).await.unwrap_err();
    assert_eq!(loser.sqlstate(), Some("40001"), "{loser:?}");
    stale.rollback().await.unwrap();
    drop(stale);
    let mut retry = orm.begin(actor).await.unwrap();
    let reopened = document(&mut retry, company, po, method).await.unwrap();
    assert_eq!(reopened, winner);
    retry.commit().await.unwrap();
    winner
}

#[test]
fn stale_purchase_snapshot_cannot_generate_duplicate_first_or_followup_bills() {
    run(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let orm = install_module(&db, "purchase_stock").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let company = bill_books::main_company(&mut tx).await;
        let system = xmlid(&mut tx, "base", "group_system").await;
        user(&mut tx, "snapshot-admin", &[system], &[company]).await;
        let purchase = xmlid(&mut tx, "purchase", "group_purchase_user").await;
        let accounting = xmlid(&mut tx, "account", "group_account_user").await;
        let actor = user(
            &mut tx,
            "snapshot-buyer",
            &[purchase, accounting],
            &[company],
        )
        .await;
        let books = bill_books::books(&mut tx, company).await;
        let buyer = user(&mut tx, "header-buyer", &[purchase], &[company]).await;
        let euro = xmlid(&mut tx, "base", "EUR").await;
        tx.env()
            .records("res.currency")
            .unwrap()
            .browse([euro])
            .unwrap()
            .write(&mut tx, RawVals::new().set_cell("active", Cell::Bool(true)))
            .await
            .unwrap();
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
                            .set_cell("product_qty", Cell::Decimal(dec("2")))
                            .set_cell("price_unit", Cell::Decimal(dec("20"))),
                    )],
                ),
        )
        .await;
        act(&mut tx, &po, "button_confirm").await.unwrap();
        let po = po.ids()[0];
        let row=&tx.query("SELECT m.id,m.picking_id FROM purchase_stock_receipt_move_link l JOIN stock_move m ON m.id=l.move_id JOIN purchase_order_line p ON p.id=l.purchase_line_id WHERE p.order_id=$1",&[&po]).await.unwrap()[0];
        let (movement, picking): (i64, i64) = (row.get(0), row.get(1));
        let detail = tx.env().records("stock.move.line").unwrap();
        let existing = tx
            .query(
                "SELECT id FROM stock_move_line WHERE move_id=$1",
                &[&movement],
            )
            .await
            .unwrap();
        let done = RawVals::new().set_cell("quantity", Cell::Decimal(dec("2")));
        if let Some(row) = existing.first() {
            detail
                .browse([row.get(0)])
                .unwrap()
                .write(&mut tx, done)
                .await
                .unwrap();
        } else {
            detail
                .create(&mut tx, vec![done.set_cell("move_id", Cell::Id(movement))])
                .await
                .unwrap();
        }
        returns::validate(&mut tx, picking).await.unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let first = compete(&orm, actor, company, po, "action_create_vendor_bill").await;
        let mut tx = orm.begin(actor).await.unwrap();
        let item = tx
            .query(
                "SELECT bill_line_id FROM purchase_direct_bill_line WHERE bill_id=$1",
                &[&first],
            )
            .await
            .unwrap()[0]
            .get(0);
        tx.env()
            .with_company(company)
            .unwrap()
            .records("account.move.line")
            .unwrap()
            .browse([item])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("quantity", Cell::Decimal(dec("1"))),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let next = compete(&orm, actor, company, po, "action_bill_remaining").await;
        assert_ne!(next, first);
        let mut tx = orm.begin(actor).await.unwrap();
        let result=&tx.query("SELECT COUNT(DISTINCT s.bill_id),SUM(l.quantity) FROM purchase_direct_bill_line s JOIN account_move_line l ON l.id=s.bill_line_id WHERE s.order_id=$1",&[&po]).await.unwrap()[0];
        assert_eq!(result.get::<_, i64>(0), 2);
        assert_eq!(result.get::<_, Decimal>(1), dec("2"));
        tx.rollback().await.unwrap();
        drop(tx);
        let mut tx = orm.begin(buyer).await.unwrap();
        let source = orders(&tx, &[company]).browse([po]).unwrap();
        act(&mut tx, &source, "button_cancel").await.unwrap();
        act(&mut tx, &source, "button_draft").await.unwrap();
        for (field, value) in [("partner_id", books.vendor), ("currency_id", euro)] {
            let before = source.cell(&mut tx, field).await.unwrap();
            assert_ne!(before, Cell::Id(value));
            let denied = source
                .write(&mut tx, RawVals::new().set_cell(field, Cell::Id(value)))
                .await
                .unwrap_err();
            assert_eq!(denied.kind(), ErrorKind::Validation, "{denied:?}");
            assert!(denied.message().contains("vendor bill"), "{denied:?}");
            assert_eq!(source.cell(&mut tx, field).await.unwrap(), before);
        }
    })
}
