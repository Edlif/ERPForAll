// SPDX-License-Identifier: MIT OR Apache-2.0
//! Direct SQL must preserve the costing history and the identity of mutable projections.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod common;
use common::*;
use erp_sdk::prelude::*;

async fn refused(tx: &mut Transaction, sql: &str, condition: &str) {
    let error = tx
        .savepoint(async |tx| tx.execute(sql, &[]).await.map(|_| ()))
        .await
        .expect_err(sql);
    assert_eq!(error.kind(), erp_sdk::ErrorKind::Programming, "{error:?}");
    assert_eq!(error.constraint(), Some(condition), "{error:?}");
    let sqlstate = match condition {
        "stock_valuation_row_immutable" => "ERP61",
        "stock_valuation_receipt_final" => "ERP62",
        "stock_valuation_control_keys" => "ERP63",
        "stock_valuation_layer_original" => "ERP64",
        other => panic!("unexpected condition {other}"),
    };
    assert_eq!(error.sqlstate(), Some(sqlstate));
    assert_eq!(error.message(), "Internal server error");
}

#[tokio::test]
async fn protected_history_has_no_in_place_mutation_even_in_bulk() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    source(&mut tx, "first").await;
    source(&mut tx, "second").await;
    refused(
        &mut tx,
        "UPDATE stk_value_source SET component='modified' || id::text",
        "stock_valuation_row_immutable",
    )
    .await;
    refused(
        &mut tx,
        "DELETE FROM stk_value_source",
        "stock_valuation_row_immutable",
    )
    .await;
    let rows = tx
        .query("SELECT component FROM stk_value_source ORDER BY id", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<_, String>(0), "first");
    assert_eq!(rows[1].get::<_, String>(0), "second");
    // Guard each operation on each participating table; the schema exposes the transition trigger event mask.
    for (table, masks) in [
        ("stk_value_source", &[16_i16, 8, 34][..]),
        ("stk_value_source_alloc", &[16, 8, 34]),
        ("stk_value_journal", &[16, 8, 34]),
        ("stk_value_remainder", &[16, 8, 34]),
        ("stk_layer_consumption", &[16, 8, 34]),
        ("stk_value_receipt", &[16, 8, 34]),
        ("stk_cost_control", &[16, 8, 34]),
        ("stk_cost_layer", &[16]),
    ] {
        for mask in masks {
            let rows=tx.query("SELECT count(*)::int8 FROM pg_trigger WHERE tgrelid=to_regclass($1) AND NOT tgisinternal AND tgname LIKE 'stk_val_%' AND tgtype=$2 AND tgenabled='A'", &[&table,mask]).await.unwrap();
            assert_eq!(rows[0].get::<_, i64>(0), 1, "{table}/{mask}");
        }
    }
    refused(
        &mut tx,
        "TRUNCATE stk_layer_consumption",
        "stock_valuation_row_immutable",
    )
    .await;
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn receipt_finalizes_once_and_control_counters_never_decrease() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let id = source(&mut tx, "receipt").await;
    tx.execute("INSERT INTO stk_value_receipt(source_id,company_id,product_id,state,recorded_at) VALUES($1,101,101,'pending','2026-01-01')", &[&id]).await.unwrap();
    let receipt = "stock_valuation_receipt_final";
    refused(
        &mut tx,
        "UPDATE stk_value_receipt SET recorded_at='2026-03-01'",
        receipt,
    )
    .await;
    refused(&mut tx, "UPDATE stk_value_receipt SET state='applied',applied_generation=0,applied_mode='invented',applied_at='2026-01-01'", receipt).await;
    tx.execute("UPDATE stk_value_receipt SET state='applied',applied_generation=0,applied_at='2026-01-01',applied_mode='inline'", &[]).await.unwrap();
    refused(
        &mut tx,
        "UPDATE stk_value_receipt SET applied_mode='replay'",
        receipt,
    )
    .await;
    refused(&mut tx, "DELETE FROM stk_value_receipt", receipt).await;
    tx.execute("INSERT INTO stk_cost_control(company_id,product_id,worker_epoch,seal_revision,publication_generation,sealed,recorded_at) VALUES(101,101,2,2,2,false,'2026-01-01')",&[]).await.unwrap();
    let control = "stock_valuation_control_keys";
    for change in [
        "company_id=102",
        "product_id=102",
        "worker_epoch=1",
        "seal_revision=1",
        "publication_generation=1",
    ] {
        refused(
            &mut tx,
            &format!("UPDATE stk_cost_control SET {change}"),
            control,
        )
        .await;
    }
    tx.execute(
        "UPDATE stk_cost_control SET worker_epoch=3,seal_revision=4,publication_generation=3",
        &[],
    )
    .await
    .unwrap();
    refused(&mut tx, "DELETE FROM stk_cost_control", control).await;
    tx.execute("INSERT INTO stk_cost_layer(company_id,product_id,source_id,kind,effective_time,original_quantity,original_value,consumed_quantity,consumed_value,residual_num,residual_den,generation) VALUES(101,101,$1,'receipt','2026-01-01',3,10,0,0,0,1,0)", &[&id]).await.unwrap();
    for change in [
        "original_value=12",
        "original_quantity=4",
        "effective_time='2026-02-01'",
    ] {
        refused(
            &mut tx,
            &format!("UPDATE stk_cost_layer SET {change}"),
            "stock_valuation_layer_original",
        )
        .await;
    }
    tx.execute("UPDATE stk_cost_layer SET consumed_quantity=1,consumed_value=3,residual_num=1,residual_den=3", &[]).await.unwrap();
    tx.rollback().await.unwrap();
}
