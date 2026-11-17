// SPDX-License-Identifier: MIT OR Apache-2.0

use erp_fields::Decimal;

use super::*;

async fn product(tx: &mut Transaction, name: &str, kind: &str, policy: &str) -> i64 {
    tx.env()
        .records("product.product")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("type", Cell::Str(kind.into()))
                    .set_cell("invoice_policy", Cell::Str(policy.into())),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

async fn line(tx: &mut Transaction, order: &Records, vals: RawVals) -> erp_fields::Result<Records> {
    let order = order.ids()[0];
    let lines = tx.env().records("sale.order.line")?;
    tx.savepoint(async move |tx| {
        let created = lines
            .create(tx, vec![vals.set_cell("order_id", Cell::Id(order))])
            .await?;
        tx.flush().await?;
        Ok(created)
    })
    .await
}

async fn product_line(tx: &mut Transaction, order: &Records, product: i64, qty: i32) -> Records {
    line(
        tx,
        order,
        RawVals::new()
            .set_cell("product_id", Cell::Id(product))
            .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(qty))),
    )
    .await
    .unwrap()
}

async fn number(tx: &mut Transaction, records: &Records, field: &str) -> Decimal {
    match records.cell(tx, field).await.unwrap() {
        Cell::Decimal(value) => value,
        Cell::Int(value) => Decimal::from(value),
        other => panic!("{field} is {other:?}"),
    }
}

async fn maybe_text(tx: &mut Transaction, records: &Records, field: &str) -> Option<String> {
    match records.cell(tx, field).await.unwrap() {
        Cell::Str(value) => Some(value.to_string()),
        Cell::Null | Cell::Bool(false) => None,
        other => panic!("{field} is {other:?}"),
    }
}

#[test]
fn the_delivered_quantity_is_manual_in_sale() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let desk = product(&mut tx, "Desk", "consu", "order").await;
        let quotation = new_order(&mut tx, &world).await;
        let desk_line = product_line(&mut tx, &quotation, desk, 3).await;
        assert_eq!(
            text(&mut tx, &desk_line, "qty_delivered_method").await,
            "manual"
        );
        assert_eq!(
            number(&mut tx, &desk_line, "qty_delivered").await,
            Decimal::ZERO
        );
        write(
            &mut tx,
            &desk_line,
            RawVals::new().set_cell("qty_delivered", Cell::Decimal(Decimal::from(2))),
        )
        .await
        .unwrap();
        assert_eq!(
            number(&mut tx, &desk_line, "qty_delivered").await,
            Decimal::from(2)
        );
        // a section line has no product, unit, price or quantity
        let section = line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("display_type", Cell::Str("line_section".into()))
                .set_cell("name", Cell::Str("Furniture".into())),
        )
        .await
        .unwrap();
        assert_eq!(
            number(&mut tx, &section, "product_uom_qty").await,
            Decimal::ZERO
        );
        assert_eq!(many2one(&mut tx, &section, "product_uom_id").await, None);
        tx.rollback().await.unwrap();
    });
}

#[test]
fn the_quantity_to_invoice_follows_the_policy() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let hours = product(&mut tx, "Consulting", "service", "order").await;
        let boxes = product(&mut tx, "Box", "service", "delivery").await;
        let quotation = new_order(&mut tx, &world).await;
        let ordered = product_line(&mut tx, &quotation, hours, 10).await;
        let delivered = product_line(&mut tx, &quotation, boxes, 10).await;
        assert_eq!(
            number(&mut tx, &ordered, "qty_to_invoice").await,
            Decimal::ZERO,
            "a quotation"
        );
        act(&mut tx, &quotation, "action_confirm").await.unwrap();
        write(
            &mut tx,
            &delivered,
            RawVals::new().set_cell("qty_delivered", Cell::Decimal(Decimal::from(4))),
        )
        .await
        .unwrap();
        assert_eq!(
            number(&mut tx, &ordered, "qty_invoiced").await,
            Decimal::ZERO
        );
        assert_eq!(
            number(&mut tx, &ordered, "qty_to_invoice").await,
            number(&mut tx, &ordered, "product_uom_qty").await
        );
        assert_eq!(
            number(&mut tx, &delivered, "qty_to_invoice").await,
            number(&mut tx, &delivered, "qty_delivered").await
        );
        tx.rollback().await.unwrap();
    });
}

#[test]
fn the_invoice_status_follows_the_lines_and_the_close_flag() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let hours = product(&mut tx, "Consulting", "service", "order").await;
        let quotation = new_order(&mut tx, &world).await;
        let hours_line = product_line(&mut tx, &quotation, hours, 10).await;
        assert_eq!(text(&mut tx, &quotation, "invoice_status").await, "no");
        refused(
            act(&mut tx, &quotation, "action_close_invoicing").await,
            ErrorKind::UserError,
            "Only a confirmed sales order can have its invoicing closed",
        );
        act(&mut tx, &quotation, "action_confirm").await.unwrap();
        assert_eq!(
            text(&mut tx, &hours_line, "invoice_status").await,
            "to invoice"
        );
        assert_eq!(
            text(&mut tx, &quotation, "invoice_status").await,
            "to invoice"
        );
        act(&mut tx, &quotation, "action_close_invoicing")
            .await
            .unwrap();
        assert_eq!(
            text(&mut tx, &quotation, "invoice_status").await,
            "invoiced"
        );
        act(&mut tx, &quotation, "action_reopen_order")
            .await
            .unwrap();
        assert_eq!(
            text(&mut tx, &quotation, "invoice_status").await,
            "to invoice"
        );
        tx.rollback().await.unwrap();
    });
}

#[test]
fn the_delivery_status_follows_the_goods_lines() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let desk = product(&mut tx, "Desk", "consu", "order").await;
        let hours = product(&mut tx, "Consulting", "service", "order").await;
        let services = new_order(&mut tx, &world).await;
        product_line(&mut tx, &services, hours, 1).await;
        act(&mut tx, &services, "action_confirm").await.unwrap();
        assert_eq!(
            maybe_text(&mut tx, &services, "delivery_status").await,
            None
        );
        let quotation = new_order(&mut tx, &world).await;
        let desk_line = product_line(&mut tx, &quotation, desk, 5).await;
        write(
            &mut tx,
            &desk_line,
            RawVals::new().set_cell("qty_delivered", Cell::Decimal(Decimal::from(2))),
        )
        .await
        .unwrap();
        assert_eq!(
            maybe_text(&mut tx, &quotation, "delivery_status").await,
            None,
            "a quotation"
        );
        act(&mut tx, &quotation, "action_confirm").await.unwrap();
        assert_eq!(
            maybe_text(&mut tx, &quotation, "delivery_status")
                .await
                .as_deref(),
            Some("partial")
        );
        write(
            &mut tx,
            &desk_line,
            RawVals::new().set_cell("qty_delivered", Cell::Decimal(Decimal::from(5))),
        )
        .await
        .unwrap();
        assert_eq!(
            maybe_text(&mut tx, &quotation, "delivery_status")
                .await
                .as_deref(),
            Some("full")
        );
        tx.rollback().await.unwrap();
    });
}

#[test]
fn a_line_offers_the_units_of_its_product() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        // the shipped Dozens unit is archived; an archived unit is never offered
        let unit = xmlid(&mut tx, "uom", "product_uom_unit").await;
        let dozen = tx
            .env()
            .records("uom.uom")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Box of 12".into()))
                        .set_cell("relative_uom_id", Cell::Id(unit))
                        .set_cell("relative_factor", Cell::Decimal(Decimal::from(12))),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        // the product is made with its second unit, as on the product form
        let templates = tx
            .env()
            .records("product.template")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Pens".into()))
                        .set_cell("type", Cell::Str("consu".into()))
                        .set_cell("invoice_policy", Cell::Str("order".into()))
                        .set_commands("uom_ids", [Command::set([id(dozen)])]),
                ],
            )
            .await
            .unwrap();
        let pens = templates
            .related(&mut tx, "product_variant_ids")
            .await
            .unwrap()
            .ids()[0];
        let quotation = new_order(&mut tx, &world).await;
        let pens_line = product_line(&mut tx, &quotation, pens, 1).await;
        let main = many2one(&mut tx, &pens_line, "product_uom_id")
            .await
            .unwrap();
        let allowed = |cell: Cell| match cell {
            Cell::Ids(ids) => ids.iter().map(|id| id.get()).collect::<Vec<_>>(),
            other => panic!("allowed_uom_ids is {other:?}"),
        };
        let single = allowed(pens_line.cell(&mut tx, "allowed_uom_ids").await.unwrap());
        assert_eq!(single, [main], "one unit while several units are off");
        let feature = xmlid(&mut tx, "uom", "group_uom").await;
        admin(&mut tx).await;
        tx.env()
            .records("res.users")
            .unwrap()
            .browse(vec![SUPERUSER_ID.get()])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_commands("group_ids", [Command::link(id(feature))]),
            )
            .await
            .unwrap();
        let ids = pens_line.ids();
        tx.commit().await.unwrap();
        // a new transaction: the allowed units are not stored and the feature is global
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let fresh = tx
            .env()
            .records("sale.order.line")
            .unwrap()
            .browse(ids)
            .unwrap();
        let several = allowed(fresh.cell(&mut tx, "allowed_uom_ids").await.unwrap());
        assert!(
            several.contains(&main) && several.contains(&dozen),
            "{several:?}"
        );
        tx.rollback().await.unwrap();
    });
}

#[test]
fn a_locked_order_keeps_its_line_prices() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let desk = product(&mut tx, "Desk", "consu", "order").await;
        let quotation = new_order(&mut tx, &world).await;
        let desk_line = product_line(&mut tx, &quotation, desk, 1).await;
        act(&mut tx, &quotation, "action_confirm").await.unwrap();
        act(&mut tx, &quotation, "action_lock").await.unwrap();
        let before = number(&mut tx, &desk_line, "price_unit").await;
        refused(
            write(
                &mut tx,
                &desk_line,
                RawVals::new().set_cell("price_unit", Cell::Decimal(Decimal::from(99))),
            )
            .await,
            ErrorKind::UserError,
            "These fields of its lines cannot be changed: Unit Price.",
        );
        assert_eq!(number(&mut tx, &desk_line, "price_unit").await, before);
        // the lead time is not protected
        write(
            &mut tx,
            &desk_line,
            RawVals::new().set_cell("customer_lead", Cell::Int(3)),
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();
    });
}

/// The structural guards of a line (B3): the kind of a line does not change, the product
/// of a confirmed order's delivered line does not change, a confirmed order keeps its product
/// lines and lets its sections go.
#[test]
fn line_guards_on_kind_product_and_delete() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let desk = product(&mut tx, "Desk", "consu", "order").await;
        let chair = product(&mut tx, "Chair", "consu", "order").await;
        let quotation = new_order(&mut tx, &world).await;
        let desk_line = product_line(&mut tx, &quotation, desk, 1).await;
        let note = line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("display_type", Cell::Str("line_note".into()))
                .set_cell("name", Cell::Str("Fragile".into())),
        )
        .await
        .unwrap();
        refused(
            write(
                &mut tx,
                &note,
                RawVals::new().set_cell("display_type", Cell::Str("line_section".into())),
            )
            .await,
            ErrorKind::UserError,
            "The kind of a sales order line cannot change",
        );
        let subsection = line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("display_type", Cell::Str("line_subsection".into()))
                .set_cell("name", Cell::Str("Desks".into())),
        )
        .await
        .unwrap();
        write(
            &mut tx,
            &subsection,
            RawVals::new().set_cell("display_type", Cell::Str("line_section".into())),
        )
        .await
        .unwrap();
        // a product line needs a unit
        let unitless = line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("name", Cell::Str("Loose".into()))
                .set_cell("product_uom_id", Cell::Null),
        )
        .await;
        assert!(unitless.is_err(), "{unitless:?}");
        act(&mut tx, &quotation, "action_confirm").await.unwrap();
        write(
            &mut tx,
            &desk_line,
            RawVals::new().set_cell("qty_delivered", Cell::Decimal(Decimal::from(1))),
        )
        .await
        .unwrap();
        refused(
            write(
                &mut tx,
                &desk_line,
                RawVals::new().set_cell("product_id", Cell::Id(chair)),
            )
            .await,
            ErrorKind::UserError,
            "The product of these sales order lines can no longer be changed",
        );
        let result = tx.savepoint(async |tx| desk_line.unlink(tx).await).await;
        refused(
            result,
            ErrorKind::UserError,
            "set its quantity to 0 instead",
        );
        tx.savepoint(async |tx| note.unlink(tx).await)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
    });
}

/// A product created with its name only, an order with customer and company only and a product
/// line with product and quantity only are saved: the policy, the addresses, the description and
/// the unit are required (or needed by a check) at the INSERT and are set before it.
#[test]
fn the_least_values_create_a_product_an_order_and_a_line() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let lamp = tx
            .env()
            .records("product.product")
            .unwrap()
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", Cell::Str("Lamp".into()))],
            )
            .await
            .unwrap();
        let company = tx
            .env()
            .records("res.company")
            .unwrap()
            .browse(vec![world.company])
            .unwrap();
        assert_eq!(
            text(&mut tx, &lamp, "invoice_policy").await,
            text(&mut tx, &company, "sale_invoice_policy").await
        );
        let quotation = new_order(&mut tx, &world).await;
        for field in ["partner_invoice_id", "partner_shipping_id"] {
            assert_eq!(
                many2one(&mut tx, &quotation, field).await,
                Some(world.customer),
                "{field}"
            );
        }
        let lamp_line = product_line(&mut tx, &quotation, lamp.ids()[0], 2).await;
        assert_eq!(
            text(&mut tx, &lamp_line, "name").await,
            text(&mut tx, &lamp, "display_name").await
        );
        assert_eq!(
            many2one(&mut tx, &lamp_line, "product_uom_id").await,
            many2one(&mut tx, &lamp, "uom_id").await
        );
        assert!(
            many2one(&mut tx, &lamp_line, "product_uom_id")
                .await
                .is_some()
        );
        tx.rollback().await.unwrap();
    });
}

#[test]
fn derived_line_fields_are_refused() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let desk = product(&mut tx, "Desk", "consu", "order").await;
        let quotation = new_order(&mut tx, &world).await;
        let desk_line = product_line(&mut tx, &quotation, desk, 1).await;
        act(&mut tx, &quotation, "action_confirm").await.unwrap();
        let order = quotation.ids()[0];
        let lines = tx.env().records("sale.order.line").unwrap();
        let derived: [(&str, Cell, Value); 9] = [
            (
                "state",
                Cell::Str("draft".into()),
                Value::Str("draft".into()),
            ),
            (
                "company_id",
                Cell::Id(world.company),
                Value::Int(world.company),
            ),
            (
                "order_partner_id",
                Cell::Id(world.customer),
                Value::Int(world.customer),
            ),
            (
                "salesman_id",
                Cell::Id(SUPERUSER_ID.get()),
                Value::Int(SUPERUSER_ID.get()),
            ),
            ("is_downpayment", Cell::Bool(true), Value::Bool(true)),
            (
                "qty_delivered_method",
                Cell::Str("manual".into()),
                Value::Str("manual".into()),
            ),
            ("qty_invoiced", Cell::Int(1), Value::Int(1)),
            ("qty_to_invoice", Cell::Int(0), Value::Int(0)),
            (
                "invoice_status",
                Cell::Str("invoiced".into()),
                Value::Str("invoiced".into()),
            ),
        ];
        let written =
            "of a sales order line is set from its order and its operations, not written.";
        for (field, cell, value) in derived {
            for target in [desk_line.clone(), desk_line.sudo(true)] {
                refused(
                    write(
                        &mut tx,
                        &target,
                        RawVals::new().set_cell(field, cell.clone()),
                    )
                    .await,
                    ErrorKind::Validation,
                    written,
                );
            }
            for model in [lines.clone(), lines.sudo(true)] {
                let vals = RawVals::new()
                    .set_cell("order_id", Cell::Id(order))
                    .set_cell("product_id", Cell::Id(desk))
                    .set_cell(field, cell.clone());
                let result = tx
                    .savepoint(async |tx| model.create(tx, vec![vals.clone()]).await.map(|_| ()))
                    .await;
                refused(result, ErrorKind::Validation, written);
            }
            let update = Command::update(
                desk_line.rec_ids()[0],
                RawVals::new().set_cell(field, cell.clone()),
            );
            let add = Command::create(
                RawVals::new()
                    .set_cell("product_id", Cell::Id(desk))
                    .set_cell(field, cell.clone()),
            );
            for command in [update, add] {
                refused(
                    write(
                        &mut tx,
                        &quotation,
                        RawVals::new().set_commands("order_line", [command]),
                    )
                    .await,
                    ErrorKind::Validation,
                    written,
                );
            }
            let defaulted = lines.with_context([(format!("default_{field}"), value)]);
            let vals = RawVals::new()
                .set_cell("order_id", Cell::Id(order))
                .set_cell("product_id", Cell::Id(desk));
            let result = tx
                .savepoint(async |tx| defaulted.create(tx, vec![vals.clone()]).await.map(|_| ()))
                .await;
            refused(
                result,
                ErrorKind::Validation,
                &format!("The context key default_{field} is not accepted"),
            );
        }
        // the confirmed order's product line is still kept
        let result = tx.savepoint(async |tx| desk_line.unlink(tx).await).await;
        refused(
            result,
            ErrorKind::UserError,
            "set its quantity to 0 instead",
        );
        // a copy carries the lines without the derived fields
        let copied = quotation.copy(&mut tx, None).await.unwrap();
        let copied_lines = copied.related(&mut tx, "order_line").await.unwrap();
        assert_eq!(copied_lines.ids().len(), 1);
        assert_eq!(text(&mut tx, &copied_lines, "state").await, "draft");
        tx.rollback().await.unwrap();
    });
}

#[test]
fn lines_of_cancelled_and_locked_orders_do_not_change() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let hours = product(&mut tx, "Consulting", "service", "order").await;
        let quotation = new_order(&mut tx, &world).await;
        let name = text(&mut tx, &quotation, "name").await;
        let hours_line = product_line(&mut tx, &quotation, hours, 2).await;
        let note = line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("display_type", Cell::Str("line_note".into()))
                .set_cell("name", Cell::Str("Remote".into())),
        )
        .await
        .unwrap();
        act(&mut tx, &quotation, "action_confirm").await.unwrap();
        act(&mut tx, &quotation, "action_lock").await.unwrap();
        let hours_vals = || {
            RawVals::new()
                .set_cell("product_id", Cell::Id(hours))
                .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(1)))
        };
        refused(
            line(&mut tx, &quotation, hours_vals()).await.map(|_| ()),
            ErrorKind::Validation,
            &format!("The sales order {name} is locked; its lines cannot be added."),
        );
        refused(
            write(
                &mut tx,
                &quotation,
                RawVals::new().set_commands("order_line", [Command::create(hours_vals())]),
            )
            .await,
            ErrorKind::Validation,
            "These fields cannot be changed: Order Lines",
        );
        let result = tx.savepoint(async |tx| note.unlink(tx).await).await;
        refused(
            result,
            ErrorKind::Validation,
            &format!("The sales order {name} is locked; its lines cannot be deleted."),
        );
        act(&mut tx, &quotation, "action_unlock").await.unwrap();
        act(&mut tx, &quotation, "action_cancel").await.unwrap();
        let cancelled = format!("The sales order {name} is cancelled; its lines cannot be");
        for (target, vals) in [
            (
                &hours_line,
                RawVals::new().set_cell("price_unit", Cell::Decimal(Decimal::from(7))),
            ),
            (
                &hours_line,
                RawVals::new().set_cell("product_uom_qty", Cell::Decimal(Decimal::from(9))),
            ),
            (
                &note,
                RawVals::new().set_cell("name", Cell::Str("Onsite".into())),
            ),
        ] {
            refused(
                write(&mut tx, target, vals).await,
                ErrorKind::Validation,
                &format!("{cancelled} changed."),
            );
        }
        refused(
            line(&mut tx, &quotation, hours_vals()).await.map(|_| ()),
            ErrorKind::Validation,
            &format!("{cancelled} added."),
        );
        for target in [&hours_line, &note] {
            let result = tx.savepoint(async |tx| target.unlink(tx).await).await;
            refused(
                result,
                ErrorKind::Validation,
                &format!("{cancelled} deleted."),
            );
        }
        // back to a quotation, the lines change again
        act(&mut tx, &quotation, "action_draft").await.unwrap();
        write(
            &mut tx,
            &hours_line,
            RawVals::new().set_cell("price_unit", Cell::Decimal(Decimal::from(7))),
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();
    });
}
