// SPDX-License-Identifier: MIT OR Apache-2.0
//! Upgrading `account` from the release before payments onto a database that holds books: a posted
//! invoice and a bank journal written before payment methods existed. The earlier release is
//! presented the way `stock_tracking_upgrade.rs` does it: the current install with the payment
//! tables and the entries' payment link dropped, the version marker rewound to 1.3.0 (the release before payments) and the
//! installed receipt given a digest no addon folder has, upgraded as a release is
//! (`identity_previous`). The models' `ir_model` rows stay, which the earlier release never wrote.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

#[path = "support/accounting.rs"]
mod accounting;

use accounting::*;
use erp_modules::ModuleName;
use erp_sdk::prelude::*;
use erp_sdk::{Command, Context, SUPERUSER_ID};

/// A digest no addon folder has: the receipt of the release before payments.
const EARLIER_ACCOUNT: &str =
    "addon-v1:0000000000000000000000000000000000000000000000000000000000000002";

/// Turns the database back into the release before payments and upgrades `account` again.
async fn rewind_and_upgrade(db: &erp_testkit::TestDb) -> erp_sdk::Orm {
    let client = db.client().await.unwrap();
    client
        .batch_execute(
            "ALTER TABLE account_move DROP COLUMN origin_payment_id; \
             DROP TABLE account_payment_register_move_rel, account_payment_register, \
             account_payment, account_payment_method_line, account_payment_method CASCADE; \
             DELETE FROM ir_model_data WHERE model IN \
             ('account.payment.method', 'account.payment.method.line'); \
             UPDATE ir_module_module SET latest_version = '1.3.0' WHERE name = 'account'",
        )
        .await
        .unwrap();
    let changed = client
        .execute(
            "UPDATE installed_addon_identity SET content_hash = $1 WHERE name = 'account'",
            &[&EARLIER_ACCOUNT],
        )
        .await
        .unwrap();
    assert_eq!(changed, 1);
    let mut previous = identities();
    previous.insert(
        ModuleName::new("account").unwrap(),
        EARLIER_ACCOUNT.to_owned(),
    );

    let system = system(db, Some(previous));
    let outcome = Box::pin(system.button_immediate_upgrade(
        &system.system_env(Context::new()),
        &[ModuleName::new("account").unwrap()],
    ))
    .await
    .unwrap();
    assert!(outcome.post_commit.is_clean(), "{:?}", outcome.post_commit);
    outcome.orm
}

/// lines from the upgrade, and a payment drafted on it takes the inbound one; the Register Payment
/// dialog reads an invoice posted before the upgrade)
#[test]
fn an_upgrade_onto_books_gives_existing_journals_their_methods() {
    big_stack(upgrade_body);
}

async fn upgrade_body() {
    let (db, orm) = install().await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    let second = other_company(&mut tx, company).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let b = books(&mut tx, company).await;
    let till = account(&mut tx, second, "101500", "asset_cash").await;
    let cash = journal(&mut tx, second, "CSH2", "cash", Some(till)).await;
    let invoice =
        posted_invoice(&mut tx, &b, "out_invoice", b.customer, "100", d(2024, 3, 1)).await;
    let invoice_id = invoice.ids()[0];
    let number = string(&invoice, &mut tx, "name").await;
    // written directly, as a journal of the earlier release: no method lines
    let bank = journal(&mut tx, company, "BNK7", "bank", Some(b.bank)).await;
    tx.commit().await.unwrap();
    drop(tx);
    drop(orm);

    let orm = rewind_and_upgrade(&db).await;

    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let lines: Vec<(String, String)> = tx
        .query(
            "SELECT payment_type::text, name::text FROM account_payment_method_line \
             WHERE journal_id = $1 ORDER BY payment_type",
            &[&bank],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        lines,
        vec![
            ("inbound".to_owned(), "Manual Payment".to_owned()),
            ("outbound".to_owned(), "Manual Payment".to_owned()),
        ]
    );
    let others: i64 = tx
        .query(
            "SELECT count(*)::int8 FROM account_payment_method_line l \
             JOIN account_journal j ON j.id = l.journal_id WHERE j.type NOT IN ('bank', 'cash', 'credit')",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(
        others, 0,
        "sale, purchase and general journals get no method lines"
    );
    // a second run adds nothing; every line is in its journal's company
    let lines_of = async |tx: &mut Transaction| -> Vec<(i64, i64, i64)> {
        tx.query(
            "SELECT l.journal_id::int8, count(*)::int8, \
                    count(*) FILTER (WHERE l.company_id = j.company_id)::int8 \
             FROM account_payment_method_line l JOIN account_journal j ON j.id = l.journal_id \
             WHERE l.journal_id = ANY($1) GROUP BY l.journal_id ORDER BY l.journal_id",
            &[&vec![bank, cash]],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
    };
    let mut expected = vec![(bank, 2, 2), (cash, 2, 2)];
    expected.sort_unstable();
    assert_eq!(lines_of(&mut tx).await, expected);
    erp_addon_account::payment::add_default_method_lines(
        &mut tx,
        &erp_sdk::migrations::SemverVersion::new(1, 3, 0),
    )
    .await
    .unwrap();
    assert_eq!(lines_of(&mut tx).await, expected);

    let invoice = moves(&tx, company).browse([invoice_id]).unwrap();
    assert_eq!(string(&invoice, &mut tx, "state").await, "posted");
    assert_eq!(string(&invoice, &mut tx, "name").await, number);
    assert_eq!(
        invoice.cell(&mut tx, "origin_payment_id").await.unwrap(),
        Cell::Null
    );

    // a payment on the upgraded journal takes its inbound line
    let payment = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("account.payment")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("payment_type", text("inbound"))
                    .set_cell("partner_type", text("customer"))
                    .set_cell("partner_id", Cell::Id(b.customer))
                    .set_cell("journal_id", Cell::Id(bank))
                    .set_cell("amount", Cell::Decimal(num("100"))),
            ],
        )
        .await
        .unwrap();
    let inbound: i64 = tx
        .query(
            "SELECT id::int8 FROM account_payment_method_line \
             WHERE journal_id = $1 AND payment_type = 'inbound'",
            &[&bank],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(
        payment
            .cell(&mut tx, "payment_method_line_id")
            .await
            .unwrap(),
        Cell::Id(inbound)
    );
    assert_eq!(string(&payment, &mut tx, "state").await, "draft");
    // with an outstanding account on the line it is confirmed
    let receipts = account(&mut tx, company, "101310", "asset_current").await;
    tx.env()
        .records("account.payment.method.line")
        .unwrap()
        .browse([inbound])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("payment_account_id", Cell::Id(receipts)),
        )
        .await
        .unwrap();
    payment
        .write(
            &mut tx,
            RawVals::new().set_cell("date", Cell::Date(d(2024, 3, 5))),
        )
        .await
        .unwrap();
    try_call(&payment, &mut tx, "action_post").await.unwrap();
    assert_eq!(string(&payment, &mut tx, "state").await, "paid");

    // the Register Payment dialog exists after the upgrade and reads the earlier invoice
    let dialog = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("account.payment.register")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_commands("invoice_ids", vec![Command::set([rec(invoice_id)])])
                    .set_cell("payment_date", Cell::Date(d(2024, 3, 5))),
            ],
        )
        .await
        .unwrap();
    assert_eq!(decimal(&dialog, &mut tx, "amount_due").await, num("100"));
    dialog
        .call::<(), Value>(&mut tx, "action_create_payments", ())
        .await
        .unwrap();
    assert_eq!(string(&invoice, &mut tx, "payment_state").await, "paid");
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}

/// A demo database upgraded from the release before payments ends as a fresh demo install does:
/// the demo bank and cash journals get their method lines with the demo outstanding accounts, so a
/// payment on the demo bank journal is confirmed.
#[test]
fn an_upgraded_demo_database_confirms_payments() {
    big_stack(demo_body);
}

async fn demo_body() {
    let (db, orm) = install_with_demo(true).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = main_company(&mut tx).await;
    tx.commit().await.unwrap();
    periods(&orm, company).await;
    drop(orm);
    let orm = rewind_and_upgrade(&db).await;

    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let bank = xml_id(&mut tx, "account", "journal_demo_bank").await;
    let cash = xml_id(&mut tx, "account", "journal_demo_cash").await;
    let set: i64 = tx
        .query(
            "SELECT count(*)::int8 FROM account_payment_method_line \
             WHERE journal_id = ANY($1) AND payment_account_id IS NOT NULL",
            &[&vec![bank, cash]],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(
        set, 4,
        "both lines of both demo journals book to the demo accounts"
    );
    let customer = partner(&mut tx, "Upgraded demo customer").await;
    let payment = tx
        .env()
        .with_company(company)
        .unwrap()
        .records("account.payment")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("partner_id", Cell::Id(customer))
                    .set_cell("journal_id", Cell::Id(bank))
                    .set_cell("date", Cell::Date(d(2024, 3, 5)))
                    .set_cell("amount", Cell::Decimal(num("10"))),
            ],
        )
        .await
        .unwrap();
    try_call(&payment, &mut tx, "action_post").await.unwrap();
    assert_eq!(string(&payment, &mut tx, "state").await, "paid");
    tx.rollback().await.unwrap();
    drop(orm);
    db.cleanup().await.unwrap();
}
