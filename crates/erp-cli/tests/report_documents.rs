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

/// An invoice-shaped document: the full column set of an invoice with amounts in the style of a
/// currency that groups by dots (`Rp 12.500.000,00`), a row per tax group in the totals, a long
/// number next to a status, and the company letterhead with a logo and a full address.
const INVOICE: &str = r#"<t t-name="test_documents.report_invoice_sample">
<t t-call="web.html_container"><t t-call="web.external_layout">
<t t-foreach="docs" t-as="o"><div class="page">
    <t t-call="web.document_title" title.translate="Vendor Credit Note" number="'BILL/2026/11/000042/BANDUNG-UTARA-WAREHOUSE'" status.translate="Cancelled"/>
    <t t-call="web.document_addresses">
        <t t-call="web.document_address" address="o" label.translate="Vendor"/>
    </t>
    <t t-call="web.document_info">
        <t t-call="web.document_info_item" label.translate="Bill Date">12/11/2026</t>
        <t t-call="web.document_info_item" label.translate="Due Date">12/12/2026</t>
        <t t-call="web.document_info_item" label.translate="Reference">INV-88231</t>
    </t>
    <t t-call="web.document_lines">
        <thead><tr><th>Description</th><th class="text-end">Quantity</th><th class="text-end">Unit Price</th><th class="text-end">Disc. %</th><th>Taxes</th><th class="text-end">Amount</th></tr></thead>
        <tbody>
            <t t-call="web.document_line_section" columns="6" text="'Office furniture'"/>
            <tr t-foreach="5" t-as="n">
                <td>Executive desk, walnut veneer, 180 x 90 cm, delivered and assembled</td>
                <td class="text-end">1.250,00 Units</td>
                <td class="text-end">Rp&#160;12.500.000,00</td>
                <td class="text-end">10,00</td>
                <td>PPN 11%<br/>PPnBM 10%</td>
                <td class="text-end">Rp&#160;14.062.500.000,00</td>
            </tr>
        </tbody>
    </t>
    <t t-call="web.document_totals">
        <t t-call="web.document_total_row" label.translate="Untaxed Amount">Rp&#160;70.312.500.000,00</t>
        <t t-call="web.document_total_row" label.translate="PPN">Rp&#160;7.734.375.000,00</t>
        <t t-call="web.document_total_row" label.translate="PPnBM">Rp&#160;7.031.250.000,00</t>
        <t t-call="web.document_total_grand" label.translate="Total">Rp&#160;85.078.125.000,00</t>
    </t>
</div></t>
</t></t>
</t>"#;

/// A small PNG (a solid colour), as a company logo.
fn test_png(width: u32, height: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
        let start = out.len();
        out.extend(kind);
        out.extend(data);
        let mut crc = !0u32;
        for b in &out[start..] {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        out.extend((!crc).to_be_bytes());
    }
    let row: Vec<u8> = std::iter::once(0)
        .chain([8, 109, 113].repeat(width as usize))
        .collect();
    let raw = row.repeat(height as usize);
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        z.push(u8::from((i + 1) * 65_535 >= raw.len()));
        let len = u16::try_from(block.len()).unwrap();
        z.extend(len.to_le_bytes());
        z.extend((!len).to_le_bytes());
        z.extend(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

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
        // The letterhead of a company with a logo and a full address.
        tx.env()
            .records("res.company")
            .unwrap()
            .browse(vec![1])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new()
                    .set_cell("street", Cell::Str("Jalan Merdeka 12".into()))
                    .set_cell("zip", Cell::Str("40111".into()))
                    .set_cell("city", Cell::Str("Bandung".into()))
                    .set_cell("phone", Cell::Str("+62 22 555 0100".into()))
                    .set_cell("email", Cell::Str("office@example.com".into()))
                    .set_cell("vat", Cell::Str("01.234.567.8-901.000".into()))
                    .set_cell("logo", Cell::Bytes(test_png(160, 64).into())),
            )
            .await
            .unwrap();
        let mut documents = Vec::new();
        for (key, arch) in [
            ("test_documents.report_sample", DOCUMENT),
            ("test_documents.report_invoice_sample", INVOICE),
        ] {
            tx.env()
                .records("ir.ui.view")
                .unwrap()
                .create(
                    &mut tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", Cell::Str(key.into()))
                            .set_cell("type", Cell::Str("qweb".into()))
                            .set_cell("key", Cell::Str(key.into()))
                            .set_cell("arch_db", Cell::Str(arch.into())),
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
                            .set_cell("report_name", Cell::Str(key.into())),
                    ],
                )
                .await
                .unwrap();
            let report = erp_addon_base::report_print::find(&mut tx, key)
                .await
                .unwrap();
            documents.push(
                erp_addon_base::report_print::prepare(&mut tx, &report, &[partner])
                    .await
                    .unwrap(),
            );
        }
        // Phone and email without a website or tax ID: the contact block ends on their line.
        tx.env()
            .records("res.company")
            .unwrap()
            .browse(vec![1])
            .unwrap()
            .write(&mut tx, RawVals::new().set_cell("vat", Cell::Null))
            .await
            .unwrap();
        let report = erp_addon_base::report_print::find(&mut tx, "test_documents.report_sample")
            .await
            .unwrap();
        let contacts = erp_addon_base::report_print::prepare(&mut tx, &report, &[partner])
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let contacts = erp_report::render_html(&contacts, &[]).unwrap();
        let at = contacts.find("office@example.com").unwrap();
        let block = &contacts[at..at + contacts[at..].find("</div>").unwrap()];
        assert!(!block.contains("<br"), "{block}");
        let invoice = documents.pop().unwrap();
        let document = documents.pop().unwrap();

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
        // The letterhead carries the logo.
        assert!(html.contains("aria-label=\"Logo\""), "{html}");

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
        // The invoice-shaped sample fits one page: the full column set, a long number beside its
        // status, and a total row per tax group.
        let invoice_pdf = engine.render(&invoice).unwrap();
        let invoice_pages = page_texts(invoice_pdf.bytes());
        assert_eq!(invoice_pages.len(), 1, "{invoice_pages:?}");
        for text in [
            "VendorCreditNoteBILL/2026/11/000042/BANDUNG-UTARA-WAREHOUSE",
            "Cancelled",
            "DescriptionQuantityUnitPriceDisc.%TaxesAmount",
            "PPN11%PPnBM10%",
            "PPNRp7.734.375.000,00",
            "TotalRp85.078.125.000,00",
        ] {
            assert!(
                invoice_pages[0].contains(text),
                "{text}: {}",
                invoice_pages[0]
            );
        }
        // Kept for a look at the layout.
        let out = Path::new(env!("CARGO_TARGET_TMPDIR"));
        fs::write(out.join("report-documents-sample.pdf"), pdf.bytes()).unwrap();
        fs::write(
            out.join("report-documents-invoice.pdf"),
            invoice_pdf.bytes(),
        )
        .unwrap();
    });
}
