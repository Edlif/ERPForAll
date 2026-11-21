// SPDX-License-Identifier: MIT OR Apache-2.0
//! Registry ordering, generic numeric failure and dependency boundaries.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod common;
use common::*;
use erp_addon_stock_account::valuation_01_source::ValuationReceipt;

const MODELS: [&str; 10] = [
    "stock.valuation.source",
    "stock.valuation.source.alloc",
    "stock.valuation.receipt",
    "stock.valuation.journal",
    "stock.valuation.remainder",
    "stock.valuation.state",
    "stock.valuation.control",
    "stock.valuation.checkpoint",
    "stock.valuation.layer",
    "stock.valuation.layer.consumption",
];

#[tokio::test]
async fn valuation_models_are_registered_target_first_and_start_empty() {
    let h = harness().await;
    let actual: Vec<&str> = h
        .registry()
        .models()
        .iter()
        .filter(|m| m.name.starts_with("stock.valuation."))
        .map(|m| m.name)
        .collect();
    assert_eq!(actual, MODELS);
    let mut tx = h.begin().await.unwrap();
    for name in MODELS {
        let model = h.registry().model(name).unwrap();
        assert_eq!(
            tx.query(&format!("SELECT count(*)::int8 FROM {}", model.table), &[])
                .await
                .unwrap()[0]
                .get::<_, i64>(0),
            0
        );
        assert!(model.field("company_id").unwrap().required);
        let expected = column_contract(model.table.as_str());
        let actual:Vec<String>=tx.query("SELECT column_name::text FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY column_name",&[&model.table]).await.unwrap().iter().map(|r|r.get(0)).collect();
        let mut expected: Vec<&str> = expected.split_whitespace().collect();
        expected.sort_unstable();
        assert_eq!(actual, expected, "exact columns of {}", model.table);
        let bad=tx.query("SELECT conname::text FROM pg_constraint WHERE conrelid=$1::text::regclass AND contype='f' AND confdeltype <> 'r' AND conname <> $1 || '_create_uid_fkey' AND conname <> $1 || '_write_uid_fkey'",&[&model.table]).await.unwrap();
        assert!(
            bad.is_empty(),
            "every business valuation FK restricts deletion: {}",
            model.table
        );
    }
    assert!(erp_addon_stock_account::ADDON.depends.contains(&"account"));
    assert!(!erp_addon_stock::ADDON.depends.contains(&"account"));
    tx.rollback().await.unwrap();
    assert!(
        h.orm()
            .sync_schema()
            .await
            .unwrap()
            .kept_comments()
            .is_empty()
    );
}

#[tokio::test]
async fn generic_decimal_read_refuses_large_grid_value() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let id=insert(&mut tx,"INSERT INTO stk_cost_state(company_id,product_id,method,policy,grid_value,quantity,currency_carry,currency_id,currency_increment,generation) VALUES(101,101,'average','cost_scale_v1',1e40,1,0,(SELECT id FROM res_currency LIMIT 1),0.01,0) RETURNING id").await.unwrap();
    let rec = tx
        .env()
        .records("stock.valuation.state")
        .unwrap()
        .browse([id])
        .unwrap();
    let error = rec.cell(&mut tx, "grid_value").await.unwrap_err();
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Database);
    assert_eq!(error.message(), "could not decode a column value");
    assert!(std::error::Error::source(&error).is_some());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn typed_receipt_distinguishes_pending_from_generation_zero() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let sid = source(&mut tx, "receipt null").await;
    let row = insert(
        &mut tx,
        &format!(
            "INSERT INTO stk_value_receipt(source_id,company_id,product_id,state,recorded_at)
                  VALUES({sid},101,101,'pending','2026-01-01') RETURNING id"
        ),
    )
    .await
    .unwrap();
    let receipt = tx
        .env()
        .model::<ValuationReceipt>()
        .unwrap()
        .browse([row])
        .unwrap();
    let pending = receipt
        .get(&mut tx, ValuationReceipt::APPLIED_GENERATION)
        .await
        .unwrap();
    assert_eq!(pending, None);

    let sid = source(&mut tx, "receipt zero").await;
    let row = insert(
        &mut tx,
        &format!("INSERT INTO stk_value_receipt(source_id,company_id,product_id,state,recorded_at,applied_generation,applied_mode,applied_at)
                  VALUES({sid},101,101,'applied','2026-01-01',0,'inline','2026-01-01') RETURNING id"),
    )
    .await
    .unwrap();
    let receipt = tx
        .env()
        .model::<ValuationReceipt>()
        .unwrap()
        .browse([row])
        .unwrap();
    let applied = receipt
        .get(&mut tx, ValuationReceipt::APPLIED_GENERATION)
        .await
        .unwrap();
    assert_eq!(applied, Some(0));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn state_cursor_refuses_cross_company_and_cross_product_sources() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let sid = source(&mut tx, "cursor fixture").await;
    let state = |company, product| {
        format!(
        "INSERT INTO stk_cost_state(company_id,product_id,method,policy,grid_value,quantity,currency_carry,currency_id,currency_increment,generation,valued_through_time,valued_through_source_id)
         VALUES({company},{product},'average','cost_scale_v1',1,1,0,(SELECT id FROM res_currency LIMIT 1),0.01,0,'2026-01-01',{sid}) RETURNING id"
    )
    };
    insert(&mut tx, &state(101, 101)).await.unwrap();
    for (company, product) in [(102, 101), (101, 102)] {
        let error = insert(&mut tx, &state(company, product)).await.unwrap_err();
        assert_eq!(error.sqlstate(), Some("23503"));
        assert_eq!(
            error.constraint(),
            Some("stk_cost_state_valued_through_source_company_product")
        );
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn valuation_restrict_keys_have_leading_indexes() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    for (table, fields) in [
        ("stk_value_source", &["product_id"][..]),
        ("stk_value_source_alloc", &["product_id", "lot_id"]),
        ("stk_value_receipt", &["product_id"]),
        (
            "stk_value_journal",
            &["product_id", "lot_id", "currency_id", "category_id"],
        ),
        ("stk_cost_state", &["product_id", "lot_id", "currency_id"]),
        ("stk_cost_control", &["product_id"]),
        (
            "stk_cost_checkpoint",
            &["product_id", "lot_id", "currency_id"],
        ),
        ("stk_cost_layer", &["product_id", "lot_id"]),
        ("stk_layer_consumption", &["product_id"]),
    ] {
        for field in fields {
            let rows = tx
                .query(
                    "SELECT EXISTS (
                    SELECT 1 FROM pg_index i
                    JOIN pg_attribute a ON a.attrelid=i.indrelid AND a.attnum=i.indkey[0]
                    JOIN pg_class c ON c.oid=i.indexrelid
                    JOIN pg_am m ON m.oid=c.relam
                    WHERE i.indrelid=$1::text::regclass AND a.attname=$2::text
                      AND i.indisvalid AND i.indpred IS NULL AND m.amname='btree'
                )",
                    &[&table, field],
                )
                .await
                .unwrap();
            assert!(rows[0].get::<_, bool>(0), "{table}.{field}");
        }
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn nullable_valuation_lineage_keys_are_scoped_and_validated() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    for (table, pointer, target) in [
        (
            "stk_value_source",
            "affected_from_source_id",
            "stk_value_source",
        ),
        ("stk_value_journal", "cause_journal_id", "stk_value_journal"),
        ("stk_value_journal", "cause_source_id", "stk_value_source"),
        (
            "stk_cost_state",
            "valued_through_source_id",
            "stk_value_source",
        ),
        (
            "stk_cost_state",
            "covered_through_source_id",
            "stk_value_source",
        ),
        ("stk_cost_control", "drained_source_id", "stk_value_source"),
        ("stk_cost_layer", "shortage_source_id", "stk_value_source"),
    ] {
        let rows = tx
            .query(
                "SELECT pg_get_constraintdef(oid) FROM pg_constraint
             WHERE conrelid=$1::text::regclass AND confrelid=$2::text::regclass
               AND contype='f' AND convalidated AND confdeltype='r' AND confmatchtype='s'",
                &[&table, &target],
            )
            .await
            .unwrap();
        let source = format!("FOREIGN KEY ({pointer}, company_id, product_id)");
        assert!(
            rows.iter().any(|row| {
                let definition: String = row.get(0);
                definition.starts_with(&source)
                    && definition.contains("(id, company_id, product_id)")
            }),
            "{table}.{pointer}"
        );
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn existing_valuation_schema_refuses_conflicting_lineage_atomically() {
    let h = harness().await;
    let options = erp_testkit::SchemaSyncOptions::inline().lifecycle(true);
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let sid = source(&mut tx, "existing cursor").await;
    // Recreate the previous schema for these two constraints while retaining a scalar FK.
    tx.batch(
        "ALTER TABLE stk_cost_state DROP CONSTRAINT stk_cost_state_valued_through_source_company_product;
         ALTER TABLE stk_value_journal DROP CONSTRAINT stk_value_journal_generation_nonneg;",
    ).await.unwrap();
    let id = insert(
        &mut tx,
        &format!("INSERT INTO stk_cost_state(company_id,product_id,method,policy,grid_value,quantity,currency_carry,currency_id,currency_increment,generation,valued_through_time,valued_through_source_id)
                  VALUES(102,101,'average','cost_scale_v1',7,3,0,(SELECT id FROM res_currency LIMIT 1),0.01,1,'2026-01-01',{sid}) RETURNING id"),
    ).await.unwrap();
    tx.commit().await.unwrap();
    drop(tx);

    let error = h.orm().sync_schema_with(&options).await.unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"));
    assert_eq!(
        error.constraint(),
        Some("stk_cost_state_valued_through_source_company_product")
    );
    let mut tx = h.begin().await.unwrap();
    let constraints = "SELECT conname::text FROM pg_constraint WHERE
          (conrelid='stk_value_journal'::regclass AND conname='stk_value_journal_generation_nonneg')
       OR (conrelid='stk_cost_state'::regclass AND conname='stk_cost_state_valued_through_source_company_product')";
    assert!(tx.query(constraints, &[]).await.unwrap().is_empty());
    let cursor = "SELECT valued_through_source_id, grid_value::text, quantity::text, generation
                  FROM stk_cost_state WHERE id=$1";
    let rows = tx.query(cursor, &[&id]).await.unwrap();
    assert_eq!(rows[0].get::<_, Option<i64>>(0), Some(sid));

    // Remove just the conflicting cursor from the fixture before retrying.
    tx.execute(
        "UPDATE stk_cost_state SET valued_through_time=NULL,valued_through_source_id=NULL WHERE id=$1",
        &[&id],
    ).await.unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    for _ in 0..2 {
        assert!(h.orm().sync_schema_with(&options).await.unwrap().is_empty());
    }
    let mut tx = h.begin().await.unwrap();
    assert_eq!(tx.query(constraints, &[]).await.unwrap().len(), 2);
    let rows = tx.query(cursor, &[&id]).await.unwrap();
    assert_eq!(rows[0].get::<_, Option<i64>>(0), None);
    assert_eq!(rows[0].get::<_, String>(1), "7");
    assert_eq!(rows[0].get::<_, String>(2), "3");
    assert_eq!(rows[0].get::<_, i32>(3), 1);
    tx.rollback().await.unwrap();
}

// Independent column inventory from approved table note 2.1–2.10, with L-2.7 applied.
fn column_contract(table: &str) -> &'static str {
    match table {
        "stk_value_source" => {
            "id company_id product_id origin_model origin_id component source_revision kind cost_partition effective_time affected_from_time affected_from_source_id method policy schema_version semantic_version request_canonical request_digest create_uid create_date write_uid write_date"
        }
        "stk_value_source_alloc" => {
            "id source_id ordinal company_id product_id lot_id move_line_id serial_child_id correction_id side quantity recorded_at"
        }
        "stk_value_receipt" => {
            "id source_id company_id product_id state applied_mode applied_generation applied_at recorded_at"
        }
        "stk_value_journal" => {
            "id source_id company_id product_id lot_id seq role quantity_delta grid_delta booked_delta currency_id currency_increment rounding_mode method policy category_id effective_time recognition_date cause_journal_id cause_source_id generation recorded_at"
        }
        "stk_value_remainder" => {
            "id journal_id company_id policy boundary operand_value operand_quantity operand_base_quantity remainder_num remainder_den recorded_at"
        }
        "stk_cost_state" => {
            "id company_id product_id lot_id method policy grid_value quantity currency_carry currency_id currency_increment valued_through_time valued_through_source_id covered_through_source_id generation row_version"
        }
        "stk_cost_control" => {
            "id company_id product_id worker_epoch seal_revision sealed seal_token sealed_until publication_generation drained_source_id recorded_at"
        }
        "stk_cost_checkpoint" => {
            "id company_id product_id lot_id effective_time source_id grid_value quantity currency_carry currency_id currency_increment generation recorded_at"
        }
        "stk_cost_layer" => {
            "id company_id product_id lot_id source_id kind effective_time original_quantity original_value consumed_quantity consumed_value residual_num residual_den shortage_source_id shortage_policy generation row_version"
        }
        "stk_layer_consumption" => {
            "id layer_id out_source_id seq company_id product_id quantity grid_value generation recorded_at"
        }
        other => panic!("unknown valuation table {other}"),
    }
}
