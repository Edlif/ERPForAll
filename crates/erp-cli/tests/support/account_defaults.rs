// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runtime checks for default account mappings on an installed accounting addon.

use erp_addon_account::mapping::{CompanyDefault, DefaultRole, ResolutionDate};
use erp_sdk::prelude::*;
use erp_sdk::{ErrorKind, Orm, SUPERUSER_ID};

pub(super) async fn exercise(orm: &Orm, main: i64) -> (CompanyDefault, i64) {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let a = company(&mut tx, main, "West", "Pacific/Pago_Pago").await;
    let b = company(&mut tx, main, "East", "Pacific/Kiritimati").await;
    let a0 = account(&mut tx, a, "16000", "asset_current").await;
    let a1 = account(&mut tx, a, "16001", "asset_current").await;
    let a2 = account(&mut tx, a, "16002", "asset_current").await;
    let b0 = account(&mut tx, b, "16000", "asset_current").await;
    let payable = account(&mut tx, a, "20000", "liability_payable").await;
    let west = CompanyDefault::new(a, DefaultRole::StockValuation).unwrap();
    let east = CompanyDefault::new(b, DefaultRole::StockValuation).unwrap();
    let other = CompanyDefault::new(a, DefaultRole::Payable).unwrap();
    let current = ResolutionDate::Current;
    let unconfigured = west.require(&mut tx, current).await.unwrap_err();
    assert_eq!(unconfigured.kind(), ErrorKind::UserError);
    assert_eq!(
        unconfigured.message(),
        "No stock valuation account configured for company West"
    );

    assert!(west.install_if_absent(&mut tx, Some(a0)).await.unwrap());
    assert!(other.install_if_absent(&mut tx, None).await.unwrap());
    assert!(east.install_if_absent(&mut tx, Some(b0)).await.unwrap());
    assert!(
        !other
            .install_if_absent(&mut tx, Some(payable))
            .await
            .unwrap()
    );
    assert_eq!(other.resolve(&mut tx, current).await.unwrap(), None);
    assert_eq!(west.require(&mut tx, current).await.unwrap(), a0);
    assert_eq!(east.require(&mut tx, current).await.unwrap(), b0);
    let generation: i64 = tx
        .query(
            "SELECT generation FROM account_default_generation WHERE company_id = $1",
            &[&a],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(generation, 1, "both West choices share one generation");
    tx.commit().await.unwrap();
    drop(tx);

    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    for (id, kind, message) in [
        (
            b0,
            ErrorKind::UserError,
            "Account is not applicable to company West",
        ),
        (
            payable,
            ErrorKind::UserError,
            "Configured account is inactive or invalid for role stock valuation",
        ),
        (
            i64::MAX,
            ErrorKind::Access,
            "Accounting configuration target does not exist or is not accessible",
        ),
        (
            0,
            ErrorKind::Validation,
            "Invalid accounting configuration identifier",
        ),
    ] {
        let error = west.set(&mut tx, Some(id)).await.unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.message(), message);
    }
    let row = &tx
        .query(
            "SELECT (transaction_timestamp() AT TIME ZONE 'Pacific/Pago_Pago')::date, \
                (transaction_timestamp() AT TIME ZONE 'Pacific/Kiritimati')::date",
            &[],
        )
        .await
        .unwrap()[0];
    let west_today: Date = row.get(0);
    let east_today: Date = row.get(1);
    assert!(
        west_today < east_today,
        "these company zones straddle the date line"
    );

    west.set(&mut tx, Some(a1)).await.unwrap();
    west.set(&mut tx, Some(a2)).await.unwrap();
    assert!(!west.install_if_absent(&mut tx, Some(a0)).await.unwrap());
    let yesterday = Date::from_days(west_today.days().checked_sub(1).unwrap());
    assert_eq!(
        west.require(&mut tx, ResolutionDate::Document(yesterday))
            .await
            .unwrap(),
        a0,
        "first choice remains on the day before the local replacement date"
    );
    assert_eq!(
        west.require(&mut tx, ResolutionDate::Document(west_today))
            .await
            .unwrap(),
        a2,
        "last ordinary edit in one transaction takes effect on the company's date"
    );
    assert_eq!(west.require(&mut tx, current).await.unwrap(), a2);
    assert_eq!(east.require(&mut tx, current).await.unwrap(), b0);
    let rows = tx
        .query(
            "SELECT s.effective_from::text, s.generation, k.version \
         FROM account_default_segment s JOIN account_default_key k ON k.id = s.key_id \
         JOIN account_default_role r ON r.id = k.role_id \
         WHERE k.company_id = $1 AND r.key = 'stock_valuation' ORDER BY s.generation",
            &[&a],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<_, String>(0), "-infinity");
    assert_eq!(rows[1].get::<_, String>(0), west_today.to_string());
    assert_eq!(rows[1].get::<_, i64>(1), 2);
    assert_eq!(rows[1].get::<_, i64>(2), 2);
    tx.commit().await.unwrap();
    drop(tx);

    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.env()
        .with_company(a)
        .unwrap()
        .records("account.account")
        .unwrap()
        .browse([a2])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("active", Cell::Bool(false)),
        )
        .await
        .unwrap();
    let error = west.require(&mut tx, current).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError);
    assert_eq!(
        error.message(),
        "Configured account is inactive or invalid for role stock valuation"
    );
    tx.rollback().await.unwrap();
    (west, a2)
}

pub(super) async fn company(tx: &mut Transaction, main: i64, name: &str, zone: &str) -> i64 {
    let company = tx.env().records("res.company").unwrap();
    let currency = company
        .browse([main])
        .unwrap()
        .cell(tx, "currency_id")
        .await
        .unwrap();
    company
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(name.into()))
                    .set_cell("tz", Cell::Str(zone.into()))
                    .set_cell("currency_id", currency),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}

pub(super) async fn account(tx: &mut Transaction, company: i64, code: &str, kind: &str) -> i64 {
    tx.env()
        .with_company(company)
        .unwrap()
        .records("account.account")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(code.into()))
                    .set_cell("code", Cell::Str(code.into()))
                    .set_cell("account_type", Cell::Str(kind.into())),
            ],
        )
        .await
        .unwrap()
        .ids()[0]
}
