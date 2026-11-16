// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real native pre-migration and later-failure rollback from a scalar addon release.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod common;
use common::*;
use erp_modules::artifact_identity::ArtifactSnapshot;
use erp_modules::system::{InitOptions, ModuleSystemBuilder};
use erp_registry::{FieldDecl, ModelDecl};
use erp_testkit::TestDb;
use std::collections::BTreeMap;

mod addon {
    use erp_sdk::migrations::{CompanyMigrationPolicy, SemverVersion};
    use erp_sdk::{Result, Transaction};
    #[erp_sdk::migration(addon="property_upgrade",version="1.1",stage=pre)]
    pub async fn assign(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
        tx.migrate_company_dependent(
            "upgrade.property",
            "note",
            &CompanyMigrationPolicy::companies([1])?,
        )
        .await
    }
}
fn declaration(company: bool) -> &'static ModelDecl {
    Box::leak(Box::new(
        ModelDecl::new("property_upgrade", "upgrade.property")
            .table("upgrade_property")
            .fields(Box::leak(
                vec![FieldDecl::char("note").company_dependent(company)].into_boxed_slice(),
            )),
    ))
}
fn configured(
    db: &TestDb,
    addons: &Addons,
    company: bool,
    previous: BTreeMap<erp_modules::manifest::ModuleName, String>,
) -> ModuleSystemBuilder {
    // The fixture base declares the company table used for explicit owner validation.
    let company_model = Box::leak(Box::new(
        ModelDecl::new("base", "res.company")
            .table("res_company")
            .fields(const { &[FieldDecl::char("name")] }),
    ));
    builder(db, addons)
        .identity_previous(previous)
        .decls([declaration(company), company_model])
}

#[tokio::test]
async fn genuine_addon_upgrade_is_atomic_and_repeatable() {
    for stage in ["post", "end"] {
        let addons = Addons::new();
        addons.add(
            "property_upgrade",
            "name='Property'\nversion='1.0'\ndepends=['base']\n",
        );
        let previous = std::fs::read_dir(addons.path())
            .unwrap()
            .map(|entry| {
                let e = entry.unwrap();
                let name = e.file_name().into_string().unwrap();
                let snap =
                    ArtifactSnapshot::capture(&name, &e.path().canonicalize().unwrap()).unwrap();
                (n(&name), snap.digest().to_owned())
            })
            .collect::<BTreeMap<_, _>>();
        let db = TestDb::create().await.unwrap();
        let system = configured(&db, &addons, false, previous.clone())
            .build()
            .unwrap();
        let initial = system
            .initialize(InitOptions::new().with_skip_auto_install(true))
            .await
            .unwrap();
        let installed = system
            .button_immediate_install(&su(&initial.orm), &[n("property_upgrade")])
            .await
            .unwrap();
        let mut tx = installed.orm.begin(erp_sdk::SUPERUSER_ID).await.unwrap();
        tx.batch("INSERT INTO res_company(id,name) VALUES(1,'One'); INSERT INTO upgrade_property(id,note) VALUES(1,'legacy'); COMMENT ON COLUMN upgrade_property.note IS 'old comment'; CREATE INDEX preserved_id ON upgrade_property(id)").await.unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        addons.add(
            "property_upgrade",
            "name='Property'\nversion='1.1'\ndepends=['base']\n",
        );
        addons.file(
            "property_upgrade",
            &format!("migrations/1.1/{stage}-failure.sql"),
            "SELECT 1/0;",
        );
        let target = configured(&db, &addons, true, previous.clone())
            .build()
            .unwrap();
        let error = target
            .button_immediate_upgrade(
                &target.system_env(Default::default()),
                &[n("property_upgrade")],
            )
            .await
            .unwrap_err();
        let expected_stage = if stage == "post" {
            erp_modules::migrations::MigrationStage::Post
        } else {
            erp_modules::migrations::MigrationStage::End
        };
        assert!(
            matches!(&error,erp_modules::error::Error::StepFailed{module,step,file:Some(_),..} if *module==n("property_upgrade") && *step==erp_modules::error::ModuleStep::Migration{stage:expected_stage}),
            "{error:#?}"
        );
        if let erp_modules::error::Error::StepFailed {
            file: Some(file), ..
        } = &error
        {
            // The temporary addon-root prefix is selected by the host fixture.
            assert!(file.ends_with(&format!("migrations/1.1/{stage}-failure.sql")));
            assert_eq!(
                error.to_string(),
                format!("Module \"property_upgrade\": {stage} migration '{file}' failed")
            );
        }
        let conn = db.pool().get().await.unwrap();
        let row=conn.query_one("SELECT note,pg_typeof(note)::text,col_description('upgrade_property'::regclass,(SELECT attnum FROM pg_attribute WHERE attrelid='upgrade_property'::regclass AND attname='note')) FROM upgrade_property",&[]).await.unwrap();
        assert_eq!(row.get::<_, String>(0), "legacy");
        assert_eq!(row.get::<_, String>(1), "character varying");
        assert_eq!(row.get::<_, String>(2), "old comment");
        let row = conn
            .query_one(
                "SELECT state,latest_version FROM ir_module_module WHERE name='property_upgrade'",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, String>(0), "installed");
        assert_eq!(row.get::<_, String>(1), "1.0.0");
        assert!(
            conn.query_one("SELECT to_regclass('preserved_id') IS NOT NULL", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        drop(conn);
        addons.file(
            "property_upgrade",
            &format!("migrations/1.1/{stage}-failure.sql"),
            "SELECT 1;",
        );
        let target = configured(&db, &addons, true, previous).build().unwrap();
        let outcome = target
            .button_immediate_upgrade(
                &target.system_env(Default::default()),
                &[n("property_upgrade")],
            )
            .await
            .unwrap();
        assert_eq!(
            rows(&outcome.orm, "SELECT note->>'1' FROM upgrade_property").await,
            vec![vec![Some("legacy".to_owned())]]
        );
        let repeat = target
            .button_immediate_upgrade(&su(&outcome.orm), &[n("property_upgrade")])
            .await
            .unwrap();
        assert_eq!(
            rows(&repeat.orm, "SELECT note->>'1' FROM upgrade_property").await,
            vec![vec![Some("legacy".to_owned())]]
        );
        db.cleanup().await.unwrap();
    }
}

mod whole {
    use erp_sdk::migrations::{CompanyMigrationPolicy, SemverVersion};
    use erp_sdk::{Result, Transaction};
    /// The explicit owner of every migrated value of `whole.property`.
    pub const OWNER: i64 = 1;
    #[erp_sdk::migration(addon="property_whole",version="1.1",stage=pre)]
    pub async fn assign(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
        let policy = CompanyMigrationPolicy::companies([OWNER])?;
        tx.migrate_company_dependent("whole.property", "note", &policy)
            .await?;
        tx.migrate_company_dependent("whole.property", "link", &policy)
            .await
    }
}
/// The releases of the `property_whole` fixture addon.
#[derive(Clone, Copy, PartialEq)]
enum Release {
    /// 1.0: scalar columns.
    Scalar,
    /// 1.1: `note`, `link` and `spare` company-dependent, `label` translated; `whole.empty.value`
    /// company-dependent.
    Company,
    /// 1.2: as 1.1 with `note` a plain scalar again.
    Reverse,
}
fn leak(decl: ModelDecl) -> &'static ModelDecl {
    Box::leak(Box::new(decl))
}
fn whole_decls(release: Release) -> Vec<&'static ModelDecl> {
    let company = release != Release::Scalar;
    let fields = vec![
        FieldDecl::char("note")
            .index(Some(erp_sdk::IndexKind::BtreeNotNull))
            .company_dependent(release == Release::Company),
        FieldDecl::many2one("link", "res.company").company_dependent(company),
        FieldDecl::char("label").translate(company),
        FieldDecl::char("spare").company_dependent(company),
    ];
    vec![
        leak(
            ModelDecl::new("property_whole", "whole.property")
                .table("whole_property")
                .fields(Box::leak(fields.into_boxed_slice())),
        ),
        leak(
            ModelDecl::new("property_whole", "whole.empty")
                .table("whole_empty")
                .fields(Box::leak(
                    vec![FieldDecl::char("value").company_dependent(company)].into_boxed_slice(),
                )),
        ),
    ]
}
fn implicit_decls(company: bool) -> Vec<&'static ModelDecl> {
    vec![leak(
        ModelDecl::new("property_implicit", "implicit.property")
            .table("implicit_property")
            .fields(Box::leak(
                vec![
                    FieldDecl::char("note").company_dependent(company),
                    FieldDecl::char("kept"),
                ]
                .into_boxed_slice(),
            )),
    )]
}
/// The module system of one release: the fixture's declarations and the company table.
fn release_system(
    db: &TestDb,
    addons: &Addons,
    previous: &BTreeMap<erp_modules::manifest::ModuleName, String>,
    decls: Vec<&'static ModelDecl>,
) -> erp_modules::system::ModuleSystem {
    let company_model = leak(
        ModelDecl::new("base", "res.company")
            .table("res_company")
            .fields(const { &[FieldDecl::char("name")] }),
    );
    builder(db, addons)
        .identity_previous(previous.clone())
        .decls(decls.into_iter().chain([company_model]))
        .build()
        .unwrap()
}
/// The digests of the addons as they are on disk now: the installed release.
fn release_digests(addons: &Addons) -> BTreeMap<erp_modules::manifest::ModuleName, String> {
    std::fs::read_dir(addons.path())
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let snapshot =
                ArtifactSnapshot::capture(&name, &entry.path().canonicalize().unwrap()).unwrap();
            (n(&name), snapshot.digest().to_owned())
        })
        .collect()
}
/// Everything an upgrade of `module` may touch, as text: columns (type, NOT NULL, comment),
/// constraints, indexes and rows of `tables`, and the module's state and version.
async fn fingerprint(db: &TestDb, module: &str, tables: &[&str]) -> Vec<String> {
    let conn = db.pool().get().await.unwrap();
    let names: Vec<String> = tables.iter().map(|table| (*table).to_owned()).collect();
    let mut lines = Vec::new();
    for sql in [
        "SELECT format('column %s.%s %s notnull=%s comment=%s', c.relname, a.attname, t.typname, a.attnotnull, col_description(c.oid, a.attnum)) FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_type t ON t.oid=a.atttypid WHERE c.relname=ANY($1) AND c.relkind='r' AND a.attnum>0 AND NOT a.attisdropped ORDER BY 1",
        "SELECT format('constraint %s %s %s', k.conrelid::regclass, k.conname, pg_get_constraintdef(k.oid)) FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid WHERE c.relname=ANY($1) ORDER BY 1",
        "SELECT format('index %s', indexdef) FROM pg_indexes WHERE tablename=ANY($1) ORDER BY 1",
    ] {
        for row in conn.query(sql, &[&names]).await.unwrap() {
            lines.push(row.get::<_, String>(0));
        }
    }
    for table in tables {
        let sql = format!(
            "SELECT format('row %s %s', '{table}', to_jsonb(t)) FROM {table} t ORDER BY id"
        );
        for row in conn.query(&sql, &[]).await.unwrap() {
            lines.push(row.get::<_, String>(0));
        }
    }
    let row = conn
        .query_one(
            "SELECT format('module %s %s %s', name, state, latest_version) FROM ir_module_module WHERE name=$1",
            &[&module],
        )
        .await
        .unwrap();
    lines.push(row.get::<_, String>(0));
    lines
}
/// The stored JSON of `column` per row of `table`, in id order (`None` for SQL NULL).
async fn stored_maps(db: &TestDb, table: &str, column: &str) -> Vec<Option<serde_json::Value>> {
    let conn = db.pool().get().await.unwrap();
    let sql = format!("SELECT {column} FROM {table} ORDER BY id");
    conn.query(&sql, &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}
/// The ORM error a failed schema step carries.
fn schema_error(error: &erp_modules::error::Error) -> Option<&erp_fields::Error> {
    match error {
        erp_modules::error::Error::Orm(inner) => Some(inner),
        _ => None,
    }
}
async fn upgrade(
    system: &erp_modules::system::ModuleSystem,
    module: &str,
) -> Result<erp_modules::system::OperationOutcome, erp_modules::error::Error> {
    system
        .button_immediate_upgrade(&system.system_env(Default::default()), &[n(module)])
        .await
}

#[tokio::test]
async fn spec_compmig_002_atomic_migration_and_refusal_without_ownership() {
    // 1. No explicit migration: a populated conversion refuses and changes nothing; the same
    //    conversion of a column whose values are all NULL is safe.
    {
        const TABLES: &[&str] = &["implicit_property"];
        let addons = Addons::new();
        addons.add(
            "property_implicit",
            "name='Implicit'\nversion='1.0'\ndepends=['base']\n",
        );
        let previous = release_digests(&addons);
        let db = TestDb::create().await.unwrap();
        let system = release_system(&db, &addons, &previous, implicit_decls(false));
        let initial = system
            .initialize(InitOptions::new().with_skip_auto_install(true))
            .await
            .unwrap();
        let installed = system
            .button_immediate_install(&su(&initial.orm), &[n("property_implicit")])
            .await
            .unwrap();
        exec(
            &installed.orm,
            "INSERT INTO implicit_property(id,note,kept) VALUES(1,'legacy','first'),(2,NULL,'second')",
        )
        .await;
        let predecessor = fingerprint(&db, "property_implicit", TABLES).await;
        assert!(predecessor.contains(&"module property_implicit installed 1.0.0".to_owned()));
        assert!(
            !predecessor
                .iter()
                .any(|line| line.starts_with("column implicit_property.note jsonb"))
        );
        addons.add(
            "property_implicit",
            "name='Implicit'\nversion='1.1'\ndepends=['base']\n",
        );
        let target = release_system(&db, &addons, &previous, implicit_decls(true));
        let error = upgrade(&target, "property_implicit").await.unwrap_err();
        let (module, step, file, source) = step_failed(&error);
        assert_eq!(
            (module, step, file),
            (
                "property_implicit",
                erp_modules::error::ModuleStep::Schema,
                None
            )
        );
        assert_eq!(
            error.to_string(),
            "Module \"property_implicit\": schema update failed"
        );
        let inner = schema_error(source).expect("the schema step fails with an ORM error");
        assert_eq!(inner.kind(), erp_fields::ErrorKind::Programming);
        assert_eq!(
            inner.message(),
            "implicit.property.note: populated scalar conversion requires an explicit company migration policy"
        );
        assert_eq!(
            fingerprint(&db, "property_implicit", TABLES).await,
            predecessor
        );
        // All values NULL: the same upgrade, still without any migration, converts the column.
        exec(&installed.orm, "UPDATE implicit_property SET note=NULL").await;
        let target = release_system(&db, &addons, &previous, implicit_decls(true));
        committed(upgrade(&target, "property_implicit").await.unwrap());
        let converted = fingerprint(&db, "property_implicit", TABLES).await;
        assert!(converted.contains(&"module property_implicit installed 1.1.0".to_owned()));
        assert!(
            converted
                .iter()
                .any(|line| line.starts_with("column implicit_property.note jsonb notnull=f"))
        );
        assert_eq!(
            stored_maps(&db, "implicit_property", "note").await,
            vec![None, None]
        );
        assert_eq!(
            stored_maps(&db, "implicit_property", "to_jsonb(kept)").await,
            vec![
                Some(serde_json::json!("first")),
                Some(serde_json::json!("second"))
            ]
        );
        db.cleanup().await.unwrap();
    }

    // 2. An explicit pre-migration and a later failing step, for each later stage.
    const TABLES: &[&str] = &["whole_property", "whole_empty"];
    // (id, note, link, label)
    type LegacyRow = (i64, Option<&'static str>, Option<i64>, Option<&'static str>);
    let legacy: [LegacyRow; 3] = [
        (1, Some("legacy"), Some(2), Some("Label")),
        (2, None, None, Some("")),
        (3, Some(""), Some(1), None),
    ];
    for stage in ["post", "end"] {
        let addons = Addons::new();
        addons.add(
            "property_whole",
            "name='Whole'\nversion='1.0'\ndepends=['base']\n",
        );
        let previous = release_digests(&addons);
        let db = TestDb::create().await.unwrap();
        let system = release_system(&db, &addons, &previous, whole_decls(Release::Scalar));
        let initial = system
            .initialize(InitOptions::new().with_skip_auto_install(true))
            .await
            .unwrap();
        let installed = system
            .button_immediate_install(&su(&initial.orm), &[n("property_whole")])
            .await
            .unwrap();
        exec(
            &installed.orm,
            "INSERT INTO res_company(id,name) VALUES(1,'One'),(2,'Two'); COMMENT ON COLUMN whole_property.note IS 'old comment'; CREATE INDEX preserved_id ON whole_property(id)",
        )
        .await;
        {
            let conn = db.pool().get().await.unwrap();
            for (id, note, link, label) in &legacy {
                conn.execute(
                    "INSERT INTO whole_property(id,note,link,label) VALUES($1::int8,$2::varchar,$3::int8,$4::varchar)",
                    &[id, note, link, label],
                )
                .await
                .unwrap();
            }
        }
        // The genuine predecessor: scalar columns, the ORM's foreign key of `link`, the index
        // of `note`, the comment, the rows, and the module at 1.0.
        let predecessor = fingerprint(&db, "property_whole", TABLES).await;
        assert!(predecessor.contains(&"module property_whole installed 1.0.0".to_owned()));
        assert!(!predecessor.iter().any(|line| line.contains(" jsonb ")));
        let link_keys = |lines: &[String]| {
            lines
                .iter()
                .filter(|line| {
                    line.starts_with("constraint whole_property ")
                        && line.contains("FOREIGN KEY (link)")
                })
                .count()
        };
        let note_indexes = |lines: &[String]| {
            lines
                .iter()
                .filter(|line| line.starts_with("index ") && line.contains("(note)"))
                .count()
        };
        assert_eq!(link_keys(&predecessor), 1);
        assert_eq!(note_indexes(&predecessor), 1);
        assert!(
            predecessor
                .iter()
                .any(|line| line.starts_with("column whole_property.note ")
                    && line.ends_with("comment=old comment"))
        );

        // The upgrade to 1.1: the pre-migration and the schema step succeed, then a later
        // migration step fails. Data, schema, dependent objects and the module's version and
        // state are the predecessor's, together.
        addons.add(
            "property_whole",
            "name='Whole'\nversion='1.1'\ndepends=['base']\n",
        );
        addons.file(
            "property_whole",
            &format!("migrations/1.1/{stage}-failure.sql"),
            "SELECT 1/0;",
        );
        let target = release_system(&db, &addons, &previous, whole_decls(Release::Company));
        let error = upgrade(&target, "property_whole").await.unwrap_err();
        let expected_stage = if stage == "post" {
            erp_modules::migrations::MigrationStage::Post
        } else {
            erp_modules::migrations::MigrationStage::End
        };
        let (module, step, file, _) = step_failed(&error);
        assert_eq!(
            (module, step),
            (
                "property_whole",
                erp_modules::error::ModuleStep::Migration {
                    stage: expected_stage
                }
            )
        );
        let file = file.expect("a migration step names its file");
        // The temporary addon-root prefix is selected by the host fixture.
        assert!(file.ends_with(&format!("migrations/1.1/{stage}-failure.sql")));
        assert_eq!(
            error.to_string(),
            format!("Module \"property_whole\": {stage} migration '{file}' failed")
        );
        assert_eq!(
            fingerprint(&db, "property_whole", TABLES).await,
            predecessor,
            "{stage}"
        );

        // The retry starts from that predecessor: the maps are the legacy scalars under the
        // migration's explicit owner, converted once.
        addons.file(
            "property_whole",
            &format!("migrations/1.1/{stage}-failure.sql"),
            "SELECT 1;",
        );
        let target = release_system(&db, &addons, &previous, whole_decls(Release::Company));
        committed(upgrade(&target, "property_whole").await.unwrap());
        let owner = whole::OWNER.to_string();
        let expected_note: Vec<Option<serde_json::Value>> = legacy
            .iter()
            .map(|(_, note, _, _)| note.map(|value| serde_json::json!({ owner.as_str(): value })))
            .collect();
        let expected_link: Vec<Option<serde_json::Value>> = legacy
            .iter()
            .map(|(_, _, link, _)| link.map(|value| serde_json::json!({ owner.as_str(): value })))
            .collect();
        // Translation conversion is independent: `label` had values and no company policy, and
        // became a translation map (source language), never a company map.
        let expected_label: Vec<Option<serde_json::Value>> = legacy
            .iter()
            .map(|(_, _, _, label)| label.map(|value| serde_json::json!({ "en_US": value })))
            .collect();
        let nulls: Vec<Option<serde_json::Value>> = legacy.iter().map(|_| None).collect();
        let check_upgraded = async |what: &str| -> Vec<String> {
            assert_eq!(
                stored_maps(&db, "whole_property", "note").await,
                expected_note,
                "{what}"
            );
            assert_eq!(
                stored_maps(&db, "whole_property", "link").await,
                expected_link,
                "{what}"
            );
            assert_eq!(
                stored_maps(&db, "whole_property", "label").await,
                expected_label,
                "{what}"
            );
            // the all-NULL column and the empty table converted without a policy
            assert_eq!(
                stored_maps(&db, "whole_property", "spare").await,
                nulls,
                "{what}"
            );
            assert_eq!(stored_maps(&db, "whole_empty", "value").await, vec![]);
            let lines = fingerprint(&db, "property_whole", TABLES).await;
            assert!(
                lines.contains(&"module property_whole installed 1.1.0".to_owned()),
                "{what}"
            );
            for column in [
                "whole_property.note",
                "whole_property.link",
                "whole_property.label",
                "whole_property.spare",
                "whole_empty.value",
            ] {
                assert!(
                    lines
                        .iter()
                        .any(|line| line.starts_with(&format!("column {column} jsonb "))),
                    "{what}: {column}"
                );
            }
            // the obsolete scalar foreign key is gone; index, comment and custom index remain
            assert_eq!(link_keys(&lines), 0, "{what}");
            assert_eq!(note_indexes(&lines), 1, "{what}");
            assert!(lines.iter().any(|line| line.contains("preserved_id")));
            assert!(
                lines
                    .iter()
                    .any(|line| line.starts_with("column whole_property.note ")
                        && line.ends_with("comment=old comment")),
                "{what}"
            );
            lines
        };
        check_upgraded("retry").await;
        let target = release_system(&db, &addons, &previous, whole_decls(Release::Company));
        committed(upgrade(&target, "property_whole").await.unwrap());
        let upgraded = check_upgraded("repeated upgrade").await;

        // Reverse conversion refuses: a release that makes `note` a plain scalar again fails in
        // its schema step and leaves the maps, the schema and the module's version.
        addons.add(
            "property_whole",
            "name='Whole'\nversion='1.2'\ndepends=['base']\n",
        );
        let reverse = release_system(&db, &addons, &previous, whole_decls(Release::Reverse));
        let error = upgrade(&reverse, "property_whole").await.unwrap_err();
        let (module, step, file, source) = step_failed(&error);
        assert_eq!(
            (module, step, file),
            (
                "property_whole",
                erp_modules::error::ModuleStep::Schema,
                None
            )
        );
        assert_eq!(
            error.to_string(),
            "Module \"property_whole\": schema update failed"
        );
        let inner = schema_error(source).expect("the schema step fails with an ORM error");
        assert_eq!(inner.kind(), erp_fields::ErrorKind::Validation);
        assert_eq!(
            inner.message(),
            "cannot update the schema: conversion of jsonb field 'whole.property.note' to varchar on populated table \"whole_property\" refused: converting jsonb column to varchar would destroy existing JSON data; manual migration required"
        );
        assert_eq!(
            fingerprint(&db, "property_whole", TABLES).await,
            upgraded,
            "{stage}"
        );
        db.cleanup().await.unwrap();
    }
}
