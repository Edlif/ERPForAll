// SPDX-License-Identifier: MIT OR Apache-2.0
//! Generic ORM mutations cannot edit valuation evidence, including under elevated privileges.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod common;
use common::*;
use erp_sdk::prelude::*;
use erp_sdk::{ErrorKind, UpsertCall};

#[tokio::test]
async fn direct_orm_mutations_refuse_every_valuation_model() {
    let h = harness_builder()
        .policy(erp_sdk::StaticPolicy::SuperuserOnly)
        .build()
        .await
        .unwrap();
    let mut tx = h.begin().await.unwrap();
    let models: Vec<_> = h
        .registry()
        .models()
        .iter()
        .filter(|m| m.name.starts_with("stock.valuation."))
        .map(|m| m.name)
        .collect();
    assert_eq!(models.len(), 10);
    for name in models {
        let model = tx
            .env()
            .records(name)
            .unwrap()
            .with_context([("default_company_id", erp_sdk::Value::Int(101))]);
        for (caller, expected) in [
            (model.clone(), ErrorKind::UserError),
            (model.with_user(23), ErrorKind::Access),
        ] {
            let rows = caller.browse([101]).unwrap();
            for operation in ["create", "write", "upsert", "copy", "unlink"] {
                let error = tx
                    .savepoint(async |tx| match operation {
                        "create" => caller.create(tx, vec![RawVals::new()]).await.map(|_| ()),
                        "write" => rows.write(tx, RawVals::new()).await,
                        "upsert" => caller
                            .upsert(tx, UpsertCall::new("_key").with_rows([RawVals::new()]))
                            .await
                            .map(|_| ()),
                        "copy" => rows.copy(tx, None).await.map(|_| ()),
                        "unlink" => rows.unlink(tx).await,
                        _ => unreachable!(),
                    })
                    .await
                    .expect_err(operation);
                assert_eq!(error.kind(), expected, "{name}.{operation}: {error}");
                if expected == ErrorKind::UserError {
                    assert_eq!(
                        error.message(),
                        format!("Only the stock valuation engine can change {name}.")
                    );
                }
            }
        }
    }
    tx.rollback().await.unwrap();
}
