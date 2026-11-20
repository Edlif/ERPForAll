// SPDX-License-Identifier: MIT OR Apache-2.0
//! As-of financial position through the installed Accounting reporting menu definition.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration assertions"
)]

#[allow(clippy::panic, reason = "shared fixture field assertions")]
#[path = "support/accounting.rs"]
mod accounting;
use accounting::*;
use erp_addon_reporting::runtime;
use erp_modules::{InitOptions, ModuleName};
use erp_orm::UserId;
use erp_sdk::{Context, SUPERUSER_ID};
use erp_testkit::TestDb;
use serde_json::json;

#[test]
fn balance_sheet_reconciles_both_sides_before_and_after_earnings_allocation() {
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
    tx.commit().await.unwrap();
    drop(tx);
    periods(orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let equity = account(&mut tx, company, "301900", "equity").await;
    posted_entry(&mut tx, &b, b.bank, equity, None, "1000", d(2024, 1, 1)).await;
    posted_entry(
        &mut tx,
        &b,
        b.receivable,
        b.income,
        Some(b.customer),
        "120",
        d(2024, 2, 2),
    )
    .await;
    posted_entry(
        &mut tx,
        &b,
        b.bank,
        b.receivable,
        Some(b.customer),
        "50",
        d(2024, 2, 10),
    )
    .await;
    posted_entry(
        &mut tx,
        &b,
        b.expense,
        b.payable,
        Some(b.vendor),
        "40",
        d(2024, 2, 3),
    )
    .await;
    posted_entry(
        &mut tx,
        &b,
        b.payable,
        b.bank,
        Some(b.vendor),
        "10",
        d(2024, 2, 20),
    )
    .await;
    // Close the two P&L accounts to the equity account in March.
    posted_entry(&mut tx, &b, b.income, equity, None, "120", d(2024, 3, 1)).await;
    posted_entry(&mut tx, &b, equity, b.expense, None, "40", d(2024, 3, 1)).await;
    let report = xml_id(&mut tx, "account_reporting", "report_balance_sheet").await;
    let group = xml_id(&mut tx, "base", "group_system").await;
    user(&mut tx, "position.admin", company, &[group]).await;
    let group = xml_id(&mut tx, "account", "group_account_readonly").await;
    let reader = user(&mut tx, "position.reader", company, &[group]).await;
    tx.install_audit_schema().await.unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    let orm = policy_orm(&db, orm).await;
    let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
    for (date, capital, open_result) in [("2024-02-29", "1000", "80"), ("2024-03-01", "1080", "0")]
    {
        let options =
            json!({"report":report,"companies":[company],"date":{"preset":"custom","to":date}});
        let output = runtime::report_lines(&tx.env(), &mut tx, &options)
            .await
            .unwrap();
        let amount = |name: &str| {
            let row = output["lines"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == name)
                .unwrap();
            num(row["cells"][0]["value"].as_str().unwrap())
        };
        assert_eq!(amount("Assets"), num("1110"));
        assert_eq!(amount("Liabilities"), num("30"));
        assert_eq!(amount("Posted equity"), num(capital));
        assert_eq!(amount("Unallocated earnings"), num(open_result));
        assert_eq!(amount("Total equity"), num("1080"));
        assert_eq!(amount("Assets"), amount("Liabilities and equity"));
    }
}
