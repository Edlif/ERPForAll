// SPDX-License-Identifier: MIT OR Apache-2.0
//! The print service prints in the printing user's language formats and timezone: a test report
//! over partners is saved and printed through the module system on a real database, and the PDF
//! is read back.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]

#[path = "support/report_dpi.rs"]
mod report_dpi;
#[path = "support/report_frame.rs"]
mod report_frame;

use std::{
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

/// Text of every page of a PDF without white space.
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

const REPORT: &str = r#"<t t-name="test_locale.report_partner">
<t t-call="web.html_container"><t t-call="web.external_layout">
<t t-foreach="docs" t-as="o"><div t-att-class="None if o_first else 'page-break-before'">
    <p>Name <span t-field="o.name"/></p>
    <p>Stamp <span t-field="o.create_date"/></p>
    <p>Amount <span t-out="12500000" t-options="{'widget': 'float', 'precision': 2}"/></p>
</div></t>
</t></t>
</t>"#;

/// A user in Jakarta whose language writes `.` between thousands and `,` before decimals prints a
/// timestamp in local time (here the next day) and amounts in that language's format; a print of
/// two records of one company is one document.
#[test]
fn a_print_uses_the_users_language_formats_and_timezone() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let system = system(&db);
        let orm = system.initialize(InitOptions::new()).await.unwrap().orm;
        let mut tx = orm
            .begin_with(SUPERUSER_ID, erp_orm::Context::new().with_company_id(1))
            .await
            .unwrap();
        tx.execute(
            "UPDATE res_lang SET active = true WHERE code = 'id_ID'",
            &[],
        )
        .await
        .unwrap();
        tx.env()
            .records("res.users")
            .unwrap()
            .browse(vec![SUPERUSER_ID.get()])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new()
                    .set_cell("lang", Cell::Str("id_ID".into()))
                    .set_cell("tz", Cell::Str("Asia/Jakarta".into())),
            )
            .await
            .unwrap();
        let key = "test_locale.report_partner";
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
                        .set_cell("arch_db", Cell::Str(REPORT.into())),
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
                        .set_cell("name", Cell::Str("Partner Sheet".into()))
                        .set_cell("model", Cell::Str("res.partner".into()))
                        .set_cell("report_type", Cell::Str("qweb-pdf".into()))
                        .set_cell("report_name", Cell::Str(key.into())),
                ],
            )
            .await
            .unwrap();
        let partners = tx
            .env()
            .records("res.partner")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new().set_cell("name", Cell::Str("Sinar Abadi".into())),
                    RawVals::new().set_cell("name", Cell::Str("Maju Jaya".into())),
                ],
            )
            .await
            .unwrap()
            .ids();
        tx.flush().await.unwrap();
        // 20:30 UTC on the 16th is 03:30 on the 17th in Jakarta (UTC+7).
        tx.execute(
            "UPDATE res_partner SET create_date = '2026-11-16 20:30:00' WHERE id = ANY($1)",
            &[&partners],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = orm
            .begin_with(SUPERUSER_ID, erp_orm::Context::new().with_company_id(1))
            .await
            .unwrap();
        let report = erp_addon_base::report_print::find(&mut tx, key)
            .await
            .unwrap();
        let documents = erp_addon_base::report_print::prepare_runs(&mut tx, &report, &partners)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(documents.len(), 1);

        let fonts = repo_addons().join("report_fonts");
        let engine =
            erp_report_typst::TypstEngine::new(&fonts, erp_report::Limits::default()).unwrap();
        let pages = page_texts(
            engine
                .render(&report_frame::through_frame(&documents[0]))
                .unwrap()
                .bytes(),
        );
        assert_eq!(pages.len(), 2, "{pages:?}");
        for (page, name) in pages.iter().zip(["SinarAbadi", "MajuJaya"]) {
            assert!(page.contains(&format!("Name{name}")), "{page}");
            assert!(page.contains("Stamp17/11/202603:30:00"), "{page}");
            assert!(page.contains("Amount12.500.000,00"), "{page}");
        }
    });
}
