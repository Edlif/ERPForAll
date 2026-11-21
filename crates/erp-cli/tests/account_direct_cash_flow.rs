// SPDX-License-Identifier: MIT OR Apache-2.0
//! Direct cash flow is based on settled bank/cash entries, with no synthetic split of ambiguous items.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration assertions"
)]
#[allow(clippy::panic, reason = "shared fixture assertions")]
#[path = "support/accounting.rs"]
mod accounting;
use accounting::*;
use erp_addon_reporting::runtime;
use erp_modules::{InitOptions, ModuleName};
use erp_orm::UserId;
use erp_sdk::{Cell, Command, Context, Domain, RawVals, SUPERUSER_ID, Value};
use erp_testkit::TestDb;
use serde_json::json;
use std::sync::Arc;

#[test]
fn posted_cash_movements_partition_by_visible_counterpart_and_reconcile() {
    big_stack(statement);
}

async fn statement() {
    let db = TestDb::builder().pool_size(8).create().await.unwrap();
    let system = system(&db, None);
    let initial = system
        .initialize(InitOptions::new().with_skip_auto_install(true))
        .await
        .unwrap();
    let installed = system
        .button_immediate_install(
            &initial.orm.env(SUPERUSER_ID, Context::new()),
            &[ModuleName::new("account_reporting").unwrap()],
        )
        .await
        .unwrap();
    assert!(installed.post_commit.is_clean());
    let orm = &installed.orm;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let report = xml_id(&mut tx, "account_reporting", "report_cash_flow").await;
    tx.commit().await.unwrap();
    drop(tx);
    periods(orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let books = books(&mut tx, company).await;
    let cash = account(&mut tx, company, "100801", "asset_cash").await;
    let till = account(&mut tx, company, "100802", "asset_cash").await;
    let assets = account(&mut tx, company, "100803", "asset_fixed").await;
    let loans = account(&mut tx, company, "200801", "liability_non_current").await;
    let open = account(&mut tx, company, "300801", "equity").await;
    let operating = xml_id(&mut tx, "account", "account_tag_operating").await;
    let investing = xml_id(&mut tx, "account", "account_tag_investing").await;
    let financing = xml_id(&mut tx, "account", "account_tag_financing").await;
    // A tag on the shared cash account cannot classify all of its movements as financing.
    for (id, tags) in [
        (cash, vec![financing]),
        (assets, vec![investing]),
        (loans, vec![financing]),
        (books.income, vec![operating]),
        (books.expense, vec![operating, investing]),
    ] {
        tx.env()
            .records("account.account")
            .unwrap()
            .browse([id])
            .unwrap()
            .write(
                &mut tx,
                RawVals::new()
                    .set_commands("tag_ids", vec![Command::set(tags.into_iter().map(rec))]),
            )
            .await
            .unwrap();
    }
    posted_entry(&mut tx, &books, cash, open, None, "100", d(2024, 1, 30)).await;
    let feb = d(2024, 2, 10);
    posted_entry(&mut tx, &books, cash, books.income, None, "25", feb).await;
    posted_entry(&mut tx, &books, assets, cash, None, "10", feb).await;
    posted_entry(&mut tx, &books, cash, loans, None, "20", feb).await;
    posted_entry(&mut tx, &books, cash, open, None, "7", feb).await;
    posted_entry(&mut tx, &books, books.expense, cash, None, "3", feb).await;
    posted_entry(&mut tx, &books, till, cash, None, "40", feb).await;
    posted_entry(&mut tx, &books, assets, open, None, "99", feb).await;
    // One bank receipt from multiple differently tagged counterpart lines remains unclassified.
    let lines = [(cash, "8"), (assets, "-4"), (loans, "-4")].map(|(id, amount)| {
        Command::create(
            RawVals::new()
                .set_cell("account_id", Cell::Id(id))
                .set_cell("balance", Cell::Decimal(num(amount))),
        )
    });
    let mixed = moves(&tx, company)
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("date", Cell::Date(feb))
                    .set_commands("line_ids", lines.to_vec()),
            ],
        )
        .await
        .unwrap();
    mixed
        .call::<(), ()>(&mut tx, "action_post", ())
        .await
        .unwrap();
    posted_entry(&mut tx, &books, cash, loans, None, "99", d(2024, 3, 1)).await;
    let administrator = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "cashflow.admin", company, &[administrator]).await;
    let group = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader = user(&mut tx, "cashflow.reader", company, &[group]).await;

    tx.install_audit_schema().await.unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    let orm = policy_orm(&db, orm).await;
    let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
    let options = json!({"report":report,"companies":[company],"date":{"preset":"custom","from":"2024-02-01","to":"2024-02-29"}});
    let output = runtime::report_lines(&tx.env(), &mut tx, &options)
        .await
        .unwrap();
    let values = output["lines"].as_array().unwrap();
    for (code, value) in [
        ("OPENING", "100"),
        ("OPERATING", "25"),
        ("INVESTING", "-10"),
        ("FINANCING", "20"),
        ("UNCLASSIFIED", "12"),
        ("NET", "47"),
        ("CLOSING", "147"),
    ] {
        let suffix = format!("|line:{code}~~");
        let row = values
            .iter()
            .find(|r| r["id"].as_str().unwrap().ends_with(&suffix))
            .unwrap();
        assert_eq!(
            num(row["cells"][0]["value"].as_str().unwrap()),
            num(value),
            "{code}"
        );
        assert_eq!(row["cells"][0]["drillable"], true);
        let env = tx.env().clone();
        let drill = runtime::report_drill(
            &env,
            &mut tx,
            &options,
            row["id"].as_str().unwrap(),
            &json!({"cell":0}),
        )
        .await
        .unwrap();
        let selection = Domain::from_json(&drill["domain"]).unwrap();
        let items = env
            .records("account.move.line")
            .unwrap()
            .search(&mut tx, &selection)
            .await
            .unwrap();
        if code == "NET" {
            // Six movements cross the cash boundary. Neither side of the internal transfer can open here.
            assert_eq!(items.len(), 6);
        }
        let mut sum = num("0");
        for v in items.cells(&mut tx, "balance").await.unwrap() {
            let balance = match v {
                Cell::Decimal(value) => Some(value),
                _ => None,
            };
            sum += balance.expect("the source journal item's balance is monetary");
        }
        assert_eq!(sum, num(value), "source of {code}");
        erp_addon_web::metadata_test_support::test_normalize_action_result(
            &env,
            &mut tx,
            Value::Json(Arc::new(drill)),
        )
        .await
        .unwrap();
    }
}
