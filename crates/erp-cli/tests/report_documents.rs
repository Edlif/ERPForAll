// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared parts of a printed business document (`addons/web/data/report_documents.xml`,
//! ): a report of a test template that calls every part is
//! saved and printed through the module system on a real database, and the PDF is read back.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
};

use erp_fields::Cell;
use erp_modules::{
    AddonIndex,
    system::{InitOptions, ModuleSystem},
};
use erp_orm::{RawVals, SUPERUSER_ID};
use erp_report::LayoutEngine;
use erp_testkit::TestDb;

fn repo_addons() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("addons")
}

fn system(db: &TestDb) -> ModuleSystem {
    let mut index = AddonIndex::new(true);
    index.add_dev_path(repo_addons());
    let native_artifact_identities = index
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
    ModuleSystem::builder(db.pool().clone(), index)
        .view_types(erp_addon_web::view_type_contributions())
        .native_artifact_identities(native_artifact_identities)
        .code([
            &erp_addon_uom::ADDON,
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
        ])
        .require_core_tables(erp_security::auth::core_tables())
        .build()
        .unwrap()
}

fn big_stack(body: impl std::future::Future<Output = ()> + Send + 'static) {
    thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(body);
        })
        .unwrap()
        .join()
        .unwrap();
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

/// A document that uses every shared part: title with number and status, two addresses, five
/// information items (four to a line), a lines table with a section, 60 lines and a note,
/// totals, a note block and two signatures.
const DOCUMENT: &str = r#"<t t-name="test_documents.report_sample">
<t t-call="web.html_container"><t t-call="web.external_layout">
<t t-foreach="docs" t-as="o"><div class="page">
    <t t-call="web.document_title" title.translate="Quotation" number="'Q-0042'" status.translate="Draft"/>
    <t t-call="web.document_addresses">
        <t t-call="web.document_address" address="o" label.translate="Customer"/>
        <t t-call="web.document_address" address="o" label.translate="Delivery Address"/>
    </t>
    <t t-call="web.document_info">
        <t t-call="web.document_info_item" label.translate="Order Date">12/11/2026</t>
        <t t-call="web.document_info_item" label.translate="Valid Until">12/12/2026</t>
        <t t-call="web.document_info_item" label.translate="Salesperson">Dewi Lestari</t>
        <t t-call="web.document_info_item" label.translate="Your Reference">PO-7781</t>
        <t t-call="web.document_info_item" label.translate="Source">Website</t>
    </t>
    <t t-call="web.document_lines">
        <thead><tr><th>Description</th><th class="text-end">Quantity</th><th class="text-end">Amount</th></tr></thead>
        <tbody>
            <t t-call="web.document_line_section" columns="3" text="'Hardware'"/>
            <tr t-foreach="60" t-as="n">
                <td>Rowstart<t t-out="n + 1"/> a line description that is long enough to wrap onto a second line of the cell before it ends Rowend<t t-out="n + 1"/>.</td>
                <td class="text-end"><t t-out="n + 1"/> Units</td>
                <td class="text-end">Rp 1.000,00</td>
            </tr>
            <t t-call="web.document_line_note" columns="3" text="'Delivered in two parts.'"/>
        </tbody>
    </t>
    <t t-call="web.document_totals">
        <t t-call="web.document_total_row" label.translate="Untaxed Amount">Rp 60.000,00</t>
        <t t-call="web.document_total_row" label.translate="VAT 11%">Rp 6.600,00</t>
        <t t-call="web.document_total_grand" label.translate="Grand Total">Rp 66.600,00</t>
    </t>
    <t t-call="web.document_note" label.translate="Terms">Payment within 30 days of the invoice date.</t>
    <t t-call="web.document_signatures">
        <t t-call="web.document_signature" label.translate="Prepared by"/>
        <t t-call="web.document_signature" label.translate="Approved by"/>
    </t>
</div></t>
</t></t>
</t>"#;

/// Every shared document part prints: the title row, both address blocks with the partner's
/// address and tax ID, an information row that wraps after four items, a lines table whose header
/// repeats on every page while no row is split across two pages, the totals and the closing
/// blocks once, on the last page.
#[test]
fn the_shared_document_parts_print_a_complete_document() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let system = system(&db);
        let orm = system.initialize(InitOptions::new()).await.unwrap().orm;
        let mut tx = orm
            .begin_with(SUPERUSER_ID, erp_orm::Context::new().with_company_id(1))
            .await
            .unwrap();
        let country = tx
            .env()
            .records("res.country")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Testland".into()))
                        .set_cell("code", Cell::Str("ZZ".into())),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let partner = tx
            .env()
            .records("res.partner")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Sinar Abadi".into()))
                        .set_cell("street", Cell::Str("Jalan Asia Afrika 8".into()))
                        .set_cell("zip", Cell::Str("40112".into()))
                        .set_cell("city", Cell::Str("Bandung".into()))
                        .set_cell("country_id", Cell::Id(country))
                        .set_cell("vat", Cell::Str("02.345.678.9-012.000".into())),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.env()
            .records("ir.ui.view")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("test_documents.report_sample".into()))
                        .set_cell("type", Cell::Str("qweb".into()))
                        .set_cell("key", Cell::Str("test_documents.report_sample".into()))
                        .set_cell("arch_db", Cell::Str(DOCUMENT.into())),
                ],
            )
            .await
            .unwrap();
        tx.env()
            .records("ir.actions.report")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Sample Document".into()))
                        .set_cell("model", Cell::Str("res.partner".into()))
                        .set_cell("report_type", Cell::Str("qweb-pdf".into()))
                        .set_cell(
                            "report_name",
                            Cell::Str("test_documents.report_sample".into()),
                        ),
                ],
            )
            .await
            .unwrap();
        let report = erp_addon_base::report_print::find(&mut tx, "test_documents.report_sample")
            .await
            .unwrap();
        let document = erp_addon_base::report_print::prepare(&mut tx, &report, &[partner])
            .await
            .unwrap();
        tx.rollback().await.unwrap();

        let html = erp_report::render_html(&document, &[]).unwrap();
        for text in [
            "Quotation",
            "Q-0042",
            "Draft",
            "Customer",
            "Delivery Address",
            "Sinar Abadi",
            "Jalan Asia Afrika 8",
            "40112 Bandung",
            "Testland",
            "02.345.678.9-012.000",
            "Valid Until",
            "PO-7781",
            "Hardware",
            "Delivered in two parts.",
            "Untaxed Amount",
            "Rp 66.600,00",
            "Payment within 30 days",
            "Approved by",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        // The section and the note span the three columns of the lines table.
        assert_eq!(html.matches("colspan=\"3\"").count(), 2, "{html}");

        let fonts = repo_addons().join("report_fonts");
        let engine =
            erp_report_typst::TypstEngine::new(&fonts, erp_report::Limits::default()).unwrap();
        let pdf = engine.render(&document).unwrap();
        let pages = page_texts(pdf.bytes());
        assert!(pages.len() >= 2, "60 lines fill more than one page");
        let last = pages.len() - 1;
        for (index, page) in pages.iter().enumerate() {
            // The header of the lines table repeats on every page that holds lines.
            if page.contains("Rowstart") {
                assert!(
                    page.contains("DescriptionQuantityAmount"),
                    "page {index}: {page}"
                );
            }
            // The totals and the closing blocks print once, after the lines.
            for closing in ["GrandTotal", "Terms", "Approvedby"] {
                assert_eq!(
                    page.contains(closing),
                    index == last,
                    "{closing} on page {index}"
                );
            }
        }
        assert!(pages[0].contains("QuotationQ-0042"), "{}", pages[0]);
        // No line is split across two pages: each line starts and ends on the same page.
        for n in 1..=60 {
            let start = format!("Rowstart{n}a");
            let end = format!("Rowend{n}.");
            let page = pages
                .iter()
                .position(|p| p.contains(&start))
                .unwrap_or_else(|| panic!("line {n} is printed"));
            assert!(pages[page].contains(&end), "line {n} ends on page {page}");
        }
        // Kept for a look at the layout.
        let out = Path::new(env!("CARGO_TARGET_TMPDIR"));
        fs::write(out.join("report-documents-sample.pdf"), pdf.bytes()).unwrap();
    });
}
