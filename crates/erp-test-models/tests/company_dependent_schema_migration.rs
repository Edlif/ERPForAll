// SPDX-License-Identifier: MIT OR Apache-2.0
//! Genuine scalar predecessor adoption through an explicit native migration policy.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl, RegistryBuilder};
use erp_sdk::migrations::CompanyMigrationPolicy;
use erp_sdk::{Digits, ErrorKind, IndexKind, Orm, SUPERUSER_ID, StaticPolicy, Transaction};
use erp_test_models::base_users;
use erp_testkit::Harness;
use serde_json::{Value, json};
use std::sync::Arc;

static ADDON: AddonDescriptor = AddonDescriptor {
    name: "company_migration",
    depends: &["base"],
    exact_only: false,
};
fn decl(fields: Vec<FieldDecl>) -> ModelDecl {
    let company_scoped = fields.iter().any(|f| f.name == "company_id");
    let mut model = ModelDecl::new("company_migration", "migration.property")
        .table("migration_property")
        .fields(Box::leak(fields.into_boxed_slice()));
    if company_scoped {
        model = model.company_access(erp_registry::CompanyAccessDecl::unrestricted(
            "Trusted migration fixture exercises shared predecessor rows",
        ));
    }
    model
}
async fn predecessor(fields: Vec<FieldDecl>) -> Harness {
    let h = Harness::builder([&base_users::ADDON, &erp_addon_base::ADDON, &ADDON])
        .with_decls([decl(fields)])
        .build()
        .await
        .unwrap();
    let mut tx = h.begin().await.unwrap();
    let currency = erp_testkit::fixtures::seed_currency_in(&mut tx)
        .await
        .unwrap();
    tx.execute("INSERT INTO res_company(tz, id,name,currency_id) VALUES ('UTC', 1,'One',$1),('UTC', 2,'Two',$1),('UTC', 3,'Three',$1) ON CONFLICT DO NOTHING", &[&currency]).await.unwrap();
    tx.commit().await.unwrap();
    h
}
fn successor(h: &Harness, fields: Vec<FieldDecl>) -> Orm {
    let target = Box::leak(Box::new(decl(fields)));
    let registry = RegistryBuilder::new([&base_users::ADDON, &erp_addon_base::ADDON, &ADDON])
        .with_decls([&*target])
        .build()
        .unwrap();
    Orm::builder(Arc::new(registry), h.db().pool().clone())
        .policy(StaticPolicy::AllowAll)
        .build()
        .unwrap()
}
async fn map(tx: &mut Transaction, field: &str) -> Vec<Option<Value>> {
    tx.query(
        &format!(
            "SELECT {} FROM migration_property ORDER BY id",
            erp_sql::quote(field)
        ),
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| r.get(0))
    .collect()
}
fn property(field: FieldDecl) -> FieldDecl {
    field.company_dependent(true)
}

#[tokio::test]
async fn explicit_set_preserves_exact_scalars_and_catalog() {
    let fields = || {
        vec![
            FieldDecl::char("note").index(Some(IndexKind::BtreeNotNull)),
            FieldDecl::float_digits("cost", Digits::Unrounded),
            FieldDecl::boolean("flag"),
            FieldDecl::integer("count"),
        ]
    };
    let h = predecessor(fields()).await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("INSERT INTO migration_property(id,note,cost,flag,count) VALUES (1,'kept',9007199254740993.123456789,true,7),(2,'',-0.0000000000000000000000000001,false,0),(3,NULL,NULL,NULL,NULL); COMMENT ON COLUMN migration_property.note IS 'legacy note'; COMMENT ON INDEX migration_property__note_index IS 'legacy index'").await.unwrap();
    let oid: i64 = tx
        .query(
            "SELECT oid::int8 FROM pg_class WHERE oid='migration_property'::regclass",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.commit().await.unwrap();
    let orm = successor(&h, fields().into_iter().map(property).collect());
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let policy = CompanyMigrationPolicy::companies([1, 2]).unwrap();
    for field in ["note", "cost", "flag", "count"] {
        tx.migrate_company_dependent("migration.property", field, &policy)
            .await
            .unwrap();
    }
    assert_eq!(
        map(&mut tx, "note").await,
        vec![
            Some(json!({"1":"kept","2":"kept"})),
            Some(json!({"1":"","2":""})),
            None
        ]
    );
    assert_eq!(
        map(&mut tx, "cost").await,
        vec![
            Some(json!({"1":"9007199254740993.123456789","2":"9007199254740993.123456789"})),
            Some(
                json!({"1":"-0.0000000000000000000000000001","2":"-0.0000000000000000000000000001"})
            ),
            None
        ]
    );
    assert_eq!(
        map(&mut tx, "flag").await,
        vec![
            Some(json!({"1":true,"2":true})),
            Some(json!({"1":false,"2":false})),
            None
        ]
    );
    assert_eq!(
        map(&mut tx, "count").await,
        vec![Some(json!({"1":7,"2":7})), Some(json!({"1":0,"2":0})), None]
    );
    let meta=tx.query("SELECT c.oid::int8,col_description(c.oid,a.attnum),a.attnotnull FROM pg_class c JOIN pg_attribute a ON a.attrelid=c.oid WHERE c.oid='migration_property'::regclass AND a.attname='note'",&[]).await.unwrap();
    assert_eq!(meta[0].get::<_, i64>(0), oid);
    assert_eq!(meta[0].get::<_, String>(1), "legacy note");
    assert!(!meta[0].get::<_, bool>(2));
    assert_eq!(
        tx.query(
            "SELECT obj_description('migration_property__note_index'::regclass)",
            &[]
        )
        .await
        .unwrap()[0]
            .get::<_, String>(0),
        "legacy index"
    );
    let before = map(&mut tx, "note").await;
    tx.init_models(&[]).await.unwrap();
    tx.migrate_company_dependent(
        "migration.property",
        "note",
        &CompanyMigrationPolicy::companies([3]).unwrap(),
    )
    .await
    .unwrap();
    tx.init_models(&[]).await.unwrap();
    assert_eq!(map(&mut tx, "note").await, before);
    let indexes=tx.query("SELECT indexdef FROM pg_indexes WHERE tablename='migration_property' AND indexname='migration_property__note_index'",&[]).await.unwrap();
    assert_eq!(indexes.len(), 1);
    assert!(
        indexes[0]
            .get::<_, String>(0)
            .contains("WHERE (note IS NOT NULL)")
    );
    tx.commit().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn row_company_is_explicit_and_unassigned_values_refuse() {
    let fields = || {
        vec![
            FieldDecl::char("note"),
            FieldDecl::many2one("company_id", "res.company"),
        ]
    };
    let h = predecessor(fields()).await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("INSERT INTO migration_property(id,note,company_id) VALUES (1,'one',1),(2,'two',2),(3,'shared',NULL)").await.unwrap();
    tx.commit().await.unwrap();
    let orm = successor(
        &h,
        vec![
            property(FieldDecl::char("note")),
            FieldDecl::many2one("company_id", "res.company"),
        ],
    );
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let policy = CompanyMigrationPolicy::row_company("company_id").unwrap();
    let err = tx
        .savepoint(async |tx| {
            tx.migrate_company_dependent("migration.property", "note", &policy)
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        "migration.property.note: non-null predecessor value has no valid company owner"
    );
    tx.execute("UPDATE migration_property SET note=NULL WHERE id=3", &[])
        .await
        .unwrap();
    tx.migrate_company_dependent("migration.property", "note", &policy)
        .await
        .unwrap();
    assert_eq!(
        map(&mut tx, "note").await,
        vec![Some(json!({"1":"one"})), Some(json!({"2":"two"})), None]
    );
    tx.commit().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn implicit_adoption_refuses_and_later_failure_restores_predecessor() {
    let h = predecessor(vec![FieldDecl::char("note"), FieldDecl::char("empty")]).await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("INSERT INTO migration_property(id,note) VALUES (1,'keep')")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let orm = successor(
        &h,
        vec![
            property(FieldDecl::char("note")),
            property(FieldDecl::char("empty")),
        ],
    );
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let err = tx
        .savepoint(async |tx| tx.init_models(&[]).await)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        "migration.property.note: populated scalar conversion requires an explicit company migration policy"
    );
    let policy = CompanyMigrationPolicy::companies([2]).unwrap();
    let err = tx
        .savepoint(async |tx| {
            tx.migrate_company_dependent("migration.property", "note", &policy)
                .await?;
            tx.batch("SELECT 1/0").await
        })
        .await
        .unwrap_err();
    assert_eq!(err.sqlstate(), Some("22012"));
    assert_eq!(
        tx.query("SELECT note FROM migration_property", &[])
            .await
            .unwrap()[0]
            .get::<_, String>(0),
        "keep"
    );
    tx.migrate_company_dependent("migration.property", "note", &policy)
        .await
        .unwrap();
    tx.init_models(&[]).await.unwrap();
    assert_eq!(map(&mut tx, "empty").await, vec![None]);
    tx.commit().await.unwrap();
    drop(tx);
    let reverse = successor(
        &h,
        vec![FieldDecl::char("note"), property(FieldDecl::char("empty"))],
    );
    let mut tx = reverse.begin(SUPERUSER_ID).await.unwrap();
    let err = tx.init_models(&[]).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        "cannot update the schema: conversion of jsonb field 'migration.property.note' to varchar on populated table \"migration_property\" refused: converting jsonb column to varchar would destroy existing JSON data; manual migration required"
    );
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn policy_admission_is_bounded_and_never_uses_installer_company() {
    for ids in [vec![], vec![0], vec![-1], vec![1, 1]] {
        let err = CompanyMigrationPolicy::companies(ids.clone()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Programming);
        assert_eq!(
            err.message(),
            if ids.is_empty() {
                "invalid company migration policy: at least one company is required"
            } else {
                "invalid company migration policy: owners must be unique positive IDs within MAX_OWNERS"
            }
        );
    }
    let err = CompanyMigrationPolicy::row_company("x\"; DROP TABLE res_company;--").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        "invalid company migration policy: row-company field must be a PostgreSQL field identifier"
    );
    let h = predecessor(vec![FieldDecl::char("note")]).await;
    let orm = successor(&h, vec![property(FieldDecl::char("note"))]);
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let err = tx
        .migrate_company_dependent(
            "migration.property",
            "note",
            &CompanyMigrationPolicy::companies([999999]).unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        "migration.property.note: company migration policy names an unknown company"
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn temporal_and_relation_migration_keeps_types_and_unrelated_objects() {
    let fields = || {
        vec![
            FieldDecl::date("day"),
            FieldDecl::datetime("moment"),
            FieldDecl::float("ratio"),
            FieldDecl::text("body"),
            FieldDecl::html("html"),
            FieldDecl::many2one("link", "res.company"),
        ]
    };
    let h = predecessor(fields()).await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("INSERT INTO migration_property(id,day,moment,ratio,body,html,link) VALUES (1,'2024-02-29','2024-02-29 12:34:56.123456',1.25,'text','<p>original</p>',2); CREATE INDEX unrelated_id ON migration_property(id)").await.unwrap();
    tx.commit().await.unwrap();
    let orm = successor(&h, fields().into_iter().map(property).collect());
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.batch("SET LOCAL DateStyle TO 'SQL, DMY'; SET LOCAL TIME ZONE 'Asia/Jakarta'")
        .await
        .unwrap();
    let policy = CompanyMigrationPolicy::companies([1]).unwrap();
    for name in ["day", "moment", "ratio", "body", "html", "link"] {
        tx.migrate_company_dependent("migration.property", name, &policy)
            .await
            .unwrap();
    }
    assert_eq!(
        map(&mut tx, "day").await,
        vec![Some(json!({"1":"2024-02-29"}))]
    );
    assert_eq!(
        map(&mut tx, "moment").await,
        vec![Some(json!({"1":"2024-02-29 12:34:56.123456"}))]
    );
    assert_eq!(map(&mut tx, "ratio").await, vec![Some(json!({"1":1.25}))]);
    assert_eq!(
        map(&mut tx, "html").await,
        vec![Some(json!({"1":"<p>original</p>"}))]
    );
    assert_eq!(map(&mut tx, "link").await, vec![Some(json!({"1":2}))]);
    tx.init_models(&[]).await.unwrap();
    let rows=tx.query("SELECT count(*) FROM pg_constraint WHERE conrelid='migration_property'::regclass AND contype='f' AND conname='migration_property_link_fkey'",&[]).await.unwrap();
    assert_eq!(rows[0].get::<_, i64>(0), 0);
    assert!(
        tx.query("SELECT to_regclass('unrelated_id') IS NOT NULL", &[])
            .await
            .unwrap()[0]
            .get::<_, bool>(0)
    );
    tx.commit().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn unsafe_dependencies_and_invalid_values_refuse_before_mutation() {
    let h = predecessor(vec![
        FieldDecl::char("note"),
        FieldDecl::float_digits("cost", Digits::Unrounded),
    ])
    .await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("INSERT INTO migration_property(id,note,cost) VALUES (1,'keep',123); CREATE UNIQUE INDEX custom_unique_note ON migration_property(note)").await.unwrap();
    tx.commit().await.unwrap();
    let orm = successor(
        &h,
        vec![
            property(FieldDecl::char("note")),
            property(FieldDecl::float_digits("cost", Digits::Unrounded)),
        ],
    );
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let policy = CompanyMigrationPolicy::companies([1]).unwrap();
    let err = tx
        .savepoint(async |tx| {
            tx.migrate_company_dependent("migration.property", "note", &policy)
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        "migration.property.note: company migration refuses dependent index 'custom_unique_note'; migrate it explicitly"
    );
    assert_eq!(
        tx.query("SELECT note FROM migration_property", &[])
            .await
            .unwrap()[0]
            .get::<_, String>(0),
        "keep"
    );
    tx.batch("DROP INDEX custom_unique_note; ALTER TABLE migration_property ADD CONSTRAINT custom_check CHECK(length(note)>0)").await.unwrap();
    let err = tx
        .savepoint(async |tx| {
            tx.migrate_company_dependent("migration.property", "note", &policy)
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Programming);
    assert_eq!(
        err.message(),
        "migration.property.note: company migration refuses dependent constraint 'custom_check'; migrate it explicitly"
    );
    tx.batch("ALTER TABLE migration_property DROP CONSTRAINT custom_check; UPDATE migration_property SET cost='NaN'").await.unwrap();
    let err = tx
        .savepoint(async |tx| {
            tx.migrate_company_dependent("migration.property", "cost", &policy)
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        "migration.property.cost: invalid predecessor value"
    );
    tx.batch("ALTER TABLE migration_property ALTER COLUMN note TYPE jsonb USING '{\"1\":\"valid\",\"3\":{\"secret\":true}}'::jsonb").await.unwrap();
    let err = tx
        .migrate_company_dependent("migration.property", "note", &policy)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Validation);
    assert_eq!(
        err.message(),
        "migration.property.note: invalid company-dependent JSONB storage; an explicit migration is required"
    );
    assert!(!err.message().contains("secret"));
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn monetary_selection_and_required_fields_keep_legacy_values() {
    let fields = || {
        vec![
            FieldDecl::many2one("currency_id", "res.currency"),
            FieldDecl::new(
                "amount",
                erp_registry::KindDecl::Monetary {
                    currency_field: Some("currency_id"),
                },
            ),
            FieldDecl::selection("choice", &[("a", "First"), ("b", "Second")]),
            FieldDecl::char("required_note").required(true),
        ]
    };
    let h = predecessor(fields()).await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("INSERT INTO migration_property(id,amount,choice,required_note) VALUES (1,9007199254740993.125,'a','present')").await.unwrap();
    tx.commit().await.unwrap();
    let mut target = fields();
    for field in &mut target {
        if field.name != "currency_id" {
            *field = (*field).company_dependent(true);
        }
    }
    let orm = successor(&h, target);
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let policy = CompanyMigrationPolicy::companies([2]).unwrap();
    for field in ["amount", "choice", "required_note"] {
        tx.migrate_company_dependent("migration.property", field, &policy)
            .await
            .unwrap();
    }
    tx.init_models(&[]).await.unwrap();
    assert_eq!(
        map(&mut tx, "amount").await,
        vec![Some(json!({"2":"9007199254740993.125"}))]
    );
    assert_eq!(map(&mut tx, "choice").await, vec![Some(json!({"2":"a"}))]);
    assert!(tx.query("SELECT NOT attnotnull FROM pg_attribute WHERE attrelid='migration_property'::regclass AND attname='required_note'",&[]).await.unwrap()[0].get::<_,bool>(0));
    tx.commit().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn migration_lock_serializes_competing_schema_work() {
    let h = predecessor(vec![FieldDecl::char("note")]).await;
    let orm = successor(&h, vec![property(FieldDecl::char("note"))]);
    let mut first = orm.begin(SUPERUSER_ID).await.unwrap();
    first
        .migrate_company_dependent(
            "migration.property",
            "note",
            &CompanyMigrationPolicy::companies([1]).unwrap(),
        )
        .await
        .unwrap();
    let other = successor(&h, vec![property(FieldDecl::char("note"))]);
    let mut second = other.begin(SUPERUSER_ID).await.unwrap();
    second.batch("SET LOCAL lock_timeout='50ms'").await.unwrap();
    let err = second.init_models(&[]).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Lock);
    assert_eq!(err.sqlstate(), Some("55P03"));
    // Database lock diagnostics contain server-selected relation/context text; SQLSTATE is stable.
    assert!(err.message().contains("lock"));
    second.rollback().await.unwrap();
    drop(second);
    first.rollback().await.unwrap();
    drop(first);
    let mut retry = orm.begin(SUPERUSER_ID).await.unwrap();
    retry.init_models(&[]).await.unwrap();
    retry.commit().await.unwrap();
    drop(retry);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn limits_and_authority_refuse_without_changing_scalar_data() {
    let error = CompanyMigrationPolicy::companies(1..=257).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming);
    assert_eq!(
        error.message(),
        "invalid company migration policy: owners must be unique positive IDs within MAX_OWNERS"
    );
    let h = predecessor(vec![FieldDecl::text("note")]).await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("INSERT INTO migration_property(id,note) VALUES(1,repeat('x',16777216))")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let orm = successor(&h, vec![property(FieldDecl::text("note"))]);
    let policy = CompanyMigrationPolicy::companies([1]).unwrap();
    let mut denied = orm.begin(erp_orm::UserId::new(2).unwrap()).await.unwrap();
    let error = denied
        .migrate_company_dependent("migration.property", "note", &policy)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access);
    assert_eq!(
        error.message(),
        "company migration requires an elevated native migration transaction"
    );
    drop(denied);
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let error = tx
        .migrate_company_dependent("migration.property", "note", &policy)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "migration.property.note: invalid predecessor value or company-map byte limit exceeded"
    );
    assert_eq!(
        tx.query("SELECT length(note) FROM migration_property", &[])
            .await
            .unwrap()[0]
            .get::<_, i32>(0),
        16777216
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn custom_reference_key_under_orm_name_is_never_dropped() {
    let h = predecessor(vec![FieldDecl::many2one("link", "res.company")]).await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("ALTER TABLE res_company ADD COLUMN external_key bigint UNIQUE; UPDATE res_company SET external_key=id; ALTER TABLE migration_property DROP CONSTRAINT migration_property_link_fkey; ALTER TABLE migration_property ADD CONSTRAINT migration_property_link_fkey FOREIGN KEY(link) REFERENCES res_company(external_key) ON DELETE SET NULL; INSERT INTO migration_property(id,link) VALUES(1,1)").await.unwrap();
    tx.commit().await.unwrap();
    let orm = successor(
        &h,
        vec![property(FieldDecl::many2one("link", "res.company"))],
    );
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let error = tx
        .migrate_company_dependent(
            "migration.property",
            "link",
            &CompanyMigrationPolicy::companies([1]).unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming);
    assert_eq!(
        error.message(),
        "migration.property.link: company migration refuses dependent constraint 'migration_property_link_fkey'; migrate it explicitly"
    );
    assert_eq!(
        tx.query("SELECT link FROM migration_property", &[])
            .await
            .unwrap()[0]
            .get::<_, i64>(0),
        1
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn maximum_owner_set_builds_every_override_without_argument_overflow() {
    let h = predecessor(vec![FieldDecl::char("note")]).await;
    let mut tx = h.begin().await.unwrap();
    tx.batch("INSERT INTO res_company(tz, id,name,currency_id) SELECT 'UTC', n,'Owner '||n,(SELECT currency_id FROM res_company WHERE id=1) FROM generate_series(4,256) n; INSERT INTO migration_property(id,note) VALUES(1,'same')").await.unwrap();
    tx.commit().await.unwrap();
    let orm = successor(&h, vec![property(FieldDecl::char("note"))]);
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.migrate_company_dependent(
        "migration.property",
        "note",
        &CompanyMigrationPolicy::companies(1..=256).unwrap(),
    )
    .await
    .unwrap();
    let values = map(&mut tx, "note").await;
    let object = values[0].as_ref().unwrap().as_object().unwrap();
    assert_eq!(object.len(), 256);
    for id in 1..=256 {
        assert_eq!(object.get(&id.to_string()), Some(&json!("same")));
    }
    drop(tx);
    h.cleanup().await.unwrap();
}

/// A company map that gives `value` to each of `owners` and to nobody else.
fn owned(owners: &[i64], value: &Value) -> Value {
    Value::Object(
        owners
            .iter()
            .map(|owner| (owner.to_string(), value.clone()))
            .collect(),
    )
}
/// Per column of `migration_property`: type name, physical NOT NULL and attribute number.
async fn columns(tx: &mut Transaction) -> std::collections::BTreeMap<String, (String, bool, i32)> {
    tx.query(
        "SELECT a.attname::text, t.typname::text, a.attnotnull, a.attnum::int4 FROM pg_attribute a JOIN pg_type t ON t.oid=a.atttypid WHERE a.attrelid='migration_property'::regclass AND a.attnum>0 AND NOT a.attisdropped",
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), (row.get(1), row.get(2), row.get(3))))
    .collect()
}
/// The number of foreign keys of `migration_property` on `column`.
async fn foreign_keys(tx: &mut Transaction, column: &str) -> i64 {
    tx.query(
        "SELECT count(*) FROM pg_constraint c JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=ANY(c.conkey) WHERE c.conrelid='migration_property'::regclass AND c.contype='f' AND a.attname=$1",
        &[&column],
    )
    .await
    .unwrap()[0]
        .get(0)
}
/// The definition of each index of `migration_property` named in `names`.
async fn index_definitions(
    tx: &mut Transaction,
    names: &[&str],
) -> std::collections::BTreeMap<String, String> {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    tx.query(
        "SELECT indexname::text, indexdef FROM pg_indexes WHERE schemaname=current_schema() AND tablename='migration_property' AND indexname=ANY($1)",
        &[&names],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), row.get(1)))
    .collect()
}
/// The comment of column `note` and of its index.
async fn comments(tx: &mut Transaction) -> (Option<String>, Option<String>) {
    let row = &tx
        .query(
            "SELECT col_description(a.attrelid, a.attnum), obj_description('migration_property__note_index'::regclass) FROM pg_attribute a WHERE a.attrelid='migration_property'::regclass AND a.attname='note'",
            &[],
        )
        .await
        .unwrap()[0];
    (row.get(0), row.get(1))
}
/// The text of a scalar column per row, in id order.
async fn scalar_text(tx: &mut Transaction, field: &str) -> Vec<Option<String>> {
    tx.query(
        &format!(
            "SELECT {}::text FROM migration_property ORDER BY id",
            erp_sql::quote(field)
        ),
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| row.get(0))
    .collect()
}

/// One predecessor row of the whole-case test.
struct Legacy {
    id: i64,
    note: Option<&'static str>,
    cost: Option<&'static str>,
    flag: Option<bool>,
    count: Option<i32>,
    link: Option<i64>,
    row_note: Option<&'static str>,
    company: Option<i64>,
    guarded: Option<&'static str>,
}

#[tokio::test]
async fn spec_compmig_001_explicit_scalar_ownership() {
    // A decimal no f64 holds: through an f64 it would come back as another number.
    const EXACT: &str = "9007199254740993.123456789";
    // A small negative fraction with more digits than the usual display scale.
    const TINY: &str = "-0.0000000000000000000000000001";
    assert_ne!(EXACT.parse::<f64>().unwrap().to_string(), EXACT);
    const NOTE_COMMENT: &str = "legacy note";
    const INDEX_COMMENT: &str = "legacy index";
    const NOTE_INDEX: &str = "migration_property__note_index";
    // The explicit company set of the When; company 3 exists and owns nothing.
    let owners = [1_i64, 2];
    let legacy = [
        Legacy {
            id: 1,
            note: Some("kept"),
            cost: Some(EXACT),
            flag: Some(true),
            count: Some(7),
            link: Some(2),
            row_note: Some("one"),
            company: Some(1),
            guarded: Some("first"),
        },
        // false, zero and empty text
        Legacy {
            id: 2,
            note: Some(""),
            cost: Some("0"),
            flag: Some(false),
            count: Some(0),
            link: Some(3),
            row_note: Some(""),
            company: Some(2),
            guarded: Some("second"),
        },
        // SQL NULL everywhere, and no row company
        Legacy {
            id: 3,
            note: None,
            cost: None,
            flag: None,
            count: None,
            link: None,
            row_note: None,
            company: None,
            guarded: None,
        },
        Legacy {
            id: 4,
            note: Some("other"),
            cost: Some(TINY),
            flag: Some(true),
            count: Some(-3),
            link: Some(1),
            row_note: Some("two"),
            company: Some(2),
            guarded: Some("fourth"),
        },
    ];
    let fields = || {
        vec![
            FieldDecl::char("note").index(Some(IndexKind::BtreeNotNull)),
            FieldDecl::float_digits("cost", Digits::Unrounded),
            FieldDecl::boolean("flag"),
            FieldDecl::integer("count"),
            FieldDecl::many2one("link", "res.company"),
            FieldDecl::char("row_note"),
            FieldDecl::many2one("company_id", "res.company"),
            FieldDecl::char("required_note").required(true),
            FieldDecl::char("guarded"),
        ]
    };
    // What becomes company-dependent, and with which ownership.
    let by_set = ["note", "cost", "flag", "count", "link", "required_note"];
    let h = predecessor(fields()).await;
    let mut tx = h.begin().await.unwrap();
    for row in &legacy {
        tx.execute(
            "INSERT INTO migration_property(id,note,cost,flag,count,link,row_note,company_id,required_note,guarded) VALUES ($1::int8,$2::varchar,$3::text::numeric,$4::bool,$5::int4,$6::int8,$7::varchar,$8::int8,$9::varchar,$10::varchar)",
            &[
                &row.id,
                &row.note,
                &row.cost,
                &row.flag,
                &row.count,
                &row.link,
                &row.row_note,
                &row.company,
                &format!("required {}", row.id),
                &row.guarded,
            ],
        )
        .await
        .unwrap();
    }
    tx.batch(&format!("COMMENT ON COLUMN migration_property.note IS '{NOTE_COMMENT}'; COMMENT ON INDEX {NOTE_INDEX} IS '{INDEX_COMMENT}'; CREATE INDEX unrelated_id ON migration_property(id)")).await.unwrap();
    let table_oid = async |tx: &mut Transaction| -> i64 {
        tx.query("SELECT 'migration_property'::regclass::oid::int8", &[])
            .await
            .unwrap()[0]
            .get(0)
    };
    // The genuine predecessor: scalar columns, a NOT NULL required column, the ORM's foreign keys.
    let oid = table_oid(&mut tx).await;
    let before = columns(&mut tx).await;
    for field in by_set.iter().chain(&["row_note", "guarded"]) {
        assert_ne!(before[*field].0, "jsonb", "{field}");
    }
    assert!(!before["note"].1);
    assert!(before["required_note"].1);
    assert_eq!(foreign_keys(&mut tx, "link").await, 1);
    assert_eq!(foreign_keys(&mut tx, "company_id").await, 1);
    let indexes_before = index_definitions(&mut tx, &[NOTE_INDEX, "unrelated_id"]).await;
    assert_eq!(indexes_before.len(), 2);
    tx.commit().await.unwrap();

    let mut target = fields();
    for field in &mut target {
        if field.name != "company_id" {
            *field = (*field).company_dependent(true);
        }
    }
    let orm = successor(&h, target);
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();

    // An unsupported dependency refuses, whatever its kind, and loses nothing: the scalar values
    // and the column type are what they were.
    let set_policy = CompanyMigrationPolicy::companies(owners).unwrap();
    let guarded_before = scalar_text(&mut tx, "guarded").await;
    assert_eq!(
        guarded_before,
        legacy
            .iter()
            .map(|row| row.guarded.map(str::to_owned))
            .collect::<Vec<_>>()
    );
    for (kind, name, create, remove) in [
        (
            "index",
            "custom_unique_guarded",
            "CREATE UNIQUE INDEX custom_unique_guarded ON migration_property(guarded)",
            "DROP INDEX custom_unique_guarded",
        ),
        (
            "constraint",
            "custom_check",
            "ALTER TABLE migration_property ADD CONSTRAINT custom_check CHECK(length(guarded)>0)",
            "ALTER TABLE migration_property DROP CONSTRAINT custom_check",
        ),
        (
            "view",
            "custom_guarded_view",
            "CREATE VIEW custom_guarded_view AS SELECT id, guarded FROM migration_property",
            "DROP VIEW custom_guarded_view",
        ),
    ] {
        tx.batch(create).await.unwrap();
        let err = tx
            .savepoint(async |tx| {
                tx.migrate_company_dependent("migration.property", "guarded", &set_policy)
                    .await
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Programming, "{kind}");
        assert_eq!(
            err.message(),
            format!(
                "migration.property.guarded: company migration refuses dependent {kind} '{name}'; migrate it explicitly"
            )
        );
        assert_eq!(scalar_text(&mut tx, "guarded").await, guarded_before);
        assert_eq!(columns(&mut tx).await, before, "{kind}");
        tx.batch(remove).await.unwrap();
    }

    // The migration: an explicit company set for six fields, the stored row company for one.
    for field in by_set.iter().chain(&["guarded"]) {
        tx.migrate_company_dependent("migration.property", field, &set_policy)
            .await
            .unwrap();
    }
    tx.migrate_company_dependent(
        "migration.property",
        "row_note",
        &CompanyMigrationPolicy::row_company("company_id").unwrap(),
    )
    .await
    .unwrap();

    // Each non-null value is an override of its declared owners only (the whole map is compared,
    // so no other company has an entry); NULL is SQL NULL; false, zero and '' are values.
    let by_owners = |value: Option<Value>| value.map(|value| owned(&owners, &value));
    let expected: Vec<(&str, Vec<Option<Value>>)> = vec![
        (
            "note",
            legacy
                .iter()
                .map(|r| by_owners(r.note.map(Value::from)))
                .collect(),
        ),
        // exact decimals are stored as their text
        (
            "cost",
            legacy
                .iter()
                .map(|r| by_owners(r.cost.map(Value::from)))
                .collect(),
        ),
        (
            "flag",
            legacy
                .iter()
                .map(|r| by_owners(r.flag.map(Value::from)))
                .collect(),
        ),
        (
            "count",
            legacy
                .iter()
                .map(|r| by_owners(r.count.map(Value::from)))
                .collect(),
        ),
        (
            "link",
            legacy
                .iter()
                .map(|r| by_owners(r.link.map(Value::from)))
                .collect(),
        ),
        (
            "required_note",
            legacy
                .iter()
                .map(|r| by_owners(Some(Value::from(format!("required {}", r.id)))))
                .collect(),
        ),
        (
            "guarded",
            legacy
                .iter()
                .map(|r| by_owners(r.guarded.map(Value::from)))
                .collect(),
        ),
        (
            "row_note",
            legacy
                .iter()
                .map(|r| {
                    r.row_note
                        .map(|value| owned(&[r.company.unwrap()], &Value::from(value)))
                })
                .collect(),
        ),
    ];
    for (field, maps) in &expected {
        assert_eq!(&map(&mut tx, field).await, maps, "{field}");
    }
    // The falsy values are entries, not absent: checked on the stored map, not on the expectation.
    let falsy = &legacy[1];
    for (field, value) in [
        ("note", json!(falsy.note.unwrap())),
        ("flag", json!(falsy.flag.unwrap())),
        ("count", json!(falsy.count.unwrap())),
        ("cost", json!(falsy.cost.unwrap())),
    ] {
        let stored = map(&mut tx, field).await[1].clone().unwrap();
        for owner in owners {
            assert_eq!(stored.get(owner.to_string()), Some(&value), "{field}");
        }
    }
    // Exact decimals: the stored entry is JSON text equal to the predecessor's digits, and
    // PostgreSQL reads it back as the same numeric.
    let exact = tx
        .query(
            "SELECT jsonb_typeof(cost->'1'), cost->>'1', (cost->>'2')::numeric = $1::text::numeric FROM migration_property WHERE id=1",
            &[&EXACT],
        )
        .await
        .unwrap();
    assert_eq!(exact[0].get::<_, String>(0), "string");
    assert_eq!(exact[0].get::<_, String>(1), EXACT);
    assert!(exact[0].get::<_, bool>(2));

    // Storage: the same table and columns, now jsonb and nullable (the required one too); the
    // comments and the supported index survive, the unrelated index and the row-company key too;
    // the ORM's scalar foreign key of the migrated many2one is gone.
    let catalog = async |tx: &mut Transaction| {
        assert_eq!(table_oid(tx).await, oid);
        let after = columns(tx).await;
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>()
        );
        for (name, (kind, not_null, number)) in &after {
            assert_eq!(*number, before[name].2, "{name}");
            if name == "id" || name == "company_id" {
                assert_eq!((kind, not_null), (&before[name].0, &before[name].1));
            } else if before[name].0 != "jsonb" && expected.iter().any(|(field, _)| field == name) {
                assert_eq!(kind, "jsonb", "{name}");
                assert!(!not_null, "{name}");
            }
        }
        assert_eq!(
            comments(tx).await,
            (
                Some(NOTE_COMMENT.to_owned()),
                Some(INDEX_COMMENT.to_owned())
            )
        );
        assert_eq!(foreign_keys(tx, "link").await, 0);
        assert_eq!(foreign_keys(tx, "company_id").await, 1);
    };
    catalog(&mut tx).await;
    assert_eq!(
        index_definitions(&mut tx, &[NOTE_INDEX, "unrelated_id"]).await,
        indexes_before
    );

    // Repeat: existing valid maps keep their owners, whatever policy the repeated run names, and
    // through schema synchronisation.
    let other = CompanyMigrationPolicy::companies([3]).unwrap();
    for round in 0..2 {
        for (field, _) in &expected {
            tx.migrate_company_dependent("migration.property", field, &other)
                .await
                .unwrap();
        }
        tx.init_models(&[]).await.unwrap();
        for (field, maps) in &expected {
            assert_eq!(&map(&mut tx, field).await, maps, "{field}, round {round}");
        }
        catalog(&mut tx).await;
        let indexes = index_definitions(&mut tx, &[NOTE_INDEX, "unrelated_id"]).await;
        assert_eq!(indexes.len(), 2);
        assert_eq!(indexes["unrelated_id"], indexes_before["unrelated_id"]);
        assert!(indexes[NOTE_INDEX].contains("WHERE (note IS NOT NULL)"));
    }
    tx.commit().await.unwrap();
    drop(tx);
    // And in a later transaction.
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.init_models(&[]).await.unwrap();
    for (field, maps) in &expected {
        assert_eq!(&map(&mut tx, field).await, maps, "{field}");
    }
    catalog(&mut tx).await;
    drop(tx);
    h.cleanup().await.unwrap();
}

/// Text that stands in every stored value of the test: no error may repeat it.
const HIDDEN: &str = "s3cret-marker";
/// A number stored in that test: no error may repeat it either.
const HIDDEN_NUMBER: i64 = 424_242;
/// Run `setup` and then the migration of `field` in a savepoint; the migration must refuse, the
/// savepoint undoes `setup`, and the error names no stored value.
async fn refused(
    tx: &mut Transaction,
    setup: &str,
    model: &str,
    field: &str,
    policy: &CompanyMigrationPolicy,
) -> erp_fields::Error {
    // A failing `setup` is a defect of the test, never the refusal under test.
    let mut prepared = false;
    let error = tx
        .savepoint(async |tx| {
            if !setup.is_empty() {
                tx.batch(setup).await?;
            }
            prepared = true;
            tx.migrate_company_dependent(model, field, policy).await
        })
        .await
        .unwrap_err();
    assert!(prepared, "setup failed: {setup}: {error:?}");
    assert!(!error.message().contains(HIDDEN), "{}", error.message());
    assert!(
        !error.message().contains(&HIDDEN_NUMBER.to_string()),
        "{}",
        error.message()
    );
    error
}
/// Rows of the test as text, with the row company.
async fn scalar_rows(
    tx: &mut Transaction,
) -> Vec<(i64, Option<String>, Option<String>, Option<i64>)> {
    tx.query(
        "SELECT id, note::text, cost::text, company_id::int8 FROM migration_property ORDER BY id",
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
    .collect()
}

#[tokio::test]
async fn spec_compmig_003_bounded_policy_and_stored_map_validation() {
    const SET: &str =
        "invalid company migration policy: owners must be unique positive IDs within MAX_OWNERS";
    const EMPTY: &str = "invalid company migration policy: at least one company is required";
    const ROW_FIELD: &str =
        "invalid company migration policy: row-company field must be a PostgreSQL field identifier";
    const STORAGE: &str = "migration.property.note: invalid company-dependent JSONB storage; an explicit migration is required";
    const INJECTION: &str = "x\"; DROP TABLE res_company;--";
    // the codec's bounds (`erp_orm::company_dependent`, not public): entries per map, bytes per map
    const MAP_ENTRIES: usize = 65_536;
    const MAP_BYTES: usize = 16 * 1024 * 1024;

    // The policy is refused at construction: empty, nonpositive, duplicate and oversized sets,
    // and a row-company name that is no identifier. The largest admitted set is accepted.
    let max = i64::try_from(CompanyMigrationPolicy::MAX_OWNERS).unwrap();
    let sets: Vec<(Vec<i64>, &str)> = vec![
        (vec![], EMPTY),
        (vec![0], SET),
        (vec![-1], SET),
        (vec![3, i64::MIN], SET),
        (vec![1, 1], SET),
        (vec![2, 5, 2], SET),
        ((1..=max + 1).collect(), SET),
    ];
    for (ids, message) in sets {
        let err = CompanyMigrationPolicy::companies(ids.clone()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Programming, "{} ids", ids.len());
        assert_eq!(err.message(), message, "{} ids", ids.len());
    }
    CompanyMigrationPolicy::companies(1..=max).unwrap();
    let long = "a".repeat(erp_registry::ident::MAX_IDENTIFIER_LEN + 1);
    for name in [INJECTION, "", "Company Id", long.as_str()] {
        let err = CompanyMigrationPolicy::row_company(name).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Programming, "{name}");
        assert_eq!(err.message(), ROW_FIELD, "{name}");
    }

    let fields = || {
        vec![
            FieldDecl::char("note"),
            FieldDecl::float_digits("cost", Digits::Unrounded),
            FieldDecl::text("body"),
            FieldDecl::many2one("company_id", "res.company"),
            FieldDecl::many2one("currency_id", "res.currency"),
        ]
    };
    let h = predecessor(fields()).await;
    let mut tx = h.begin().await.unwrap();
    let seeded = [
        (1_i64, format!("{HIDDEN} one"), "12.5", 1_i64),
        (2, format!("{HIDDEN} two"), "7", 2),
    ];
    for (id, note, cost, company) in &seeded {
        tx.execute(
            "INSERT INTO migration_property(id,note,cost,company_id) VALUES ($1::int8,$2::varchar,$3::text::numeric,$4::int8)",
            &[id, note, cost, company],
        )
        .await
        .unwrap();
    }
    // a column the registry does not declare
    tx.batch("ALTER TABLE migration_property ADD COLUMN stray varchar")
        .await
        .unwrap();
    let company_key: String = tx
        .query(
            "SELECT c.conname::text FROM pg_constraint c JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=ANY(c.conkey) WHERE c.conrelid='migration_property'::regclass AND c.contype='f' AND a.attname='company_id'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    tx.commit().await.unwrap();

    let mut target = fields();
    for field in &mut target {
        if !field.name.ends_with("_id") {
            *field = (*field).company_dependent(true);
        }
    }
    let orm = successor(&h, target);
    // The installer's transaction has a current company: 1. It must never become an owner.
    let installer = erp_orm::Context::from_pairs([(
        "allowed_company_ids",
        erp_fields::Value::list([erp_fields::Value::Int(1)]),
    )]);
    let mut tx = orm.begin_with(SUPERUSER_ID, installer).await.unwrap();
    assert_eq!(tx.env().company_id().unwrap(), Some(1));
    tx.capture_statements(true);
    let original = scalar_rows(&mut tx).await;
    assert_eq!(
        original,
        seeded
            .iter()
            .map(|(id, note, cost, company)| (
                *id,
                Some(note.clone()),
                Some((*cost).to_owned()),
                Some(*company)
            ))
            .collect::<Vec<_>>()
    );
    let types = columns(&mut tx).await;
    let companies_before: i64 = tx
        .query("SELECT count(*) FROM res_company", &[])
        .await
        .unwrap()[0]
        .get(0);
    let set = CompanyMigrationPolicy::companies([2]).unwrap();
    let by_row = CompanyMigrationPolicy::row_company("company_id").unwrap();
    let model = "migration.property";
    // Every refusal below: kind, whole message, and scalar data and column types unchanged.
    let mut refusals = 0;
    let mut check = async |tx: &mut Transaction,
                           what: &str,
                           error: erp_fields::Error,
                           kind: ErrorKind,
                           message: &str| {
        assert_eq!(error.kind(), kind, "{what}");
        assert_eq!(error.message(), message, "{what}");
        assert_eq!(scalar_rows(tx).await, original, "{what}");
        assert_eq!(columns(tx).await, types, "{what}");
        refusals += 1;
    };

    // Unknown companies.
    for ids in [vec![999_999], vec![1, 999_999]] {
        let unknown = CompanyMigrationPolicy::companies(ids).unwrap();
        let error = refused(&mut tx, "", model, "note", &unknown).await;
        check(
            &mut tx,
            "unknown company",
            error,
            ErrorKind::Validation,
            "migration.property.note: company migration policy names an unknown company",
        )
        .await;
    }

    // Invalid row owners: a field that is not declared, one that is no link to res.company, a
    // row without a company, and a row whose company does not exist.
    let undeclared = CompanyMigrationPolicy::row_company("stray").unwrap();
    let error = refused(&mut tx, "", model, "note", &undeclared).await;
    check(
        &mut tx,
        "undeclared row-company field",
        error,
        ErrorKind::Programming,
        "company migration row-company field does not exist",
    )
    .await;
    for name in ["currency_id", "cost"] {
        let wrong = CompanyMigrationPolicy::row_company(name).unwrap();
        let error = refused(&mut tx, "", model, "note", &wrong).await;
        check(
            &mut tx,
            name,
            error,
            ErrorKind::Programming,
            "company migration row-company field must be an ordinary stored Many2one to res.company",
        )
        .await;
    }
    let dangling = format!(
        "ALTER TABLE migration_property DROP CONSTRAINT {}; UPDATE migration_property SET company_id={HIDDEN_NUMBER} WHERE id=2",
        erp_sql::quote(&company_key)
    );
    for setup in [
        "UPDATE migration_property SET company_id=NULL WHERE id=2",
        dangling.as_str(),
    ] {
        let error = refused(&mut tx, setup, model, "note", &by_row).await;
        check(
            &mut tx,
            setup,
            error,
            ErrorKind::Validation,
            "migration.property.note: non-null predecessor value has no valid company owner",
        )
        .await;
    }

    // An incompatible source type: the message names the type found and the type expected.
    let declared = types["note"].0.clone();
    for (found, setup) in [
        (
            "text",
            "ALTER TABLE migration_property ALTER COLUMN note TYPE text",
        ),
        (
            "bytea",
            "ALTER TABLE migration_property ALTER COLUMN note TYPE bytea USING convert_to(note, 'UTF8')",
        ),
    ] {
        assert_ne!(found, declared);
        let error = refused(&mut tx, setup, model, "note", &set).await;
        check(
            &mut tx,
            found,
            error,
            ErrorKind::Programming,
            &format!(
                "migration.property.note: incompatible company migration predecessor type '{found}'; expected '{declared}'"
            ),
        )
        .await;
    }

    // Unsupported dependencies of each kind.
    for (kind, name, setup) in [
        (
            "index",
            "custom_unique_note",
            "CREATE UNIQUE INDEX custom_unique_note ON migration_property(note)",
        ),
        (
            "index",
            "custom_expression",
            "CREATE INDEX custom_expression ON migration_property(lower(note))",
        ),
        (
            "constraint",
            "custom_check",
            "ALTER TABLE migration_property ADD CONSTRAINT custom_check CHECK(length(note)>0)",
        ),
        (
            "view",
            "custom_note_view",
            "CREATE VIEW custom_note_view AS SELECT id, note FROM migration_property",
        ),
    ] {
        let error = refused(&mut tx, setup, model, "note", &set).await;
        check(
            &mut tx,
            name,
            error,
            ErrorKind::Programming,
            &format!(
                "migration.property.note: company migration refuses dependent {kind} '{name}'; migrate it explicitly"
            ),
        )
        .await;
    }

    // Scalar values are validated with the codec: a decimal the codec refuses, and a text
    // larger than a company map may be.
    let error = refused(
        &mut tx,
        "UPDATE migration_property SET cost='NaN' WHERE id=2",
        model,
        "cost",
        &set,
    )
    .await;
    check(
        &mut tx,
        "NaN",
        error,
        ErrorKind::Validation,
        "migration.property.cost: invalid predecessor value",
    )
    .await;
    let oversized = format!(
        "UPDATE migration_property SET body='{HIDDEN}' || repeat('x', {MAP_BYTES}) WHERE id=2"
    );
    let error = refused(&mut tx, &oversized, model, "body", &set).await;
    check(
        &mut tx,
        "oversized text",
        error,
        ErrorKind::Validation,
        "migration.property.body: invalid predecessor value or company-map byte limit exceeded",
    )
    .await;

    // Existing maps: every entry of every row is validated. The current company is 1; an
    // invalid entry of company 3, next to a valid entry of company 1, refuses all the same, and
    // so does an invalid map in one row only.
    let convert = |map: &str, filter: &str| {
        format!(
            "ALTER TABLE migration_property ALTER COLUMN note TYPE jsonb USING jsonb_build_object('1', 'valid'); UPDATE migration_property SET note={map} WHERE {filter}"
        )
    };
    let hidden = format!("'{HIDDEN}'::text");
    let maps: Vec<(&str, String)> = vec![
        (
            "another company's entry is an object",
            format!("jsonb_build_object('1', 'valid', '3', jsonb_build_object({hidden}, true))"),
        ),
        (
            "another company's entry is a number",
            format!("jsonb_build_object('1', 'valid', '3', {HIDDEN_NUMBER})"),
        ),
        (
            "another company's entry is an array",
            format!("jsonb_build_object('1', 'valid', '2', jsonb_build_array({hidden}))"),
        ),
        (
            "a key that is no company id",
            format!("jsonb_build_object('1', 'valid', '0', {hidden})"),
        ),
        (
            "a key with a leading zero",
            format!("jsonb_build_object('1', 'valid', '03', {hidden})"),
        ),
        ("not a map", format!("to_jsonb({hidden})")),
        (
            "more entries than a map may hold",
            format!(
                "(SELECT jsonb_object_agg(i::text, 'x') FROM generate_series(1, {}) i)",
                MAP_ENTRIES + 1
            ),
        ),
        (
            "an entry larger than a map may be",
            format!("jsonb_build_object('1', 'valid', '3', repeat('x', {MAP_BYTES}))"),
        ),
    ];
    for (what, map) in &maps {
        for filter in ["true", "id=2"] {
            let error = refused(&mut tx, &convert(map, filter), model, "note", &set).await;
            check(&mut tx, what, error, ErrorKind::Validation, STORAGE).await;
        }
    }
    // The same conversion with valid entries of companies 1 and 3 is admitted (in a savepoint
    // that is undone), so the refusals above are about the entries.
    let admitted = tx
        .savepoint(async |tx| {
            tx.batch(&convert(
                "jsonb_build_object('1', 'valid', '3', 'other')",
                "id=2",
            ))
            .await?;
            tx.migrate_company_dependent(model, "note", &set).await?;
            let maps = map(tx, "note").await;
            tx.batch("SELECT 1/0").await?;
            Ok(maps)
        })
        .await;
    // (the division by zero only undoes the savepoint; the maps were read before it)
    assert_eq!(admitted.unwrap_err().sqlstate(), Some("22012"));
    assert_eq!(scalar_rows(&mut tx).await, original);

    // SQL identifiers come from the registry: a model or field the registry does not declare
    // is refused by name lookup, also a column that exists in the table, and no text of such a
    // name reaches a statement.
    for (target_model, field, message) in [
        (
            INJECTION,
            "note",
            "company migration target model does not exist",
        ),
        (
            "migration_property",
            "note",
            "company migration target model does not exist",
        ),
        (
            model,
            INJECTION,
            "company migration target field does not exist",
        ),
        (
            model,
            "stray",
            "company migration target field does not exist",
        ),
    ] {
        let error = refused(&mut tx, "", target_model, field, &set).await;
        check(&mut tx, field, error, ErrorKind::Programming, message).await;
    }
    assert!(
        tx.captured()
            .iter()
            .all(|sql| !sql.contains("DROP TABLE") && !sql.contains("stray"))
    );
    let companies_after: i64 = tx
        .query("SELECT count(*) FROM res_company", &[])
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(companies_after, companies_before);
    // 2 unknown, 3 row fields, 2 row owners, 2 types, 4 dependencies, 2 values, the maps twice,
    // 4 identifiers: every refusal above was checked.
    assert_eq!(refusals, 2 + 3 + 2 + 2 + 4 + 2 + 2 * maps.len() + 4);

    // The installer context never supplies ownership: without a policy the populated
    // conversion refuses instead of taking the current company, and with a policy the owners
    // are the policy's (company 2, the row company), never the current company 1.
    let error = tx
        .savepoint(async |tx| tx.init_models(&[]).await)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming);
    let fields_without_policy = ["note", "cost"];
    assert!(
        fields_without_policy.iter().any(|field| error.message()
            == format!(
                "migration.property.{field}: populated scalar conversion requires an explicit company migration policy"
            )),
        "{}",
        error.message()
    );
    assert_eq!(scalar_rows(&mut tx).await, original);
    assert_eq!(columns(&mut tx).await, types);
    tx.migrate_company_dependent(model, "note", &set)
        .await
        .unwrap();
    tx.migrate_company_dependent(model, "cost", &by_row)
        .await
        .unwrap();
    tx.migrate_company_dependent(model, "body", &set)
        .await
        .unwrap();
    tx.init_models(&[]).await.unwrap();
    assert_eq!(tx.env().company_id().unwrap(), Some(1));
    assert_eq!(
        map(&mut tx, "note").await,
        seeded
            .iter()
            .map(|(_, note, _, _)| Some(owned(&[2], &Value::from(note.as_str()))))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        map(&mut tx, "cost").await,
        seeded
            .iter()
            .map(|(_, _, cost, company)| Some(owned(&[*company], &Value::from(*cost))))
            .collect::<Vec<_>>()
    );
    assert_eq!(map(&mut tx, "body").await, vec![None, None]);
    drop(tx);
    h.cleanup().await.unwrap();
}

/// An ORM runtime built from nothing shared: its own registry and its own connection pool.
fn independent_runtime(h: &Harness, fields: Vec<FieldDecl>) -> (Arc<erp_registry::Registry>, Orm) {
    let target = Box::leak(Box::new(decl(fields)));
    let registry = Arc::new(
        RegistryBuilder::new([&base_users::ADDON, &erp_addon_base::ADDON, &ADDON])
            .with_decls([&*target])
            .build()
            .unwrap(),
    );
    let pool = erp_orm::pool::create_pool(h.db().config(), 2).unwrap();
    let orm = Orm::builder(Arc::clone(&registry), pool)
        .policy(StaticPolicy::AllowAll)
        .build()
        .unwrap();
    (registry, orm)
}
async fn backend_pid(tx: &mut Transaction) -> i32 {
    tx.query("SELECT pg_backend_pid()", &[]).await.unwrap()[0].get(0)
}
/// Whether backend `pid` holds the schema advisory lock, as PostgreSQL reports it.
async fn holds_schema_lock(tx: &mut Transaction, pid: i32) -> bool {
    tx.query(
        "SELECT EXISTS(SELECT FROM pg_locks WHERE locktype='advisory' AND granted AND pid=$1 AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))",
        &[&pid],
    )
    .await
    .unwrap()[0]
        .get(0)
}
/// The catalog type of column `note` (a catalog read: it does not wait for the table lock).
async fn note_type(tx: &mut Transaction) -> String {
    tx.query(
        "SELECT t.typname::text FROM pg_attribute a JOIN pg_type t ON t.oid=a.atttypid WHERE a.attrelid=to_regclass('migration_property') AND a.attname='note'",
        &[],
    )
    .await
    .unwrap()[0]
        .get(0)
}

#[tokio::test]
async fn spec_compmig_004_schema_locking_across_independent_runtimes() {
    // The second runtime's own bound, far below the schema step's default.
    const LOCK_TIMEOUT_MS: u64 = 200;
    let bound = std::time::Duration::from_millis(LOCK_TIMEOUT_MS);
    assert!(bound < erp_orm::SCHEMA_LOCK_TIMEOUT_DURATION);
    let kept = ["first", "second"];
    let h = predecessor(vec![FieldDecl::char("note"), FieldDecl::char("kept")]).await;
    let mut tx = h.begin().await.unwrap();
    for (id, value) in (1_i64..).zip(kept) {
        tx.execute(
            "INSERT INTO migration_property(id,kept) VALUES ($1::int8,$2::varchar)",
            &[&id, &value],
        )
        .await
        .unwrap();
    }
    let predecessor_type = note_type(&mut tx).await;
    assert_ne!(predecessor_type, "jsonb");
    tx.commit().await.unwrap();
    let rows = async |tx: &mut Transaction| -> Vec<(i64, Option<String>, Option<String>)> {
        tx.query(
            "SELECT id, note::text, kept::text FROM migration_property ORDER BY id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
    };
    let expected_rows: Vec<(i64, Option<String>, Option<String>)> = (1_i64..)
        .zip(kept)
        .map(|(id, value)| (id, None, Some(value.to_owned())))
        .collect();

    // Two runtimes with nothing in common but the database: separate registries, separate
    // pools, separate server sessions. Whatever serialises them is in PostgreSQL.
    let target = || vec![property(FieldDecl::char("note")), FieldDecl::char("kept")];
    let (first_registry, first_orm) = independent_runtime(&h, target());
    let (second_registry, second_orm) = independent_runtime(&h, target());
    assert!(!Arc::ptr_eq(&first_registry, &second_registry));
    let mut first = first_orm.begin(SUPERUSER_ID).await.unwrap();
    let mut second = second_orm.begin(SUPERUSER_ID).await.unwrap();
    let first_pid = backend_pid(&mut first).await;
    let second_pid = backend_pid(&mut second).await;
    assert_ne!(first_pid, second_pid);
    assert!(!holds_schema_lock(&mut second, first_pid).await);

    // The first holds a company migration open.
    first
        .migrate_company_dependent(
            "migration.property",
            "note",
            &CompanyMigrationPolicy::companies([1]).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(note_type(&mut first).await, "jsonb");
    // The second sees, from the server, that the other session holds the schema lock, and the
    // uncommitted change is not its catalog.
    assert!(holds_schema_lock(&mut second, first_pid).await);
    assert!(!holds_schema_lock(&mut second, second_pid).await);
    assert_eq!(note_type(&mut second).await, predecessor_type);

    // Its schema synchronisation obeys its own bounded lock timeout: Lock, SQLSTATE 55P03,
    // after at least the bound and well before the default bound of a schema step.
    second
        .batch(&format!("SET LOCAL lock_timeout='{LOCK_TIMEOUT_MS}ms'"))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let err = second.init_models(&[]).await.unwrap_err();
    let waited = started.elapsed();
    assert_eq!(err.kind(), ErrorKind::Lock);
    assert_eq!(err.sqlstate(), Some("55P03"));
    assert_eq!(
        err.message(),
        format!(
            "another schema synchronization of this database is running; gave up waiting for it after {LOCK_TIMEOUT_MS}ms (lock_timeout), try again when it has finished"
        )
    );
    assert!(waited >= bound, "{waited:?}");
    assert!(waited < erp_orm::SCHEMA_LOCK_TIMEOUT_DURATION, "{waited:?}");
    second.rollback().await.unwrap();
    drop(second);

    // The first rolls back: the lock is gone with its transaction and the predecessor is
    // retained, schema and rows.
    first.rollback().await.unwrap();
    drop(first);
    let mut second = second_orm.begin(SUPERUSER_ID).await.unwrap();
    assert!(!holds_schema_lock(&mut second, first_pid).await);
    assert_eq!(note_type(&mut second).await, predecessor_type);
    assert_eq!(rows(&mut second).await, expected_rows);
    // Synchronisation by the second runtime now succeeds against that predecessor (all its
    // values are NULL, so no ownership is needed) and commits.
    second.init_models(&[]).await.unwrap();
    assert_eq!(note_type(&mut second).await, "jsonb");
    assert_eq!(rows(&mut second).await, expected_rows);
    second.commit().await.unwrap();
    drop(second);
    // The first runtime finds the committed result and has nothing left to do.
    let mut first = first_orm.begin(SUPERUSER_ID).await.unwrap();
    assert_eq!(note_type(&mut first).await, "jsonb");
    first.init_models(&[]).await.unwrap();
    assert_eq!(rows(&mut first).await, expected_rows);
    first.rollback().await.unwrap();
    drop(first);
    drop(first_orm);
    drop(second_orm);
    h.cleanup().await.unwrap();
}
