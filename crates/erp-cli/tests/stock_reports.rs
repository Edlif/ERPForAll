// SPDX-License-Identifier: MIT OR Apache-2.0
//! The printed inventory documents (`addons/stock/views/stock_reports.xml`) on a real install:
//! delivery slip, receipt and picking operations of a transfer, printed through the report service
//! and read back from the PDF: a draft delivery, a ready and a validated receipt, and a long
//! transfer over several pages; and the three documents printed by an inventory user.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::fs;
use std::path::{Path, PathBuf};

use erp_modules::system::{InitOptions, ModuleSystem};
use erp_modules::{AddonIndex, ModuleName};
use erp_report::LayoutEngine;
use erp_sdk::prelude::*;
use erp_sdk::{Context, Orm, SUPERUSER_ID};
use erp_testkit::TestDb;

#[path = "support/report_access.rs"]
mod report_access;
#[path = "support/report_frame.rs"]
mod report_frame;

fn big_stack<F: std::future::Future<Output = ()> + 'static>(test: fn() -> F) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(test());
        })
        .unwrap()
        .join()
        .unwrap();
}

fn addons_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("addons")
}

/// A fresh database with `stock` installed.
async fn install() -> (TestDb, Orm) {
    let db = TestDb::create().await.unwrap();
    let mut index = AddonIndex::new(true);
    index.add_dev_path(addons_dir());
    let identities = index
        .discover_entries()
        .unwrap()
        .into_iter()
        .map(|(name, entry)| {
            let snapshot = erp_modules::artifact_identity::ArtifactSnapshot::capture(
                name.as_str(),
                Path::new(entry.source_location()),
            )
            .unwrap();
            (name, snapshot.digest().to_owned())
        })
        .collect();
    let system = ModuleSystem::builder(db.pool().clone(), index)
        .view_types(erp_addon_web::view_type_contributions())
        .native_artifact_identities(identities)
        .require_sql_objects([&erp_addon_stock::schema_guards::MANIFEST])
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_barcodes::ADDON,
            &erp_addon_barcodes_gs1_nomenclature::ADDON,
            &erp_addon_uom::ADDON,
            &erp_addon_product::ADDON,
            &erp_addon_stock::ADDON,
        ])
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .require_core_tables(erp_security::auth::core_tables())
        .build()
        .unwrap();
    let init = Box::pin(system.initialize(InitOptions::new().with_skip_auto_install(true)))
        .await
        .unwrap();
    let env = init.orm.env(SUPERUSER_ID, Context::new());
    let outcome =
        Box::pin(system.button_immediate_install(&env, &[ModuleName::new("stock").unwrap()]))
            .await
            .unwrap();
    (db, outcome.orm)
}

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

async fn one(tx: &mut Transaction, sql: &str) -> i64 {
    tx.query(sql, &[]).await.unwrap()[0].get(0)
}

async fn create(tx: &mut Transaction, company: i64, model: &str, vals: RawVals) -> i64 {
    tx.env()
        .with_company(company)
        .unwrap()
        .records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap_or_else(|e| panic!("create {model}: {e:?}"))
        .ids()[0]
}

async fn product(tx: &mut Transaction, company: i64, name: &str) -> i64 {
    let template = create(
        tx,
        company,
        "product.template",
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("is_storable", Cell::Bool(true)),
    )
    .await;
    tx.query(
        "SELECT id FROM product_product WHERE product_tmpl_id = $1",
        &[&template],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// A transfer of the company's first operation type with `code`, with one move per `(product,
/// quantity)` between the type's default locations.
async fn picking(
    tx: &mut Transaction,
    company: i64,
    code: &str,
    name: &str,
    partner: i64,
    origin: &str,
    moves: &[(i64, String)],
) -> i64 {
    let row = &tx
        .query(
            "SELECT id, default_location_src_id, default_location_dest_id FROM stock_picking_type \
             WHERE code = $1 AND company_id = $2 ORDER BY id LIMIT 1",
            &[&code, &company],
        )
        .await
        .unwrap()[0];
    let (kind, src, dest): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
    let id = create(
        tx,
        company,
        "stock.picking",
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("company_id", Cell::Id(company))
            .set_cell("picking_type_id", Cell::Id(kind))
            .set_cell("location_id", Cell::Id(src))
            .set_cell("location_dest_id", Cell::Id(dest))
            .set_cell("partner_id", Cell::Id(partner))
            .set_cell("origin", text(origin)),
    )
    .await;
    for (product, quantity) in moves {
        create(
            tx,
            company,
            "stock.move",
            RawVals::new()
                .set_cell("company_id", Cell::Id(company))
                .set_cell("picking_id", Cell::Id(id))
                .set_cell("product_id", Cell::Id(*product))
                .set_cell("location_id", Cell::Id(src))
                .set_cell("location_dest_id", Cell::Id(dest))
                .set_cell("product_uom_qty", Cell::Decimal(quantity.parse().unwrap())),
        )
        .await;
    }
    id
}

async fn call(tx: &mut Transaction, company: i64, picking: i64, method: &'static str) {
    let records = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("stock.picking")
        .unwrap()
        .browse([picking])
        .unwrap();
    tx.savepoint(async move |tx| records.call::<(), ()>(tx, method, ()).await)
        .await
        .unwrap_or_else(|e| panic!("{method}: {e:?}"));
}

/// Text of every page of a PDF without white space, read back with an independent parser.
fn page_texts(bytes: &[u8]) -> Vec<String> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    pdf.get_pages()
        .keys()
        .map(|page| {
            let text = pdf.extract_text(&[*page]).unwrap();
            text.chars().filter(|c| !c.is_whitespace()).collect()
        })
        .collect()
}

/// What one print gave: the text of each PDF page, the file name, and the alternative text of
/// each barcode or QR code of the document the render worker laid out.
struct Printed {
    pages: Vec<String>,
    file: String,
    codes: Vec<String>,
}

async fn print(tx: &mut Transaction, report: &str, ids: &[i64], keep: &str) -> Printed {
    let action = erp_addon_base::report_print::find(tx, report)
        .await
        .unwrap();
    erp_addon_base::report_print::check_records(tx, &action, None, ids)
        .await
        .unwrap();
    let document = erp_addon_base::report_print::prepare(tx, &action, ids)
        .await
        .unwrap();
    let file = erp_addon_base::report_print::file_name(tx, &action, ids, "pdf")
        .await
        .unwrap();
    let fonts = addons_dir().join("report_fonts");
    let engine = erp_report_typst::TypstEngine::new(&fonts, erp_report::Limits::default()).unwrap();
    let framed = report_frame::through_frame(&document);
    let codes = framed
        .nodes()
        .iter()
        .filter_map(|node| match node {
            erp_report::Node::Vector { alt, .. } => Some(alt.clone()),
            _ => None,
        })
        .collect();
    let pdf = engine.render(&framed).unwrap();
    // Kept in the test scratch folder for a look at the layout.
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::write(out.join(format!("stock-{keep}.pdf")), pdf.bytes()).unwrap();
    Printed {
        pages: page_texts(pdf.bytes()),
        file,
        codes,
    }
}

fn has(page: &str, parts: &[&str]) {
    for part in parts {
        assert!(page.contains(part), "{part}: {page}");
    }
}

/// report service: delivery slip with address, source document, ordered and delivered quantities
/// and signatures; receipt of a validated transfer; picking operations with locations per line;
/// a long transfer repeats its table header and prints the signatures on the last page)
#[test]
fn inventory_documents_print_their_titles_lines_and_signatures() {
    big_stack(body);
}

async fn body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = one(
        &mut tx,
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
    )
    .await;

    // The letterhead and footer print the company of the session.
    tx.env()
        .with_company(company)
        .unwrap()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new()
                .set_cell("name", text("Atlas Trading Co"))
                .set_cell("street", text("Jalan Merdeka 1"))
                .set_cell("zip", text("10110"))
                .set_cell("city", text("Jakarta"))
                .set_cell("phone", text("+62 21 5550100"))
                .set_cell("email", text("info@atlas.example")),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = orm
        .begin_with(SUPERUSER_ID, Context::new().with_company_id(company))
        .await
        .unwrap();

    // The three reports are in the Print menu of transfers.
    let listed = erp_addon_base::report_print::printable(&mut tx, "stock.picking")
        .await
        .unwrap();
    for (report, label) in [
        ("stock.report_deliveryslip", "Delivery Slip"),
        ("stock.report_receipt", "Receipt"),
        ("stock.report_picking_operations", "Picking Operations"),
    ] {
        assert!(
            listed.contains(&(report.to_owned(), label.to_owned())),
            "{listed:?}"
        );
    }

    let country = one(&mut tx, "SELECT id FROM res_country ORDER BY id LIMIT 1").await;
    let partner = create(
        &mut tx,
        company,
        "res.partner",
        RawVals::new()
            .set_cell("name", text("Sinar Abadi"))
            .set_cell("street", text("Jalan Asia Afrika 8"))
            .set_cell("zip", text("40112"))
            .set_cell("city", text("Bandung"))
            .set_cell("country_id", Cell::Id(country)),
    )
    .await;
    let chair = product(&mut tx, company, "Office Chair").await;
    tx.execute(
        "UPDATE product_product SET default_code = 'CH-01' WHERE id = $1",
        &[&chair],
    )
    .await
    .unwrap();
    tx.invalidate_model("product.product", None).await.unwrap();
    let desk = product(&mut tx, company, "Standing Desk").await;
    tx.execute(
        "UPDATE product_product SET barcode = '5901234123457' WHERE id = $1",
        &[&desk],
    )
    .await
    .unwrap();
    tx.invalidate_model("product.product", None).await.unwrap();

    // A draft delivery: the slip says Draft, names the customer and source document and the
    // ordered quantity of each product, and ends with both signature lines.
    let delivery = picking(
        &mut tx,
        company,
        "outgoing",
        "OUT/0001",
        partner,
        "SO0042",
        &[(chair, "5".into()), (desk, "2".into())],
    )
    .await;
    let slip = print(
        &mut tx,
        "stock.report_deliveryslip",
        &[delivery],
        "slip-draft",
    )
    .await;
    assert_eq!(slip.file, "Delivery Slip - OUT_0001.pdf");
    assert_eq!(slip.pages.len(), 1, "{:?}", slip.pages);
    has(
        &slip.pages[0],
        &[
            "DeliverySlipOUT/0001",
            "Draft",
            "Customer",
            "SinarAbadi",
            "JalanAsiaAfrika8",
            "40112Bandung",
            "SourceDocumentSO0042",
            "AtlasTradingCo",
            "JalanMerdeka1",
            "ScheduledDate",
            "OperationType",
            "Product",
            "Ordered",
            "Delivered",
            "OfficeChair",
            "StandingDesk",
            "Units",
            "Deliveredby",
            "Receivedby(nameandsignature)",
        ],
    );
    call(&mut tx, company, delivery, "action_confirm").await;
    let slip = print(&mut tx, "stock.report_deliveryslip", &[delivery], "slip").await;
    assert!(!slip.pages[0].contains("Draft"), "{}", slip.pages[0]);
    // The delivery slip goes to the customer: it carries no code (as in the reference).
    assert!(slip.codes.is_empty(), "{:?}", slip.codes);
    // No seconds in the dates: the hour and minute end the scheduled date.
    assert!(
        !slip.pages[0].contains("ScheduledDate") || {
            let at = slip.pages[0].find("ScheduledDate").unwrap();
            let tail: String = slip.pages[0][at + 13..].chars().take(18).collect();
            tail.matches(':').count() == 1
        },
        "{}",
        slip.pages[0]
    );

    // The picking operations sheet of the same transfer: locations of each row, quantity to move
    // and a blank to write the done quantity in.
    let sheet = print(
        &mut tx,
        "stock.report_picking_operations",
        &[delivery],
        "operations",
    )
    .await;
    has(
        &sheet.pages[0],
        &[
            "PickingOperationsOUT/0001",
            "SourceDocumentSO0042",
            "From",
            "To",
            "ToMove",
            "Done",
            "OfficeChair",
            "StandingDesk",
            "[CH-01]OfficeChair",
            "Barcode:5901234123457",
            "______",
        ],
    );
    assert!(
        !sheet.pages[0].contains("Lot/SerialNumber"),
        "no lot column without a lot: {}",
        sheet.pages[0]
    );
    // render worker frame; that such a code reads is the scan check of erp-report-typst)
    assert_eq!(sheet.codes.len(), 1, "{:?}", sheet.codes);
    assert!(
        sheet.codes[0].starts_with("Barcode ") && sheet.codes[0].ends_with("OUT/0001"),
        "{:?}",
        sheet.codes
    );

    // A receipt that is ready (lines from the vendor) and then validated: the received quantity is
    // filled in and the date of the transfer replaces the scheduled date.
    let receipt = picking(
        &mut tx,
        company,
        "incoming",
        "IN/0001",
        partner,
        "PO0007",
        &[(chair, "10".into())],
    )
    .await;
    call(&mut tx, company, receipt, "action_assign").await;
    let ready = print(&mut tx, "stock.report_receipt", &[receipt], "receipt-ready").await;
    has(
        &ready.pages[0],
        &[
            "ReceiptIN/0001",
            "Vendor",
            "SinarAbadi",
            "SourceDocumentPO0007",
        ],
    );
    has(&ready.pages[0], &["Received", "OfficeChair", "Receivedby"]);
    let sheet = print(
        &mut tx,
        "stock.report_picking_operations",
        &[receipt],
        "operations-ready",
    )
    .await;
    has(&sheet.pages[0], &["Vendors", "OfficeChair", "______"]);
    // A line with a lot name adds the lot column to the sheet.
    tx.execute(
        "UPDATE stock_move_line SET lot_name = 'LOT-A1' WHERE picking_id = $1",
        &[&receipt],
    )
    .await
    .unwrap();
    tx.invalidate_model("stock.move.line", None).await.unwrap();
    let sheet = print(
        &mut tx,
        "stock.report_picking_operations",
        &[receipt],
        "operations-lot",
    )
    .await;
    has(&sheet.pages[0], &["Lot/SerialNumber", "LOT-A1"]);
    call(&mut tx, company, receipt, "button_validate").await;

    // A reserved transfer of three products without lots: three operation lines, no lot column;
    // a lot name on one of the three lines adds the column with that line's lot only.
    let cabinet = product(&mut tx, company, "Filing Cabinet").await;
    let three = picking(
        &mut tx,
        company,
        "incoming",
        "IN/0002",
        partner,
        "PO0008",
        &[
            (chair, "4".into()),
            (desk, "3".into()),
            (cabinet, "2".into()),
        ],
    )
    .await;
    call(&mut tx, company, three, "action_assign").await;
    let lines: i64 = one(
        &mut tx,
        &format!("SELECT count(*) FROM stock_move_line WHERE picking_id = {three}"),
    )
    .await;
    assert_eq!(lines, 3, "one operation line per move");
    let sheet = print(
        &mut tx,
        "stock.report_picking_operations",
        &[three],
        "operations-three",
    )
    .await;
    has(
        &sheet.pages[0],
        &[
            "PickingOperationsIN/0002",
            "OfficeChair",
            "StandingDesk",
            "FilingCabinet",
        ],
    );
    assert!(
        !sheet.pages[0].contains("Lot/SerialNumber"),
        "no lot column without a lot: {}",
        sheet.pages[0]
    );
    tx.execute(
        "UPDATE stock_move_line SET lot_name = 'LOT-B7' WHERE picking_id = $1 AND product_id = $2",
        &[&three, &desk],
    )
    .await
    .unwrap();
    tx.invalidate_model("stock.move.line", None).await.unwrap();
    let sheet = print(
        &mut tx,
        "stock.report_picking_operations",
        &[three],
        "operations-three-lot",
    )
    .await;
    has(
        &sheet.pages[0],
        &["Lot/SerialNumber", "LOT-B7", "FilingCabinet"],
    );
    assert_eq!(
        sheet.pages[0].matches("LOT-").count(),
        1,
        "{}",
        sheet.pages[0]
    );
    let done = print(&mut tx, "stock.report_receipt", &[receipt], "receipt").await;
    has(
        &done.pages[0],
        &[
            "ReceiptIN/0001",
            "DateofTransfer",
            "Received",
            "Lot/SerialNumber:LOT-A1",
            "10.00Units10.00Units",
        ],
    );
    assert!(
        !done.pages[0].contains("ScheduledDate"),
        "{}",
        done.pages[0]
    );

    // A long transfer: the table header repeats on each page, the signatures print once, on the
    // last page, and two transfers in one print start on separate pages.
    let many: Vec<(i64, String)> = (0..110)
        .map(|n| (if n % 2 == 0 { chair } else { desk }, format!("{}", n + 1)))
        .collect();
    let long = picking(
        &mut tx, company, "outgoing", "OUT/0002", partner, "SO0043", &many,
    )
    .await;
    let printed = print(
        &mut tx,
        "stock.report_deliveryslip",
        &[long, delivery],
        "slip-long",
    )
    .await;
    let long_pages: Vec<&String> = printed
        .pages
        .iter()
        .take_while(|page| !page.contains("OUT/0001"))
        .collect();
    assert!(long_pages.len() >= 2, "110 lines fill several pages");
    for page in &long_pages {
        has(page, &["ProductOrderedDelivered"]);
    }
    let last = long_pages.len() - 1;
    for (index, page) in long_pages.iter().enumerate() {
        assert_eq!(page.contains("Deliveredby"), index == last, "page {index}");
    }
    assert!(
        printed.pages[long_pages.len()].contains("DeliverySlipOUT/0001"),
        "the second transfer starts on its own page"
    );

    // An inventory user prints the three documents of a transfer of the user's company; a user
    // without an inventory role and a transfer of another company are refused.
    let currency = one(
        &mut tx,
        &format!("SELECT currency_id::int8 FROM res_company WHERE id = {company}"),
    )
    .await;
    let second = create(
        &mut tx,
        company,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Harbour Logistics"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let elsewhere = picking(
        &mut tx,
        second,
        "outgoing",
        "OUT/0901",
        partner,
        "SO0901",
        &[(chair, "1".into())],
    )
    .await;
    report_access::admin(&mut tx, company).await;
    let clerk = report_access::user(
        &mut tx,
        "report-stock-user",
        company,
        &[("stock", "group_stock_user")],
    )
    .await;
    let outsider = report_access::user(&mut tx, "report-stock-outsider", company, &[]).await;
    tx.commit().await.unwrap();
    let users = report_access::policy_orm(&db, &orm).await;
    for (report, ids, title) in [
        (
            "stock.report_deliveryslip",
            [delivery],
            "DeliverySlipOUT/0001",
        ),
        ("stock.report_receipt", [receipt], "ReceiptIN/0001"),
        (
            "stock.report_picking_operations",
            [three],
            "PickingOperationsIN/0002",
        ),
    ] {
        let pages = report_access::print_as(&users, clerk, company, report, &ids)
            .await
            .unwrap_or_else(|e| panic!("{report} as an inventory user: {e:?}"));
        has(&pages[0], &[title, "AtlasTradingCo"]);
        report_access::refused(
            report_access::print_as(&users, outsider, company, report, &ids).await,
            &format!("{report} as a user without an inventory role"),
        );
        report_access::refused(
            report_access::print_as(&users, clerk, company, report, &[elsewhere]).await,
            &format!("{report} of another company"),
        );
    }
    drop(users);
    db.cleanup().await.unwrap();
}

#[test]
fn count_sheet_prints_selected_quants_grouped_by_location() {
    big_stack(count_sheet_body);
}

async fn count_sheet_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = one(
        &mut tx,
        "SELECT res_id::int8 FROM ir_model_data WHERE module = 'base' AND name = 'main_company'",
    )
    .await;
    let mut tx = {
        tx.commit().await.unwrap();
        orm.begin_with(SUPERUSER_ID, Context::new().with_company_id(company))
            .await
            .unwrap()
    };

    let listed = erp_addon_base::report_print::printable(&mut tx, "stock.quant")
        .await
        .unwrap();
    assert!(
        listed.contains(&(
            "stock.report_inventory".to_owned(),
            "Count Sheet".to_owned()
        )),
        "{listed:?}"
    );
    // The stock action offers the list and the form, and the form view is installed.
    let action: String = tx
        .query(
            "SELECT a.view_mode FROM ir_act_window a JOIN ir_model_data d ON d.model = 'ir.actions.act_window' \
             AND d.res_id = a.id WHERE d.module = 'stock' AND d.name = 'action_stock_quant'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(action, "list,form");
    let form_arch: String = tx
        .query(
            "SELECT v.arch_db::text FROM ir_ui_view v JOIN ir_model_data d ON d.model = 'ir.ui.view' \
             AND d.res_id = v.id WHERE d.module = 'stock' AND d.name = 'view_stock_quant_form'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert!(form_arch.contains("name=\"uom_id\""), "{form_arch}");

    let bolt = product(&mut tx, company, "Hex Bolt").await;
    let nut = product(&mut tx, company, "Wing Nut").await;
    let shelf_a = one(
        &mut tx,
        &format!(
            "SELECT id FROM stock_location WHERE usage = 'internal' AND company_id = {company} ORDER BY id LIMIT 1"
        ),
    )
    .await;
    let parent = one(
        &mut tx,
        &format!("SELECT location_id::int8 FROM stock_location WHERE id = {shelf_a}"),
    )
    .await;
    let shelf_b = create(
        &mut tx,
        company,
        "stock.location",
        RawVals::new()
            .set_cell("name", text("Rack Z9"))
            .set_cell("usage", text("internal"))
            .set_cell("location_id", Cell::Id(parent))
            .set_cell("company_id", Cell::Id(company)),
    )
    .await;
    let lot = create(
        &mut tx,
        company,
        "stock.lot",
        RawVals::new()
            .set_cell("name", text("LOT-Z1"))
            .set_cell("product_id", Cell::Id(bolt))
            .set_cell("company_id", Cell::Id(company)),
    )
    .await;
    let mut quants = Vec::new();
    for (product, location, quantity, lot) in [
        (bolt, shelf_a, "12", Some(lot)),
        (nut, shelf_b, "7", None),
        (nut, shelf_a, "3", None),
    ] {
        // Balances change only through moves (the ORM refuses a create), so the fixture is SQL.
        let quantity: Decimal = quantity.parse().unwrap();
        quants.push(
            tx.query(
                "INSERT INTO stock_quant (company_id, product_id, location_id, lot_id, quantity, reserved_quantity, in_date) \
                 VALUES ($1, $2, $3, $4, $5, 0, now()) RETURNING id",
                &[&company, &product, &location, &lot, &quantity],
            )
            .await
            .unwrap()[0]
                .get::<_, i64>(0),
        );
    }
    let a_name: String = tx
        .query(
            "SELECT complete_name FROM stock_location WHERE id = $1",
            &[&shelf_a],
        )
        .await
        .unwrap()[0]
        .get(0);
    let a: String = a_name.chars().filter(|c| !c.is_whitespace()).collect();

    // Default: a blind count (no On Hand quantity), the Lot column because one line has a lot, no
    // Package column because none does.
    let blind = print(&mut tx, "stock.report_inventory", &quants, "count-sheet").await;
    assert_eq!(blind.pages.len(), 1, "{:?}", blind.pages);
    let page = &blind.pages[0];
    has(
        page,
        &[
            "CountSheet",
            "Product",
            "Lot/SerialNumber",
            "OnHand",
            "Counted",
        ],
    );
    assert!(!page.contains("Package"), "no package used: {page}");
    for quantity in ["12.00", "7.00", "3.00"] {
        assert!(
            !page.contains(quantity),
            "blind count shows {quantity}: {page}"
        );
    }
    assert_grouped(page, &a);

    // The setting shows On Hand, each quantity with its unit inside its own location's block.
    tx.env()
        .records("ir.config_parameter")
        .unwrap()
        .call::<(String, Option<bool>), bool>(
            &mut tx,
            "set_bool",
            ("stock.show_expected_quantity_count".to_owned(), Some(true)),
        )
        .await
        .unwrap();
    // a real print is its own transaction; here the first print cached the switch
    tx.invalidate_model("stock.quant", None).await.unwrap();
    let shown = print(
        &mut tx,
        "stock.report_inventory",
        &quants,
        "count-sheet-on-hand",
    )
    .await;
    let page = &shown.pages[0];
    assert_grouped(page, &a);
    for quantity in ["12.00Units", "7.00Units", "3.00Units"] {
        has(page, &[quantity]);
    }
    let (first, second) = if page.find(&a) < page.find("RackZ9") {
        (&a[..], "RackZ9")
    } else {
        ("RackZ9", &a[..])
    };
    let (start, split) = (page.find(first).unwrap(), page.find(second).unwrap());
    let (block_a, block_b) = if first == a {
        (&page[start..split], &page[split..])
    } else {
        (&page[split..], &page[start..split])
    };
    assert!(
        block_a.contains("12.00Units") && block_a.contains("3.00Units"),
        "{page}"
    );
    assert!(
        block_b.contains("7.00Units") && !block_b.contains("12.00"),
        "{page}"
    );
    db.cleanup().await.unwrap();
}

/// The page prints each location heading once, each product once per location, and a block reads
/// heading, product, lot (for the lotted line), next product, in that order.
fn assert_grouped(page: &str, a: &str) {
    assert_eq!(
        page.matches(a).count(),
        1,
        "one heading per location: {page}"
    );
    assert_eq!(page.matches("RackZ9").count(), 1, "{page}");
    assert_eq!(page.matches("HexBolt").count(), 1, "{page}");
    assert_eq!(
        page.matches("WingNut").count(),
        2,
        "one per location: {page}"
    );
    let pos = |part: &str| page.find(part).unwrap_or_else(|| panic!("{part}: {page}"));
    let (heading_a, bolt, lot, last_nut) = (
        pos(a),
        pos("HexBolt"),
        pos("LOT-Z1"),
        page.find("WingNut").unwrap(),
    );
    let rack = pos("RackZ9");
    if heading_a < rack {
        // shelf A first: its heading, then Hex Bolt, its lot, Wing Nut, then the other heading and its nut
        assert!(
            heading_a < bolt && bolt < lot && lot < last_nut && last_nut < rack,
            "{page}"
        );
        assert!(page.rfind("WingNut").unwrap() > rack, "{page}");
    } else {
        // the other location first: its Wing Nut, then shelf A's heading, product, lot and Wing Nut
        assert!(
            rack < last_nut && last_nut < heading_a && heading_a < bolt && bolt < lot,
            "{page}"
        );
        assert!(page.rfind("WingNut").unwrap() > lot, "{page}");
    }
}
