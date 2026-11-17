// SPDX-License-Identifier: MIT OR Apache-2.0
//! Printing a business document as an ordinary user of its application, for the report tests: an
//! ORM that asks the installed access policy, users of one company with the groups of a role, and
//! a print in the user's own transaction that returns the refusal instead of failing.

use std::path::Path;
use std::sync::Arc;

use erp_fields::{Cell, Error, ErrorKind, RecId};
use erp_orm::{Command, Context, Orm, RawVals, SUPERUSER_ID, Transaction, UserId};
use erp_report::LayoutEngine;
use erp_testkit::TestDb;

/// An ORM on the same database that asks the installed access policy, so a test can act as a user
/// who is not the superuser. Build it after the commit that adds the users.
pub async fn policy_orm(db: &TestDb, installed: &Orm) -> Orm {
    let source = erp_security::source::PolicySource::build(
        Arc::clone(installed.registry()),
        db.pool().clone(),
    )
    .await
    .unwrap();
    Orm::builder(Arc::clone(installed.registry()), db.pool().clone())
        .generation_attachment(erp_orm::GenerationAttachment::new(
            Arc::clone(installed.registry()),
            installed
                .env(SUPERUSER_ID, Context::new())
                .generation_attachment::<erp_views::PublishedViewTypes>()
                .unwrap(),
        ))
        .require_core_tables(erp_security::auth::core_tables())
        .require_core_tables(erp_addon_base::partner_hierarchy::core_tables())
        .resolver(erp_security::resolver::Resolver::for_registry(installed.registry()).unwrap())
        .policy(Arc::new(source) as Arc<dyn erp_orm::PolicyProvider>)
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap()
}

async fn xml_id(tx: &mut Transaction, module: &str, name: &str) -> i64 {
    tx.query(
        "SELECT res_id::int8 FROM ir_model_data WHERE module = $1::text AND name = $2::text",
        &[&module, &name],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// An internal user of `company` only, holding the groups `groups` (`(module, name)` external ids).
pub async fn user(tx: &mut Transaction, login: &str, company: i64, groups: &[(&str, &str)]) -> i64 {
    let mut ids = vec![xml_id(tx, "base", "group_user").await];
    for (module, name) in groups {
        ids.push(xml_id(tx, module, name).await);
    }
    let vals = RawVals::new()
        .set_cell("name", Cell::Str(login.into()))
        .set_cell("login", Cell::Str(login.into()))
        .set_cell("company_id", Cell::Id(company))
        .set_commands(
            "company_ids",
            [Command::set([RecId::from_real(company).unwrap()])],
        )
        .set_commands(
            "group_ids",
            [Command::set(
                ids.iter().map(|id| RecId::from_real(*id).unwrap()),
            )],
        );
    tx.env()
        .with_company(company)
        .unwrap()
        .records("res.users")
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

/// A settings administrator of `company`: the base rule refuses to add users while the database
/// has none.
pub async fn admin(tx: &mut Transaction, company: i64) -> i64 {
    user(tx, "report-admin", company, &[("base", "group_system")]).await
}

/// Print `ids` through `report` as `uid` in `company`, the way the print route does: the records
/// are checked, the document prepared with the user's rights and laid out. The text of each PDF
/// page without white space, or the refusal.
pub async fn print_as(
    orm: &Orm,
    uid: i64,
    company: i64,
    report: &str,
    ids: &[i64],
) -> Result<Vec<String>, Error> {
    let mut tx = orm
        .begin_with(
            UserId::new(uid).unwrap(),
            Context::new().with_company_id(company),
        )
        .await
        .unwrap();
    let prepared = async {
        let action = erp_addon_base::report_print::find(&mut tx, report).await?;
        erp_addon_base::report_print::check_records(&mut tx, &action, None, ids).await?;
        erp_addon_base::report_print::prepare(&mut tx, &action, ids).await
    }
    .await;
    tx.rollback().await.unwrap();
    let fonts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts");
    let engine = erp_report_typst::TypstEngine::new(&fonts, erp_report::Limits::default()).unwrap();
    let pdf = engine.render(&prepared?).unwrap();
    let parsed = lopdf::Document::load_mem(pdf.bytes()).unwrap();
    Ok(parsed
        .get_pages()
        .keys()
        .map(|page| {
            let text = parsed.extract_text(&[*page]).unwrap();
            text.chars().filter(|c| !c.is_whitespace()).collect()
        })
        .collect())
}

/// `printed` is the access refusal of a document the user may not read.
#[track_caller]
pub fn refused(printed: Result<Vec<String>, Error>, what: &str) {
    match printed {
        Ok(pages) => panic!("{what} must be refused, printed: {pages:?}"),
        Err(error) => assert_eq!(error.kind(), ErrorKind::Access, "{what}: {error:?}"),
    }
}
