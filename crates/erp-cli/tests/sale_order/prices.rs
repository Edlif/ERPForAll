// SPDX-License-Identifier: MIT OR Apache-2.0
//! Prices, taxes and amounts of the installed `sale` addon: the line price from the pricelist or
//! the product, the manual price, the discount, the taxes with their fiscal position, the line and
//! order amounts, the order currency and its rate, and the derived fields no client writes
//! (reference behaviour and
//! ).

use erp_fields::{Date, Decimal};

use super::*;

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

async fn amount(tx: &mut Transaction, records: &Records, field: &str) -> Decimal {
    match records.cell(tx, field).await.unwrap() {
        Cell::Decimal(value) => value.normalize(),
        Cell::Int(value) => Decimal::from(value),
        other => panic!("{field} is {other:?}"),
    }
}

async fn ids_of(tx: &mut Transaction, records: &Records, field: &str) -> Vec<i64> {
    match records.cell(tx, field).await.unwrap() {
        Cell::Ids(ids) => ids.iter().map(|id| id.as_real().unwrap().get()).collect(),
        other => panic!("{field} is {other:?}"),
    }
}

/// Creates in the main company, where the accounting records of these cases live.
async fn create(tx: &mut Transaction, model: &str, vals: RawVals) -> i64 {
    let company = xmlid(tx, "base", "main_company").await;
    tx.env()
        .with_company(company)
        .unwrap()
        .records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

async fn tax(tx: &mut Transaction, company: i64, name: &str, percent: &str) -> i64 {
    // a tax needs a tax group, and a plain install ships none
    let group = create(
        tx,
        "account.tax.group",
        RawVals::new().set_cell("name", Cell::Str(format!("{name} group").into())),
    )
    .await;
    // taxes are configured inside a company
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.tax")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("amount_type", Cell::Str("percent".into()))
                    .set_cell("amount", Cell::Decimal(dec(percent)))
                    .set_cell("type_tax_use", Cell::Str("sale".into()))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("tax_group_id", Cell::Id(group)),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// A product in Units at the sales price `price` with the sales taxes `taxes`.
async fn product(tx: &mut Transaction, name: &str, price: &str, taxes: &[i64]) -> i64 {
    let unit = xmlid(tx, "uom", "product_uom_unit").await;
    create(
        tx,
        "product.product",
        RawVals::new()
            .set_cell("name", Cell::Str(name.into()))
            .set_cell("list_price", Cell::Decimal(dec(price)))
            .set_cell("uom_id", Cell::Id(unit))
            .set_commands(
                "taxes_id",
                [Command::set(
                    taxes.iter().map(|t| id(*t)).collect::<Vec<_>>(),
                )],
            ),
    )
    .await
}

/// [`add_line`] in a savepoint, so a refusal leaves the transaction usable.
async fn try_line(
    tx: &mut Transaction,
    order: &Records,
    vals: RawVals,
) -> erp_fields::Result<Records> {
    let vals = vals.set_cell("order_id", Cell::Id(order.ids()[0]));
    tx.savepoint(async move |tx| {
        tx.env()
            .records("sale.order.line")?
            .create(tx, vec![vals])
            .await
    })
    .await
}

async fn add_line(tx: &mut Transaction, order: &Records, vals: RawVals) -> Records {
    tx.env()
        .records("sale.order.line")
        .unwrap()
        .create(
            tx,
            vec![vals.set_cell("order_id", Cell::Id(order.ids()[0]))],
        )
        .await
        .unwrap()
}

#[test]
fn line_prices_come_from_the_pricelist_or_the_product_and_a_manual_price_stays() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let dozen = xmlid(&mut tx, "uom", "product_uom_dozen").await;
        let chair = product(&mut tx, "Chair", "100", &[]).await;
        let company = tx
            .env()
            .records("res.company")
            .unwrap()
            .browse([world.company])
            .unwrap();
        let currency = many2one(&mut tx, &company, "currency_id").await.unwrap();
        let pricelist = create(
            &mut tx,
            "product.pricelist",
            RawVals::new()
                .set_cell("name", Cell::Str("Partners".into()))
                .set_cell("currency_id", Cell::Id(currency)),
        )
        .await;
        create(
            &mut tx,
            "product.pricelist.item",
            RawVals::new()
                .set_cell("pricelist_id", Cell::Id(pricelist))
                .set_cell("applied_on", Cell::Str("0_product_variant".into()))
                .set_cell("product_id", Cell::Id(chair))
                .set_cell("compute_price", Cell::Str("fixed".into()))
                .set_cell("fixed_price", Cell::Decimal(dec("80"))),
        )
        .await;

        let priced = order(
            &mut tx,
            &world,
            RawVals::new().set_cell("pricelist_id", Cell::Id(pricelist)),
        )
        .await;
        let line = add_line(
            &mut tx,
            &priced,
            RawVals::new().set_cell("product_id", Cell::Id(chair)),
        )
        .await;
        assert_eq!(amount(&mut tx, &line, "price_unit").await, dec("80"));
        assert_eq!(
            amount(&mut tx, &line, "technical_price_unit").await,
            dec("80")
        );

        // without a pricelist: the sales price, per unit of the line
        let plain = new_order(&mut tx, &world).await;
        let manual = add_line(
            &mut tx,
            &plain,
            RawVals::new().set_cell("product_id", Cell::Id(chair)),
        )
        .await;
        let automatic = add_line(
            &mut tx,
            &plain,
            RawVals::new().set_cell("product_id", Cell::Id(chair)),
        )
        .await;
        assert_eq!(amount(&mut tx, &automatic, "price_unit").await, dec("100"));
        write(
            &mut tx,
            &manual,
            RawVals::new().set_cell("price_unit", Cell::Decimal(dec("50"))),
        )
        .await
        .unwrap();
        for line in [&manual, &automatic] {
            write(
                &mut tx,
                line,
                RawVals::new().set_cell("product_uom_id", Cell::Id(dozen)),
            )
            .await
            .unwrap();
        }
        assert_eq!(amount(&mut tx, &manual, "price_unit").await, dec("50"));
        assert_eq!(amount(&mut tx, &automatic, "price_unit").await, dec("1200"));
    });
}

#[test]
fn taxes_line_amounts_and_order_totals() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        // taxes belong to the company's country
        let us = xmlid(&mut tx, "base", "us").await;
        let company_partner = xmlid(&mut tx, "base", "main_partner").await;
        tx.env()
            .with_company(world.company)
            .unwrap()
            .records("res.partner")
            .unwrap()
            .browse([company_partner])
            .unwrap()
            .write(&mut tx, RawVals::new().set_cell("country_id", Cell::Id(us)))
            .await
            .unwrap();
        let ten = tax(&mut tx, world.company, "Sales 10%", "10").await;
        let fifteen = tax(&mut tx, world.company, "Sales 15%", "15").await;
        let export = tax(&mut tx, world.company, "Export 0%", "0").await;
        let default = tax(&mut tx, world.company, "Default 5%", "5").await;
        let desk = product(&mut tx, "Desk", "100", &[ten]).await;
        let lamp = product(&mut tx, "Lamp", "100", &[fifteen]).await;
        let untaxed = product(&mut tx, "Voucher", "30", &[]).await;

        // 023 and 024: two lines and the order totals
        let quotation = new_order(&mut tx, &world).await;
        let first = add_line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("product_id", Cell::Id(desk))
                .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(2)))
                .set_cell("discount", Cell::Decimal(dec("10"))),
        )
        .await;
        assert_eq!(ids_of(&mut tx, &first, "tax_ids").await, vec![ten]);
        assert_eq!(amount(&mut tx, &first, "price_subtotal").await, dec("180"));
        assert_eq!(amount(&mut tx, &first, "price_tax").await, dec("18"));
        assert_eq!(amount(&mut tx, &first, "price_total").await, dec("198"));
        add_line(
            &mut tx,
            &quotation,
            RawVals::new().set_cell("product_id", Cell::Id(desk)),
        )
        .await;
        assert_eq!(
            amount(&mut tx, &quotation, "amount_untaxed").await,
            dec("280")
        );
        assert_eq!(amount(&mut tx, &quotation, "amount_tax").await, dec("28"));
        assert_eq!(
            amount(&mut tx, &quotation, "amount_total").await,
            dec("308")
        );
        // a deleted line leaves the totals
        first.unlink(&mut tx).await.unwrap();
        assert_eq!(
            amount(&mut tx, &quotation, "amount_total").await,
            dec("110")
        );

        // 022: the fiscal position maps the product's 15 % to the export tax
        let position = create(
            &mut tx,
            "account.fiscal.position",
            RawVals::new()
                .set_cell("name", Cell::Str("Export".into()))
                .set_cell("company_id", Cell::Id(world.company)),
        )
        .await;
        tx.env()
            .with_company(world.company)
            .unwrap()
            .records("account.tax")
            .unwrap()
            .browse([export])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new()
                    .set_commands("fiscal_position_ids", [Command::set([id(position)])])
                    .set_commands("original_tax_ids", [Command::set([id(fifteen)])]),
            )
            .await
            .unwrap();
        let exported = order(
            &mut tx,
            &world,
            RawVals::new().set_cell("fiscal_position_id", Cell::Id(position)),
        )
        .await;
        let mapped = add_line(
            &mut tx,
            &exported,
            RawVals::new().set_cell("product_id", Cell::Id(lamp)),
        )
        .await;
        assert_eq!(ids_of(&mut tx, &mapped, "tax_ids").await, vec![export]);
        assert_eq!(amount(&mut tx, &mapped, "price_tax").await, dec("0"));

        // 037: the company default reaches only a productless ordinary line
        tx.env()
            .records("res.company")
            .unwrap()
            .browse([world.company])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new().set_cell("account_sale_tax_id", Cell::Id(default)),
            )
            .await
            .unwrap();
        // an ordinary line without a product still needs a unit
        let unit = xmlid(&mut tx, "uom", "product_uom_unit").await;
        let mixed = new_order(&mut tx, &world).await;
        let voucher = add_line(
            &mut tx,
            &mixed,
            RawVals::new().set_cell("product_id", Cell::Id(untaxed)),
        )
        .await;
        let service = add_line(
            &mut tx,
            &mixed,
            RawVals::new()
                .set_cell("name", Cell::Str("Installation".into()))
                .set_cell("product_uom_id", Cell::Id(unit))
                .set_cell("price_unit", Cell::Decimal(dec("40"))),
        )
        .await;
        let note = add_line(
            &mut tx,
            &mixed,
            RawVals::new()
                .set_cell("display_type", Cell::Str("line_note".into()))
                .set_cell("name", Cell::Str("Delivered on site".into())),
        )
        .await;
        assert!(ids_of(&mut tx, &voucher, "tax_ids").await.is_empty());
        assert_eq!(ids_of(&mut tx, &service, "tax_ids").await, vec![default]);
        assert!(ids_of(&mut tx, &note, "tax_ids").await.is_empty());
        assert_eq!(amount(&mut tx, &mixed, "amount_untaxed").await, dec("70"));
        assert_eq!(amount(&mut tx, &mixed, "amount_tax").await, dec("2"));
    });
}

#[test]
fn currency_follows_the_pricelist_and_a_confirmed_pricelist_is_kept() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let companies = tx.env().records("res.company").unwrap();
        let company = companies.browse([world.company]).unwrap();
        let home = many2one(&mut tx, &company, "currency_id").await.unwrap();
        let eur = xmlid(&mut tx, "base", "EUR").await;
        let usd = xmlid(&mut tx, "base", "USD").await;
        let foreign = if home == eur { usd } else { eur };
        tx.env()
            .records("res.currency")
            .unwrap()
            .browse([foreign])
            .unwrap()
            .write(&mut tx, RawVals::new().set_cell("active", Cell::Bool(true)))
            .await
            .unwrap();
        for (currency, rate) in [(home, "1"), (foreign, "0.9")] {
            create(
                &mut tx,
                "res.currency.rate",
                RawVals::new()
                    .set_cell("name", Cell::Date(Date::from_ymd(2026, 1, 1).unwrap()))
                    .set_cell("rate", Cell::Decimal(dec(rate)))
                    .set_cell("currency_id", Cell::Id(currency))
                    .set_cell("company_id", Cell::Id(world.company)),
            )
            .await;
        }
        let pricelist = create(
            &mut tx,
            "product.pricelist",
            RawVals::new()
                .set_cell("name", Cell::Str("Abroad".into()))
                .set_cell("currency_id", Cell::Id(foreign)),
        )
        .await;
        let abroad = order(
            &mut tx,
            &world,
            RawVals::new().set_cell("pricelist_id", Cell::Id(pricelist)),
        )
        .await;
        assert_eq!(
            many2one(&mut tx, &abroad, "currency_id").await,
            Some(foreign)
        );
        assert_eq!(amount(&mut tx, &abroad, "currency_rate").await, dec("0.9"));
        let home_order = new_order(&mut tx, &world).await;
        assert_eq!(
            many2one(&mut tx, &home_order, "currency_id").await,
            Some(home)
        );
        assert_eq!(
            amount(&mut tx, &home_order, "currency_rate").await,
            dec("1")
        );

        // no rule of the foreign pricelist: the sales price would need a conversion, refused
        let lamp = product(&mut tx, "Lamp", "100", &[]).await;
        refused(
            try_line(
                &mut tx,
                &abroad,
                RawVals::new().set_cell("product_id", Cell::Id(lamp)),
            )
            .await
            .map(|_| ()),
            ErrorKind::Validation,
            "The pricelist Abroad is in ",
        );
        refused(
            try_line(
                &mut tx,
                &abroad,
                RawVals::new().set_cell("product_id", Cell::Id(lamp)),
            )
            .await
            .map(|_| ()),
            ErrorKind::Validation,
            "would need a currency conversion",
        );
        // a typed price is the caller's and is kept
        let typed = add_line(
            &mut tx,
            &abroad,
            RawVals::new()
                .set_cell("product_id", Cell::Id(lamp))
                .set_cell("price_unit", Cell::Decimal(dec("95"))),
        )
        .await;
        assert_eq!(amount(&mut tx, &typed, "price_unit").await, dec("95"));
        // a fixed-price rule is in the pricelist currency and prices the line
        create(
            &mut tx,
            "product.pricelist.item",
            RawVals::new()
                .set_cell("pricelist_id", Cell::Id(pricelist))
                .set_cell("applied_on", Cell::Str("0_product_variant".into()))
                .set_cell("product_id", Cell::Id(lamp))
                .set_cell("compute_price", Cell::Str("fixed".into()))
                .set_cell("fixed_price", Cell::Decimal(dec("90"))),
        )
        .await;
        let fixed = add_line(
            &mut tx,
            &abroad,
            RawVals::new().set_cell("product_id", Cell::Id(lamp)),
        )
        .await;
        assert_eq!(amount(&mut tx, &fixed, "price_unit").await, dec("90"));

        act(&mut tx, &abroad, "action_confirm").await.unwrap();
        refused(
            write(
                &mut tx,
                &abroad,
                RawVals::new().set_cell("pricelist_id", Cell::Null),
            )
            .await,
            ErrorKind::UserError,
            "cannot be changed after it is confirmed",
        );
        assert_eq!(
            many2one(&mut tx, &abroad, "pricelist_id").await,
            Some(pricelist)
        );
    });
}

#[test]
fn derived_amounts_are_never_written_by_a_client() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let desk = product(&mut tx, "Desk", "100", &[]).await;
        let quotation = new_order(&mut tx, &world).await;
        let line = add_line(
            &mut tx,
            &quotation,
            RawVals::new().set_cell("product_id", Cell::Id(desk)),
        )
        .await;
        let message = "is computed from the order and its lines and cannot be written";
        refused(
            write(
                &mut tx,
                &quotation,
                RawVals::new().set_cell("amount_total", Cell::Decimal(dec("1"))),
            )
            .await,
            ErrorKind::Validation,
            message,
        );
        refused(
            write(
                &mut tx,
                &line,
                RawVals::new().set_cell("price_subtotal", Cell::Decimal(dec("1"))),
            )
            .await,
            ErrorKind::Validation,
            message,
        );
        // through an order command of a new line, a create with the field, and a context default
        let orders = tx.env().records("sale.order").unwrap();
        let attempts: Vec<(Records, RawVals)> = vec![
            (
                orders.clone(),
                RawVals::new()
                    .set_cell("partner_id", Cell::Id(world.customer))
                    .set_cell("company_id", Cell::Id(world.company))
                    .set_commands(
                        "order_line",
                        [Command::Create(
                            RawVals::new()
                                .set_cell("product_id", Cell::Id(desk))
                                .set_cell("price_total", Cell::Decimal(dec("1"))),
                        )],
                    ),
            ),
            (
                orders.clone(),
                RawVals::new()
                    .set_cell("partner_id", Cell::Id(world.customer))
                    .set_cell("company_id", Cell::Id(world.company))
                    .set_cell("currency_rate", Cell::Decimal(dec("2"))),
            ),
            (
                orders.with_context([("default_amount_untaxed", Value::Int(5))]),
                RawVals::new()
                    .set_cell("partner_id", Cell::Id(world.customer))
                    .set_cell("company_id", Cell::Id(world.company)),
            ),
            (
                tx.env()
                    .records("sale.order.line")
                    .unwrap()
                    .with_context([("default_price_tax", Value::Int(5))]),
                RawVals::new()
                    .set_cell("order_id", Cell::Id(quotation.ids()[0]))
                    .set_cell("product_id", Cell::Id(desk)),
            ),
        ];
        for (model, vals) in attempts {
            let result = tx
                .savepoint(async move |tx| model.create(tx, vec![vals]).await.map(|_| ()))
                .await;
            refused(result, ErrorKind::Validation, message);
        }
        assert_eq!(
            amount(&mut tx, &quotation, "amount_total").await,
            dec("100")
        );
    });
}

#[test]
fn a_line_from_the_form_is_priced_and_a_given_price_wins() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let chair = product(&mut tx, "Chair", "100", &[]).await;
        let desk = product(&mut tx, "Desk", "300", &[]).await;
        let quotation = new_order(&mut tx, &world).await;
        // what the line form sends: its default price, discount and technical price, untouched or typed
        let form_line = |price: &str| {
            RawVals::new()
                .set_cell("product_id", Cell::Id(chair))
                .set_cell("product_uom_qty", Cell::Decimal(Decimal::ONE))
                .set_cell("price_unit", Cell::Decimal(dec(price)))
                .set_cell("technical_price_unit", Cell::Decimal(dec("0")))
                .set_cell("discount", Cell::Decimal(dec("0")))
        };
        write(
            &mut tx,
            &quotation,
            RawVals::new().set_commands(
                "order_line",
                [
                    Command::Create(form_line("0")),
                    Command::Create(form_line("55")),
                ],
            ),
        )
        .await
        .unwrap();
        let lines = quotation.related(&mut tx, "order_line").await.unwrap();
        let ids = lines.ids();
        assert_eq!(ids.len(), 2);
        let (untouched, typed) = (
            lines.browse([ids[0]]).unwrap(),
            lines.browse([ids[1]]).unwrap(),
        );
        assert_eq!(amount(&mut tx, &untouched, "price_unit").await, dec("100"));
        assert_eq!(amount(&mut tx, &typed, "price_unit").await, dec("55"));
        // over RPC: no price is priced, a price given without the technical price is kept
        let plain = add_line(
            &mut tx,
            &quotation,
            RawVals::new().set_cell("product_id", Cell::Id(chair)),
        )
        .await;
        assert_eq!(amount(&mut tx, &plain, "price_unit").await, dec("100"));
        let given = add_line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("product_id", Cell::Id(chair))
                .set_cell("price_unit", Cell::Decimal(dec("70"))),
        )
        .await;
        assert_eq!(amount(&mut tx, &given, "price_unit").await, dec("70"));

        // a price given with a new product is the one kept
        write(
            &mut tx,
            &untouched,
            RawVals::new()
                .set_cell("product_id", Cell::Id(desk))
                .set_cell("price_unit", Cell::Decimal(dec("120"))),
        )
        .await
        .unwrap();
        assert_eq!(amount(&mut tx, &untouched, "price_unit").await, dec("120"));
        // a price typed in the form was set by hand and survives a new product
        write(
            &mut tx,
            &typed,
            RawVals::new().set_cell("product_id", Cell::Id(desk)),
        )
        .await
        .unwrap();
        assert_eq!(amount(&mut tx, &typed, "price_unit").await, dec("55"));
        // a priced line takes the new product's price
        write(
            &mut tx,
            &plain,
            RawVals::new().set_cell("product_id", Cell::Id(desk)),
        )
        .await
        .unwrap();
        assert_eq!(amount(&mut tx, &plain, "price_unit").await, dec("300"));
        assert_eq!(
            amount(&mut tx, &quotation, "amount_untaxed").await,
            dec("545")
        );
        tx.rollback().await.unwrap();
    });
}
