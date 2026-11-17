// SPDX-License-Identifier: MIT OR Apache-2.0
//! The printed sales documents of the installed `sale` addon (`addons/sale/views/sale_reports.xml`):
//! the quotation / sales order and the pro-forma invoice, printed through the report service and
//! read back from the HTML preview and the PDF; and both printed by a salesperson.

use erp_fields::Decimal;
use erp_report::LayoutEngine;

use super::prices::{add_line, dec, product, tax};
use super::*;

/// The HTML preview and the PDF of `report` over the orders `ids`, printed as a user of company 1;
/// the PDF is kept in the target directory under `name` for a look at the layout.
async fn print(
    tx: &mut Transaction,
    report: &str,
    ids: &[i64],
    name: &str,
) -> (String, Vec<String>) {
    let found = erp_addon_base::report_print::find(tx, report)
        .await
        .unwrap();
    let document = erp_addon_base::report_print::prepare(tx, &found, ids)
        .await
        .unwrap();
    let html = erp_report::render_html(&document, &[]).unwrap();
    let fonts = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../addons/report_fonts")
        .canonicalize()
        .unwrap();
    let engine = erp_report_typst::TypstEngine::new(&fonts, erp_report::Limits::default()).unwrap();
    let pdf = engine
        .render(&crate::report_frame::through_frame(&document))
        .unwrap();
    std::fs::write(
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("sale-{name}.pdf")),
        pdf.bytes(),
    )
    .unwrap();
    let pdf = lopdf::Document::load_mem(pdf.bytes()).unwrap();
    let pages = pdf
        .get_pages()
        .keys()
        .map(|page| {
            let text = pdf.extract_text(&[*page]).unwrap();
            text.chars().filter(|c| !c.is_whitespace()).collect()
        })
        .collect();
    (html, pages)
}

async fn partner_at(
    tx: &mut Transaction,
    name: &str,
    street: &str,
    parent: Option<(i64, &str)>,
) -> i64 {
    let mut vals = RawVals::new()
        .set_cell("name", Cell::Str(name.into()))
        .set_cell("street", Cell::Str(street.into()))
        .set_cell("zip", Cell::Str("40112".into()))
        .set_cell("city", Cell::Str("Bandung".into()));
    if let Some((parent, kind)) = parent {
        vals = vals
            .set_cell("parent_id", Cell::Id(parent))
            .set_cell("type", Cell::Str(kind.into()));
    } else {
        vals = vals.set_cell("is_company", Cell::Bool(true));
    }
    partner(tx, vals).await
}

/// The quotation, the confirmed order, the cancelled quotation and the pro-forma invoice print with
/// their title, addresses, lines, a row per tax group and the closing blocks; sixty lines run over
/// several pages with the table header repeated and the totals on the last page; both reports are in
/// the Print menu of sales orders.
#[test]
fn the_sales_documents_print_a_quotation_an_order_and_a_pro_forma() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm
            .begin_with(SUPERUSER_ID, Context::new().with_company_id(1))
            .await
            .unwrap();
        let mut world = world(&mut tx).await;
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
        tx.env()
            .records("res.company")
            .unwrap()
            .browse(vec![world.company])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new()
                    .set_cell("street", Cell::Str("Jalan Merdeka 12".into()))
                    .set_cell("zip", Cell::Str("40111".into()))
                    .set_cell("city", Cell::Str("Bandung".into()))
                    .set_cell("phone", Cell::Str("+62 22 555 0100".into()))
                    .set_cell("email", Cell::Str("office@example.com".into())),
            )
            .await
            .unwrap();
        let ten = tax(&mut tx, world.company, "VAT 10%", "10").await;
        let fifteen = tax(&mut tx, world.company, "Luxury 15%", "15").await;
        let desk = product(&mut tx, "Desk", "100", &[ten]).await;
        let lamp = product(&mut tx, "Lamp", "100", &[fifteen]).await;
        let voucher = product(&mut tx, "Voucher", "10", &[]).await;

        // The Print menu of sales orders lists both documents.
        let listed = erp_addon_base::report_print::printable(&mut tx, "sale.order")
            .await
            .unwrap();
        for entry in [
            ("sale.report_saleorder", "Quotation / Order"),
            ("sale.report_saleorder_pro_forma", "PRO-FORMA Invoice"),
        ] {
            assert!(
                listed.contains(&(entry.0.to_owned(), entry.1.to_owned())),
                "{listed:?}"
            );
        }

        // A company customer whose invoice and delivery contacts differ from it.
        let deco = partner_at(&mut tx, "Deco Interior", "Jalan Asia Afrika 8", None).await;
        partner_at(
            &mut tx,
            "Deco billing",
            "Jalan Braga 21",
            Some((deco, "invoice")),
        )
        .await;
        partner_at(
            &mut tx,
            "Deco dock",
            "Jalan Cihampelas 5",
            Some((deco, "delivery")),
        )
        .await;
        world.customer = deco;
        let quotation = order(
            &mut tx,
            &world,
            RawVals::new()
                .set_cell("client_order_ref", Cell::Str("PO-7781".into()))
                .set_cell("user_id", Cell::Id(SUPERUSER_ID.into()))
                .set_cell(
                    "validity_date",
                    Cell::Date(erp_fields::Date::from_ymd(2026, 12, 17).unwrap()),
                )
                .set_cell(
                    "note",
                    Cell::Str("<p>Payment within 30 days of delivery.</p>".into()),
                ),
        )
        .await;
        add_line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("display_type", Cell::Str("line_section".into()))
                .set_cell("name", Cell::Str("Hardware".into())),
        )
        .await;
        add_line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("product_id", Cell::Id(desk))
                .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(2)))
                .set_cell("discount", Cell::Decimal(dec("10"))),
        )
        .await;
        add_line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("product_id", Cell::Id(lamp))
                .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(1))),
        )
        .await;
        add_line(
            &mut tx,
            &quotation,
            RawVals::new()
                .set_cell("display_type", Cell::Str("line_note".into()))
                .set_cell("name", Cell::Str("Delivered in two parts".into())),
        )
        .await;
        tx.flush().await.unwrap();
        let q = quotation.ids()[0];
        let number = text(&mut tx, &quotation, "name").await;

        let (html, pages) = print(&mut tx, "sale.report_saleorder", &[q], "quotation").await;
        for expected in [
            "Quotation",
            number.as_str(),
            "Draft",
            "Customer",
            "Deco Interior",
            "Jalan Asia Afrika 8",
            "Invoice Address",
            "Jalan Braga 21",
            "Delivery Address",
            "Jalan Cihampelas 5",
            "Quotation Date",
            "Salesperson",
            "Expiration",
            "12/17/2026",
            "Your Reference",
            "PO-7781",
            "Hardware",
            "Desk",
            "Disc. %",
            "Taxes",
            "VAT 10%",
            "Delivered in two parts",
            "Untaxed Amount",
            "VAT 10% group",
            "Luxury 15% group",
            "18.00",
            "15.00",
            "280.00",
            "313.00",
            "Terms and Conditions",
            "Payment within 30 days of delivery.",
            "Customer Signature",
            // the company letterhead
            "Jalan Merdeka 12",
        ] {
            assert!(html.contains(expected), "{expected}: {html}");
        }
        // The unit price is a plain number with the price decimals; the amounts carry the currency.
        assert!(html.contains("100.00"), "{html}");
        // The rich-text terms print without their paragraph tags.
        assert!(
            !html.contains("&lt;p&gt;") && !html.contains("<p>Payment"),
            "{html}"
        );
        assert_eq!(pages.len(), 1, "{pages:?}");
        assert!(pages[0].contains("Quotation"), "{}", pages[0]);
        assert!(pages[0].contains("313.00"), "{}", pages[0]);
        // The PDF text has the terms without their tags.
        assert!(
            pages[0].contains("Paymentwithin30daysofdelivery.") && !pages[0].contains("<p>"),
            "{}",
            pages[0]
        );
        // Terms typed with `&` and `<`, a paragraph with an attribute, bold text, a line break and
        // a non-breaking space print as plain text; an emptied editor value prints no terms block.
        let typed = order(
            &mut tx,
            &world,
            RawVals::new().set_cell(
                "note",
                Cell::Str(
                    "<p class=\"x\">Prices in USD &amp; excl. VAT.</p><p>Orders of 5 &lt; 10 \
                     units: <strong>no discount</strong>.<br>Ask&nbsp;us.</p>"
                        .into(),
                ),
            ),
        )
        .await;
        let (html, pages) = print(&mut tx, "sale.report_saleorder", &typed.ids(), "terms").await;
        assert!(
            pages[0].contains("PricesinUSD&excl.VAT.Ordersof5<10units:nodiscount.Askus."),
            "{}",
            pages[0]
        );
        for markup in ["&amp;", "&lt;", "&nbsp;", "<p", "<strong", "class", "<br"] {
            assert!(!pages[0].contains(markup), "{markup}: {}", pages[0]);
        }
        assert!(
            !html.contains("&amp;amp;") && !html.contains("&lt;strong"),
            "{html}"
        );
        let emptied = order(
            &mut tx,
            &world,
            RawVals::new().set_cell("note", Cell::Str("<p><br></p>".into())),
        )
        .await;
        let (html, _) = print(&mut tx, "sale.report_saleorder", &emptied.ids(), "no-terms").await;
        assert!(!html.contains("Terms and Conditions"), "{html}");

        // The same order confirmed is a sales order; the pro-forma invoice prints the same data.
        act(&mut tx, &quotation, "action_confirm").await.unwrap();
        let (html, _) = print(&mut tx, "sale.report_saleorder", &[q], "order").await;
        for expected in ["Sales Order", number.as_str(), "Order Date"] {
            assert!(html.contains(expected), "{expected}: {html}");
        }
        assert!(
            !html.contains("Draft") && !html.contains("Quotation Date"),
            "{html}"
        );
        let (html, _) = print(
            &mut tx,
            "sale.report_saleorder_pro_forma",
            &[q],
            "pro-forma",
        )
        .await;
        for expected in [
            "Pro-Forma Invoice",
            number.as_str(),
            "Issued Date",
            "313.00",
        ] {
            assert!(html.contains(expected), "{expected}: {html}");
        }

        // A cancelled quotation says so and has no signature block; one address, no taxes.
        world.customer = partner_at(&mut tx, "Sinar Abadi", "Jalan Dago 3", None).await;
        let cancelled = new_order(&mut tx, &world).await;
        add_line(
            &mut tx,
            &cancelled,
            RawVals::new()
                .set_cell("product_id", Cell::Id(voucher))
                .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(3))),
        )
        .await;
        act(&mut tx, &cancelled, "action_cancel").await.unwrap();
        let (html, _) = print(
            &mut tx,
            "sale.report_saleorder",
            &cancelled.ids(),
            "cancelled",
        )
        .await;
        for expected in ["Cancelled Quotation", "Sinar Abadi", "Voucher", "30.00"] {
            assert!(html.contains(expected), "{expected}: {html}");
        }
        for absent in [
            "Customer Signature",
            "Invoice Address",
            "Delivery Address",
            "Disc. %",
        ] {
            assert!(!html.contains(absent), "{absent}: {html}");
        }
        // The pro-forma invoice of the cancelled quotation keeps the cancelled mark.
        let (_, pages) = print(
            &mut tx,
            "sale.report_saleorder_pro_forma",
            &cancelled.ids(),
            "cancelled-pro-forma",
        )
        .await;
        assert!(
            pages[0].contains("Pro-FormaInvoice") && pages[0].contains("Cancelled"),
            "{}",
            pages[0]
        );

        // Sixty lines over several pages: the table header repeats, no line is split, the totals and
        // the closing blocks print once, on the last page.
        let long = new_order(&mut tx, &world).await;
        for n in 1..=60 {
            add_line(
                &mut tx,
                &long,
                RawVals::new()
                    .set_cell("product_id", Cell::Id(voucher))
                    .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(1)))
                    .set_cell(
                        "name",
                        Cell::Str(format!("Rowstart{n}a line description long enough to wrap onto a second line of the cell before it ends Rowend{n}.").into()),
                    ),
            )
            .await;
        }
        tx.flush().await.unwrap();
        let (_, pages) = print(&mut tx, "sale.report_saleorder", &long.ids(), "long").await;
        assert!(pages.len() >= 2, "{}", pages.len());
        let last = pages.len() - 1;
        for (index, page) in pages.iter().enumerate() {
            if page.contains("Rowstart") {
                assert!(
                    page.contains("DescriptionQuantityUnitPriceAmount"),
                    "page {index}: {page}"
                );
            }
            for closing in ["Total", "CustomerSignature"] {
                assert_eq!(
                    page.contains(closing),
                    index == last,
                    "{closing} on page {index}"
                );
            }
        }
        for n in 1..=60 {
            let page = pages
                .iter()
                .position(|p| p.contains(&format!("Rowstart{n}a")))
                .unwrap_or_else(|| panic!("line {n} is printed"));
            assert!(
                pages[page].contains(&format!("Rowend{n}.")),
                "line {n} ends on page {page}"
            );
        }

        // Amounts in the millions in a currency with a long symbol: no cell wraps, the lines fill
        // two pages, the totals come last.
        let idr = xmlid(&mut tx, "base", "IDR").await;
        let companies = tx.env().records("res.company").unwrap();
        let company = companies.browse([world.company]).unwrap();
        let home = many2one(&mut tx, &company, "currency_id").await.unwrap();
        tx.env()
            .records("res.currency")
            .unwrap()
            .browse([idr])
            .unwrap()
            .write(&mut tx, RawVals::new().set_cell("active", Cell::Bool(true)))
            .await
            .unwrap();
        for (currency, rate) in [(home, "1"), (idr, "16000")] {
            tx.env()
                .with_company(world.company)
                .unwrap()
                .records("res.currency.rate")
                .unwrap()
                .create(
                    &mut tx,
                    vec![
                        RawVals::new()
                            .set_cell(
                                "name",
                                Cell::Date(erp_fields::Date::from_ymd(2026, 1, 1).unwrap()),
                            )
                            .set_cell("rate", Cell::Decimal(dec(rate)))
                            .set_cell("currency_id", Cell::Id(currency))
                            .set_cell("company_id", Cell::Id(world.company)),
                    ],
                )
                .await
                .unwrap();
        }
        let pricelist = tx
            .env()
            .records("product.pricelist")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Rupiah".into()))
                        .set_cell("currency_id", Cell::Id(idr)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let rupiah = order(
            &mut tx,
            &world,
            RawVals::new().set_cell("pricelist_id", Cell::Id(pricelist)),
        )
        .await;
        for n in 1..=30 {
            add_line(
                &mut tx,
                &rupiah,
                RawVals::new()
                    .set_cell("product_id", Cell::Id(desk))
                    .set_cell("product_uom_qty", Cell::Decimal(Decimal::from(3)))
                    .set_cell("price_unit", Cell::Decimal(dec("12500000")))
                    .set_cell("discount", Cell::Decimal(dec(if n == 1 { "10" } else { "0" })))
                    .set_cell(
                        "name",
                        Cell::Str(format!("Executive desk {n}, walnut veneer, 180 x 90 cm, delivered and assembled on site").into()),
                    ),
            )
            .await;
        }
        tx.flush().await.unwrap();
        let (html, pages) = print(&mut tx, "sale.report_saleorder", &rupiah.ids(), "rupiah").await;
        for expected in [
            "Rp",
            "12,500,000.00",
            "37,500,000.00",
            "33,750,000.00",
            "1,121,250,000.00",
        ] {
            assert!(html.contains(expected), "{expected}: {html}");
        }
        assert!(pages.len() >= 2, "{}", pages.len());
        tx.rollback().await.unwrap();
    });
}

/// A salesperson who sees own documents only prints an own quotation in both reports; another
/// salesperson's quotation and a quotation of another company are refused. A user of all documents
/// prints a colleague's quotation with the colleague's name as salesperson.
#[test]
fn a_salesperson_prints_own_quotations_only() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        // a second company and its quotation, made outside a company session: the work the commit
        // does for them needs a caller who may act for that company
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let world = world(&mut tx).await;
        let currency: i64 = tx
            .query(
                "SELECT currency_id::int8 FROM res_company WHERE id = $1",
                &[&world.company],
            )
            .await
            .unwrap()[0]
            .get(0);
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
        // an unassigned quotation of another company: only the company keeps it from the seller
        let elsewhere = tx
            .env()
            .with_company(second)
            .unwrap()
            .records("sale.order")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("partner_id", Cell::Id(world.customer))
                        .set_cell("company_id", Cell::Id(second)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.env()
            .records("sale.order")
            .unwrap()
            .browse([elsewhere])
            .unwrap()
            .write(&mut tx, RawVals::new().set_cell("user_id", Cell::Null))
            .await
            .unwrap();
        let unassigned: Option<i64> = tx
            .query(
                "SELECT user_id::int8 FROM sale_order WHERE id = $1",
                &[&elsewhere],
            )
            .await
            .unwrap()[0]
            .get(0);
        assert_eq!(unassigned, None);
        tx.commit().await.unwrap();
        let mut tx = orm
            .begin_with(SUPERUSER_ID, Context::new().with_company_id(world.company))
            .await
            .unwrap();
        report_access::admin(&mut tx, world.company).await;
        let salesman = [("sales_team", "group_sale_salesman")];
        let seller = report_access::user(&mut tx, "report-seller", world.company, &salesman).await;
        let colleague =
            report_access::user(&mut tx, "report-colleague", world.company, &salesman).await;
        let manager = report_access::user(
            &mut tx,
            "report-all-documents",
            world.company,
            &[("sales_team", "group_sale_salesman_all_leads")],
        )
        .await;
        let own = order(
            &mut tx,
            &world,
            RawVals::new().set_cell("user_id", Cell::Id(seller)),
        )
        .await
        .ids()[0];
        let theirs = order(
            &mut tx,
            &world,
            RawVals::new().set_cell("user_id", Cell::Id(colleague)),
        )
        .await
        .ids()[0];
        tx.commit().await.unwrap();
        let users = report_access::policy_orm(&db, &orm).await;
        for report in ["sale.report_saleorder", "sale.report_saleorder_pro_forma"] {
            let pages = report_access::print_as(&users, seller, world.company, report, &[own])
                .await
                .unwrap_or_else(|e| panic!("{report} as a salesperson: {e:?}"));
            assert!(pages[0].contains("Azure"), "{}", pages[0]);
            report_access::refused(
                report_access::print_as(&users, seller, world.company, report, &[theirs]).await,
                &format!("{report} of another salesperson"),
            );
            // a user of all documents prints a colleague's quotation with that salesperson's name
            let pages = report_access::print_as(&users, manager, world.company, report, &[theirs])
                .await
                .unwrap_or_else(|e| panic!("{report} as a user of all documents: {e:?}"));
            assert!(
                pages[0].contains("Salesperson") && pages[0].contains("report-colleague"),
                "{}",
                pages[0]
            );
            report_access::refused(
                report_access::print_as(&users, seller, world.company, report, &[elsewhere]).await,
                &format!("{report} of another company"),
            );
        }
        drop(users);
        db.cleanup().await.unwrap();
    });
}
