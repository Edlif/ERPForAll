// SPDX-License-Identifier: MIT OR Apache-2.0
//! Chart-default activation on an installed account addon with a staged pack fixture.

use std::collections::BTreeMap;
use std::fs;

use erp_addon_account::mapping::{CompanyDefault, DefaultRole, ResolutionDate};
use erp_cli::module::chart_defaults::ChartAccountDefaults;
use erp_modules::pack::{
    FieldSpec, FieldType, ModelSchema, PackLimits, PackRegistry, Record, SchemaSet, StageContext,
};
use erp_sdk::{ErrorKind, Orm, SUPERUSER_ID};
use sha2::{Digest, Sha256};

use super::account_defaults::{account, company};

const ROLE_RECORDS: &str = r#"
[[record]]
model = "account.account"
key = "sales"
[record.fields]
account_type = "income"

[[record]]
model = "account.account"
key = "inventory"
[record.fields]
account_type = "asset_current"

[[record]]
model = "res.company"
key = "defaults"
[record.fields]
income_account_id = "sales"
account_stock_valuation_id = "inventory"
"#;

fn defaults() -> Record {
    let schema = |model: &str, fields| ModelSchema {
        model: model.into(),
        fields,
        dated_scope: None,
        code_field: None,
        active_field: None,
    };
    let field = |name: &str, ty| FieldSpec {
        name: name.into(),
        ty,
        required: true,
        nullable: false,
    };
    let accounts = schema(
        "account.account",
        vec![field("account_type", FieldType::Text)],
    );
    let company = schema(
        "res.company",
        ["income_account_id", "account_stock_valuation_id"]
            .into_iter()
            .map(|name| field(name, FieldType::Ref("account.account".into())))
            .collect(),
    );
    let account_schema = accounts.digest();
    let company_schema = company.digest();
    let records_digest: String = Sha256::digest(ROLE_RECORDS)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let schemas = SchemaSet::new(vec![accounts, company]).unwrap();
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("chart.toml"), ROLE_RECORDS).unwrap();
    fs::write(
        directory.path().join("pack.toml"),
        format!(
            r#"
format_version = 1
pack_id = "example.chart"
profile_id = "example.chart.stock"
rule_release_id = "R1"
pack_version = 1
release_date = 2026-01-15
jurisdictions = ["XX"]

[schemas]
"account.account" = "{account_schema}"
"res.company" = "{company_schema}"

[origin]
kind = "authored"
publisher = "Example Publisher"
licence = "MIT OR Apache-2.0"
source_document = "Company chart test fixture"
source_version = "1"
verified_on = 2026-01-10
reviewer = "Fixture Reviewer"
review_status = "reviewed"

[[files]]
path = "chart.toml"
sha256 = "{records_digest}"
kind = "records"

[[template]]
key = "chart"
country = "XX"
files = ["chart.toml"]
"#
        ),
    )
    .unwrap();
    let mut registry = PackRegistry::default();
    registry
        .stage_dir(
            directory.path(),
            &StageContext {
                schemas: &schemas,
                limits: PackLimits::default(),
            },
        )
        .unwrap();
    registry.get("example.chart", "R1").unwrap().templates["chart"].records
        [&("res.company".into(), "defaults".into())]
        .clone()
}

pub(super) async fn exercise(orm: &Orm, main: i64) {
    let staged = defaults();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let a = company(&mut tx, main, "Pack West", "UTC").await;
    let b = company(&mut tx, main, "Pack East", "UTC").await;
    let sales_a = account(&mut tx, a, "40000", "income").await;
    let stock_a = account(&mut tx, a, "16000", "asset_current").await;
    let sales_b = account(&mut tx, b, "40000", "income").await;
    let stock_b = account(&mut tx, b, "16000", "asset_current").await;
    tx.commit().await.unwrap();
    drop(tx);

    let missing =
        ChartAccountDefaults::prepare(a, &staged, &BTreeMap::from([("sales".into(), sales_a)]))
            .err()
            .expect("the company charter references an account with no installed identity");
    assert_eq!(missing.kind(), ErrorKind::Validation);
    assert_eq!(
        missing.message(),
        "Company default account_stock_valuation_id has no resolved account for key inventory."
    );

    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let incomplete = ChartAccountDefaults::prepare(
        a,
        &staged,
        &BTreeMap::from([("sales".into(), sales_a), ("inventory".into(), stock_b)]),
    )
    .unwrap();
    let foreign = incomplete.apply(&mut tx).await.unwrap_err();
    assert_eq!(foreign.kind(), ErrorKind::UserError);
    assert_eq!(
        foreign.message(),
        "Account is not applicable to company Pack West"
    );
    let income_a = CompanyDefault::new(a, DefaultRole::Income).unwrap();
    assert_eq!(
        income_a
            .resolve(&mut tx, ResolutionDate::Current)
            .await
            .unwrap(),
        None
    );
    let a_accounts = BTreeMap::from([("sales".into(), sales_a), ("inventory".into(), stock_a)]);
    for (company, accounts) in [
        (a, a_accounts.clone()),
        (
            b,
            BTreeMap::from([("sales".into(), sales_b), ("inventory".into(), stock_b)]),
        ),
    ] {
        ChartAccountDefaults::prepare(company, &staged, &accounts)
            .unwrap()
            .apply(&mut tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    drop(tx);

    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    for (company, expected_account) in [(a, stock_a), (b, stock_b)] {
        let stock = CompanyDefault::new(company, DefaultRole::StockValuation).unwrap();
        assert_eq!(
            stock
                .require(&mut tx, ResolutionDate::Current)
                .await
                .unwrap(),
            expected_account
        );
    }
    assert_eq!(
        income_a
            .require(&mut tx, ResolutionDate::Current)
            .await
            .unwrap(),
        sales_a,
    );
    income_a.set(&mut tx, None).await.unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    ChartAccountDefaults::prepare(a, &staged, &a_accounts)
        .unwrap()
        .apply(&mut tx)
        .await
        .unwrap();
    assert_eq!(
        income_a
            .resolve(&mut tx, ResolutionDate::Current)
            .await
            .unwrap(),
        None
    );
    tx.rollback().await.unwrap();
}
