// SPDX-License-Identifier: MIT OR Apache-2.0
//! The schema must use the same numeric domains as the exact valuation codec.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod common;
use common::*;

#[tokio::test]
async fn costing_storage_rejects_unrepresentable_values_and_accepts_exact_boundaries() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    seed(&mut tx).await;
    let state = "INSERT INTO stk_cost_state(company_id,product_id,method,policy,grid_value,quantity,currency_carry,currency_id,currency_increment,generation) VALUES(101,101,'average','cost_scale_v1',1,-1,-0.005,(SELECT id FROM res_currency LIMIT 1),0.01,0) RETURNING id";
    let id = insert(&mut tx, state).await.unwrap();
    for (assignment, suffix) in [
        ("grid_value=1e-19", "_valid_grid_value"),
        ("grid_value=1e60", "_valid_grid_value"),
        ("currency_carry=0.00501", "_valid_carry"),
        ("currency_carry=-0.00501", "_valid_carry"),
        ("currency_increment=1e-29", "_valid_carry"),
    ] {
        rejected(
            &mut tx,
            &format!("UPDATE stk_cost_state SET {assignment} WHERE id={id}"),
            "stk_cost_state",
            suffix,
        )
        .await;
    }
    tx.execute("UPDATE stk_cost_state SET currency_carry=0.005, grid_value=-0.0000000000000000010 WHERE id=$1", &[&id]).await.unwrap();
    let sid = source(&mut tx, "numeric").await;
    let prefix = format!(
        "INSERT INTO stk_cost_layer(company_id,product_id,source_id,kind,effective_time,original_quantity,original_value,consumed_quantity,consumed_value,residual_num,residual_den,generation) VALUES(101,101,{sid},'receipt','2026-01-01',3,10,0,0,"
    );
    for (num, den) in [
        ("2", "6"),
        ("0", "2"),
        ("1", "0"),
        ("0.5", "1"),
        ("power(2::numeric,4096)", "1"),
    ] {
        rejected(
            &mut tx,
            &format!("{prefix}{num},{den},0)"),
            "stk_cost_layer",
            "_valid_fraction",
        )
        .await;
    }
    insert(&mut tx, &format!("{prefix}-1,3,0) RETURNING id"))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
}
