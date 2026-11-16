// SPDX-License-Identifier: MIT OR Apache-2.0
//! Existing JSONB is admitted only when every value is valid company-map storage.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl, RegistryBuilder};
use erp_sdk::{Digits, ErrorKind, Orm, SUPERUSER_ID, StaticPolicy, Transaction};
use erp_test_models::base_users;
use erp_testkit::Harness;
use serde_json::{Value, json};
use std::sync::Arc;

static ADDON: AddonDescriptor = AddonDescriptor {
    name: "property_schema",
    depends: &["base"],
    exact_only: false,
};
fn declaration(field: FieldDecl) -> ModelDecl {
    ModelDecl::new("property_schema", "property.schema")
        .table("property_schema")
        .fields(Box::leak(vec![field].into_boxed_slice()))
}
async fn predecessor() -> Harness {
    Harness::builder([&base_users::ADDON, &ADDON])
        .with_decls([declaration(FieldDecl::json("value"))])
        .build()
        .await
        .unwrap()
}
fn successor(h: &Harness, field: FieldDecl) -> Orm {
    let target = ModelDecl::new("property_schema", "property.schema")
        .table("property_schema")
        .fields(Box::leak(
            vec![field.company_dependent(true), FieldDecl::char("added")].into_boxed_slice(),
        ));
    let registry = Arc::new(
        RegistryBuilder::new([&base_users::ADDON, &ADDON])
            .with_decls([&*Box::leak(Box::new(target))])
            .build()
            .unwrap(),
    );
    Orm::builder(registry, h.db().pool().clone())
        .policy(StaticPolicy::AllowAll)
        .build()
        .unwrap()
}
async fn snapshot(tx: &mut Transaction) -> Vec<(i64, Option<Value>)> {
    tx.query("SELECT id, value FROM property_schema ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

#[tokio::test]
async fn existing_jsonb_adoption_checks_every_row_and_refuses_invalid_maps() {
    let h = predecessor().await;
    let mut tx = h.begin().await.unwrap();
    tx.execute(
        "INSERT INTO property_schema (id, value) VALUES (1, '{\"1\":\"valid\"}'), (2, NULL)",
        &[],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let orm = successor(&h, FieldDecl::char("value"));
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    for bad in [
        json!("scalar"),
        Value::Null,
        json!([]),
        json!({"en_US":"translation"}),
        json!({"01":"bad"}),
        json!({"0":"bad"}),
        json!({"-1":"bad"}),
        json!({"1": {"nested":true}}),
        json!({"1":42}),
    ] {
        tx.execute("UPDATE property_schema SET value=$1 WHERE id=2", &[&bad])
            .await
            .unwrap();
        let before = snapshot(&mut tx).await;
        let result = tx.savepoint(async |tx| tx.init_models(&[]).await).await;
        let error = result.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(
            error.message(),
            "property.schema.value: invalid company-dependent JSONB storage; an explicit migration is required"
        );
        assert_eq!(snapshot(&mut tx).await, before);
        let kind = tx.query("SELECT data_type FROM information_schema.columns WHERE table_name='property_schema' AND column_name='value'", &[]).await.unwrap();
        assert_eq!(kind[0].get::<_, String>(0), "jsonb");
        let added = tx.query("SELECT count(*) FROM information_schema.columns WHERE table_name='property_schema' AND column_name='added'", &[]).await.unwrap();
        assert_eq!(added[0].get::<_, i64>(0), 0);
    }
    tx.execute("UPDATE property_schema SET value=NULL WHERE id=2", &[])
        .await
        .unwrap();
    // Force more than one keyset window and put the incompatible row last.
    tx.execute("INSERT INTO property_schema (id, value) SELECT i, '{\"1\":\"valid\"}'::jsonb FROM generate_series(3, 131) i", &[]).await.unwrap();
    tx.execute("UPDATE property_schema SET value='[]' WHERE id=131", &[])
        .await
        .unwrap();
    let error = tx
        .savepoint(async |tx| tx.init_models(&[]).await)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "property.schema.value: invalid company-dependent JSONB storage; an explicit migration is required"
    );
    tx.execute("DELETE FROM property_schema WHERE id=131", &[])
        .await
        .unwrap();
    tx.init_models(&[]).await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn existing_company_maps_and_exact_decimals_survive_repeat_initialization() {
    for field in [
        FieldDecl::char("value"),
        FieldDecl::float_digits(
            "value",
            Digits::Fixed {
                precision: 0,
                scale: 3,
            },
        ),
    ] {
        let h = predecessor().await;
        let mut tx = h.begin().await.unwrap();
        let map = json!({"1":"12345678901234567890.125", "2":null});
        tx.execute(
            "INSERT INTO property_schema (id, value) VALUES (1, $1), (2, NULL), (3, '{}')",
            &[&map],
        )
        .await
        .unwrap();
        let before = snapshot(&mut tx).await;
        tx.commit().await.unwrap();
        let orm = successor(&h, field);
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        tx.init_models(&[]).await.unwrap();
        tx.init_models(&[]).await.unwrap();
        assert_eq!(snapshot(&mut tx).await, before);
        drop(tx);
        h.cleanup().await.unwrap();
    }
}

#[tokio::test]
async fn existing_jsonb_adoption_enforces_map_and_transfer_bounds() {
    let h = predecessor().await;
    let mut tx = h.begin().await.unwrap();
    tx.execute("INSERT INTO property_schema (id) VALUES (1)", &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let orm = successor(&h, FieldDecl::char("value"));
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.capture_statements(true);
    for sql in [
        "UPDATE property_schema SET value=(SELECT jsonb_object_agg(i::text, 'x') FROM generate_series(1, 65537) i)",
        "UPDATE property_schema SET value=jsonb_build_object('1', repeat('x', 16777216))",
        "UPDATE property_schema SET value=to_jsonb(repeat('x', 110000000))",
    ] {
        tx.execute(sql, &[]).await.unwrap();
        let error = tx
            .savepoint(async |tx| tx.init_models(&[]).await)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(
            error.message(),
            "property.schema.value: invalid company-dependent JSONB storage; an explicit migration is required"
        );
    }
    let validation = tx
        .captured()
        .iter()
        .find(|sql| sql.contains("WITH candidates AS MATERIALIZED"))
        .unwrap();
    assert!(validation.contains("LIMIT 64"));
    assert!(validation.contains("CASE WHEN s.bytes <= 100663296"));
    assert!(validation.contains("s.total <= 100663296 + 32"));
    drop(tx);
    h.cleanup().await.unwrap();
}

const REFUSAL: &str = "property.schema.value: invalid company-dependent JSONB storage; an explicit migration is required";
/// Every row as id, whether it is SQL NULL, and a digest of its text (some rows are megabytes).
async fn digest(tx: &mut Transaction) -> Vec<(i64, bool, Option<String>)> {
    tx.query(
        "SELECT id, value IS NULL, md5(value::text) FROM property_schema ORDER BY id",
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), row.get(1), row.get(2)))
    .collect()
}
/// The type of column `value` and whether the successor's new column `added` exists.
async fn schema(tx: &mut Transaction) -> (String, bool) {
    let kind = tx.query("SELECT data_type::text FROM information_schema.columns WHERE table_schema = current_schema() AND table_name='property_schema' AND column_name='value'", &[]).await.unwrap();
    let added = tx.query("SELECT count(*) FROM information_schema.columns WHERE table_schema = current_schema() AND table_name='property_schema' AND column_name='added'", &[]).await.unwrap();
    (kind[0].get(0), added[0].get::<_, i64>(0) > 0)
}

#[tokio::test]
async fn spec_field_107_existing_jsonb_company_map_admission() {
    // A marker that stands in every refused value: no message may repeat it.
    const MARKER: &str = "s3cret-marker";
    // The most bytes of one stored value the validation reads: six times the 16 MiB map limit.
    const WIRE_LIMIT: usize = 16 * 1024 * 1024 * 6;
    let decimal = || {
        FieldDecl::float_digits(
            "value",
            Digits::Fixed {
                precision: 0,
                scale: 3,
            },
        )
    };
    let valid_rows = [
        (
            1_i64,
            Some(json!({"1": "12345678901234567890.125", "2": null})),
        ),
        (2, None),
        (3, Some(json!({}))),
        (4, Some(json!({"7": null}))),
        (5, Some(json!({"3": "0.500", "9007199254740993": "-1.125"}))),
    ];
    for (name, field) in [("char", FieldDecl::char("value")), ("decimal", decimal())] {
        let h = predecessor().await;
        let mut tx = h.begin().await.unwrap();
        for (id, value) in &valid_rows {
            tx.execute(
                "INSERT INTO property_schema (id, value) VALUES ($1, $2)",
                &[id, value],
            )
            .await
            .unwrap();
        }
        let original = snapshot(&mut tx).await;
        assert_eq!(original, valid_rows.to_vec());
        assert_eq!(schema(&mut tx).await, ("jsonb".to_owned(), false));
        tx.commit().await.unwrap();
        let orm = successor(&h, field);

        // Each refusal class, on one more row: kind, message, no value in the message, and
        // neither data nor schema changed (the refusal comes before any schema mutation).
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        tx.execute("INSERT INTO property_schema (id) VALUES (6)", &[])
            .await
            .unwrap();
        let marker = format!("'{MARKER}'");
        let mut invalid: Vec<(&str, String)> = vec![
            ("shape: a JSON scalar", format!("to_jsonb({marker}::text)")),
            ("shape: JSON null", "'null'::jsonb".to_owned()),
            (
                "shape: an array",
                format!("jsonb_build_array({marker}::text)"),
            ),
            (
                "key: not a company id",
                format!("jsonb_build_object('en_US', {marker}::text)"),
            ),
            (
                "key: leading zero",
                format!("jsonb_build_object('01', {marker}::text)"),
            ),
            (
                "key: zero",
                format!("jsonb_build_object('0', {marker}::text)"),
            ),
            (
                "key: negative",
                format!("jsonb_build_object('-1', {marker}::text)"),
            ),
            (
                "type: a nested object",
                format!("jsonb_build_object('1', jsonb_build_object({marker}::text, true))"),
            ),
            (
                "type: an array value",
                format!("jsonb_build_object('1', jsonb_build_array({marker}::text))"),
            ),
            (
                "limit: more entries than a map may hold",
                "(SELECT jsonb_object_agg(i::text, '1.5') FROM generate_series(1, 65537) i)"
                    .to_owned(),
            ),
            (
                "limit: an entry larger than a value may be",
                "jsonb_build_object('1', repeat('7', 16777216))".to_owned(),
            ),
            // Larger than the validation transfers at all (six times the map byte limit, see
            // `existing_jsonb_adoption_enforces_map_and_transfer_bounds`): masked in SQL.
            (
                "limit: a stored value larger than the wire bound",
                format!("jsonb_build_object('1', repeat('7', {WIRE_LIMIT}))"),
            ),
        ];
        // the field's codec: a Char takes no number, an exact decimal no free text
        invalid.push(if name == "char" {
            (
                "type: a number in a Char map",
                "jsonb_build_object('1', 424242)".to_owned(),
            )
        } else {
            (
                "type: text that is no decimal",
                format!("jsonb_build_object('1', {marker}::text)"),
            )
        });
        for (class, expression) in &invalid {
            let sql = format!("UPDATE property_schema SET value = {expression} WHERE id = 6");
            tx.execute(&sql, &[]).await.unwrap();
            let before = digest(&mut tx).await;
            // twice: a repeated initialisation refuses the same way
            for _ in 0..2 {
                let error = tx
                    .savepoint(async |tx| tx.init_models(&[]).await)
                    .await
                    .unwrap_err();
                assert_eq!(error.kind(), ErrorKind::Validation, "{name}, {class}");
                assert_eq!(error.message(), REFUSAL, "{name}, {class}");
                assert!(!error.message().contains(MARKER));
                assert!(!error.message().contains("424242"));
                assert_eq!(digest(&mut tx).await, before, "{name}, {class}");
                assert_eq!(
                    schema(&mut tx).await,
                    ("jsonb".to_owned(), false),
                    "{name}, {class}"
                );
            }
        }
        // Rollback of the refused initialisation keeps the old data and the old schema.
        let error = tx.init_models(&[]).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(error.message(), REFUSAL);
        tx.rollback().await.unwrap();
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        assert_eq!(snapshot(&mut tx).await, original, "{name}");
        assert_eq!(schema(&mut tx).await, ("jsonb".to_owned(), false), "{name}");

        // Valid storage is admitted as it is, also when the initialisation is repeated: SQL
        // NULL, the empty map, explicit null entries and exact decimal strings are unchanged.
        tx.init_models(&[]).await.unwrap();
        assert_eq!(snapshot(&mut tx).await, original, "{name}");
        assert_eq!(schema(&mut tx).await, ("jsonb".to_owned(), true), "{name}");
        tx.init_models(&[]).await.unwrap();
        assert_eq!(snapshot(&mut tx).await, original, "{name}");
        tx.commit().await.unwrap();
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        tx.init_models(&[]).await.unwrap();
        assert_eq!(snapshot(&mut tx).await, original, "{name}");
        assert_eq!(schema(&mut tx).await, ("jsonb".to_owned(), true), "{name}");
        drop(tx);
        h.cleanup().await.unwrap();
    }
}
