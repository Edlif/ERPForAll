// SPDX-License-Identifier: MIT OR Apache-2.0
//! A line never changes its order, and an order keeps its company once confirmed or once it has
//! lines: both checked for a salesperson and a Sales administrator, the company change with a user
//! of two companies.

use erp_fields::Decimal;

use super::*;

async fn chair(tx: &mut Transaction) -> i64 {
    let company = xmlid(tx, "base", "main_company").await;
    tx.env()
        .with_company(company)
        .unwrap()
        .records("product.product")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("Chair".into()))
                    .set_cell("list_price", Cell::Decimal(Decimal::from(100))),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

async fn product_line(tx: &mut Transaction, order: &Records, product: i64) -> i64 {
    tx.env()
        .records("sale.order.line")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("order_id", Cell::Id(order.ids()[0]))
                    .set_cell("product_id", Cell::Id(product)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// An internal user of `companies` (the first is the current one) in `group` of `sales_team`.
async fn member(tx: &mut Transaction, login: &str, group: &str, companies: &[i64]) -> i64 {
    let group = xmlid(tx, "sales_team", group).await;
    let partner = partner(tx, RawVals::new().set_cell("name", Cell::Str(login.into()))).await;
    tx.env()
        .records("res.users")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(login.into()))
                    .set_cell("login", Cell::Str(login.into()))
                    .set_cell("partner_id", Cell::Id(partner))
                    .set_cell("company_id", Cell::Id(companies[0]))
                    .set_commands(
                        "company_ids",
                        [Command::set(
                            companies.iter().map(|c| id(*c)).collect::<Vec<_>>(),
                        )],
                    )
                    .set_commands("group_ids", [Command::set([id(group)])]),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// `model` records `ids` of a transaction whose user works in `companies`.
fn browse(tx: &Transaction, model: &str, ids: &[i64], companies: &[i64]) -> Records {
    let active = Value::list(companies.iter().map(|c| Value::Int(*c)));
    tx.env()
        .with_context([("allowed_company_ids", active)])
        .records(model)
        .unwrap()
        .browse(ids.to_vec())
        .unwrap()
}

#[test]
fn a_line_never_moves_to_another_order() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        admin(&mut tx).await;
        let salesman = user(&mut tx, "sam", "group_sale_salesman_all_leads").await;
        let manager = user(&mut tx, "meg", "group_sale_manager").await;
        let product = chair(&mut tx).await;
        let draft = new_order(&mut tx, &world).await;
        let draft_line = product_line(&mut tx, &draft, product).await;
        let confirmed = new_order(&mut tx, &world).await;
        let confirmed_line = product_line(&mut tx, &confirmed, product).await;
        act(&mut tx, &confirmed, "action_confirm").await.unwrap();
        let locked = new_order(&mut tx, &world).await;
        product_line(&mut tx, &locked, product).await;
        act(&mut tx, &locked, "action_confirm").await.unwrap();
        act(&mut tx, &locked, "action_lock").await.unwrap();
        let cancelled = new_order(&mut tx, &world).await;
        act(&mut tx, &cancelled, "action_cancel").await.unwrap();
        let (draft, confirmed, locked, cancelled) = (
            draft.ids()[0],
            confirmed.ids()[0],
            locked.ids()[0],
            cancelled.ids()[0],
        );
        tx.commit().await.unwrap();
        let orm = policy_orm(&db, &orm).await;
        let c = [world.company];
        let moved = "A sales order line cannot move to another order";

        let mut tx = orm
            .begin(erp_orm::UserId::new(salesman).unwrap())
            .await
            .unwrap();
        let ours = browse(&tx, "sale.order.line", &[draft_line], &c);
        let sold = browse(&tx, "sale.order.line", &[confirmed_line], &c);
        for target in [locked, cancelled, confirmed] {
            refused(
                write(
                    &mut tx,
                    &ours,
                    RawVals::new().set_cell("order_id", Cell::Id(target)),
                )
                .await,
                ErrorKind::UserError,
                moved,
            );
        }
        refused(
            write(
                &mut tx,
                &sold,
                RawVals::new().set_cell("order_id", Cell::Id(draft)),
            )
            .await,
            ErrorKind::UserError,
            moved,
        );
        let quotation = browse(&tx, "sale.order", &[draft], &c);
        for command in [
            Command::link(id(confirmed_line)),
            Command::set([id(draft_line), id(confirmed_line)]),
        ] {
            refused(
                write(
                    &mut tx,
                    &quotation,
                    RawVals::new().set_commands("order_line", [command]),
                )
                .await,
                ErrorKind::UserError,
                moved,
            );
        }
        // a new line given a locked order through the context default is not added
        let lines = tx
            .env()
            .with_context([("default_order_id", Value::Int(locked))])
            .records("sale.order.line")
            .unwrap();
        let added = tx
            .savepoint(async move |tx| {
                lines
                    .create(
                        tx,
                        vec![RawVals::new().set_cell("product_id", Cell::Id(product))],
                    )
                    .await
                    .map(|_| ())
            })
            .await;
        refused(
            added,
            ErrorKind::Validation,
            "is locked; its lines cannot be added",
        );
        // naming the order the line is on changes nothing and passes
        write(
            &mut tx,
            &ours,
            RawVals::new().set_cell("order_id", Cell::Id(draft)),
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();

        let mut tx = orm
            .begin(erp_orm::UserId::new(manager).unwrap())
            .await
            .unwrap();
        let sold = browse(&tx, "sale.order.line", &[confirmed_line], &c);
        refused(
            write(
                &mut tx,
                &sold,
                RawVals::new().set_cell("order_id", Cell::Id(draft)),
            )
            .await,
            ErrorKind::UserError,
            moved,
        );
        tx.rollback().await.unwrap();

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let sold = browse(&tx, "sale.order.line", &[confirmed_line], &c);
        assert_eq!(many2one(&mut tx, &sold, "order_id").await, Some(confirmed));
        let order = browse(&tx, "sale.order", &[confirmed], &c);
        match order.cell(&mut tx, "amount_untaxed").await.unwrap() {
            Cell::Decimal(total) => assert_eq!(total.normalize(), Decimal::from(100)),
            other => panic!("amount_untaxed is {other:?}"),
        }
        tx.rollback().await.unwrap();
    });
}

#[test]
fn the_company_of_a_confirmed_order_or_of_a_quotation_with_lines_is_kept() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let main = tx
            .env()
            .records("res.company")
            .unwrap()
            .browse([world.company])
            .unwrap();
        let currency = many2one(&mut tx, &main, "currency_id").await.unwrap();
        let second = tx
            .env()
            .records("res.company")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Sales Company B".into()))
                        .set_cell("currency_id", Cell::Id(currency)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let both = [world.company, second];
        admin(&mut tx).await;
        let manager = member(&mut tx, "meg", "group_sale_manager", &both).await;
        let salesman = member(&mut tx, "sam", "group_sale_salesman_all_leads", &both).await;
        let product = chair(&mut tx).await;
        let confirmed = new_order(&mut tx, &world).await;
        product_line(&mut tx, &confirmed, product).await;
        act(&mut tx, &confirmed, "action_confirm").await.unwrap();
        let locked = new_order(&mut tx, &world).await;
        product_line(&mut tx, &locked, product).await;
        act(&mut tx, &locked, "action_confirm").await.unwrap();
        act(&mut tx, &locked, "action_lock").await.unwrap();
        let lined = new_order(&mut tx, &world).await;
        product_line(&mut tx, &lined, product).await;
        let empty = new_order(&mut tx, &world).await;
        let (confirmed, locked, lined, empty) = (
            confirmed.ids()[0],
            locked.ids()[0],
            lined.ids()[0],
            empty.ids()[0],
        );
        tx.commit().await.unwrap();
        let orm = policy_orm(&db, &orm).await;
        let to_second = || RawVals::new().set_cell("company_id", Cell::Id(second));
        let confirmed_message = "cannot be changed after it is confirmed";

        let mut tx = orm
            .begin(erp_orm::UserId::new(manager).unwrap())
            .await
            .unwrap();
        for order in [confirmed, locked] {
            let order = browse(&tx, "sale.order", &[order], &both);
            refused(
                write(&mut tx, &order, to_second()).await,
                ErrorKind::Validation,
                confirmed_message,
            );
        }
        let with_lines = browse(&tx, "sale.order", &[lined], &both);
        refused(
            write(&mut tx, &with_lines, to_second()).await,
            ErrorKind::Validation,
            "has lines, whose taxes and prices belong to its company",
        );
        let without_lines = browse(&tx, "sale.order", &[empty], &both);
        write(&mut tx, &without_lines, to_second()).await.unwrap();
        assert_eq!(
            many2one(&mut tx, &without_lines, "company_id").await,
            Some(second)
        );
        tx.commit().await.unwrap();

        let mut tx = orm
            .begin(erp_orm::UserId::new(salesman).unwrap())
            .await
            .unwrap();
        let order = browse(&tx, "sale.order", &[confirmed], &both);
        refused(
            write(&mut tx, &order, to_second()).await,
            ErrorKind::Validation,
            confirmed_message,
        );
        tx.rollback().await.unwrap();

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        for order in [confirmed, locked, lined] {
            let order = browse(&tx, "sale.order", &[order], &both);
            assert_eq!(
                many2one(&mut tx, &order, "company_id").await,
                Some(world.company)
            );
        }
        tx.rollback().await.unwrap();
    });
}
