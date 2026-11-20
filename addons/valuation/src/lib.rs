// SPDX-License-Identifier: MIT OR Apache-2.0
//! Optional role-separated admission for original company-owned incoming stock.
use erp_sdk::sql_objects::{
    SqlBinding, SqlDep, SqlFunctionDecl, SqlLanguage, SqlObjectDecl, SqlObjectManifest, SqlResult,
    SqlScalar, SqlSecurity, SqlTableGrantDecl, SqlTableOp, SqlType,
};

erp_sdk::addon!(
    name = "valuation",
    exact_only = true,
    depends = ["base", "stock_account"]
);

pub mod lifecycle;
mod receipt;

pub(crate) const KEY_LIMIT: &str = "valuation.max_admission_keys_per_unit";
pub(crate) const UNIT_LIMIT: &str = "valuation.unit_transaction_timeout_ms";
pub(crate) const LOCK_WAIT: &str = "valuation.admission_lock_timeout_ms";

fn parameters() -> erp_sdk::Result<Vec<erp_sdk::settings::ParameterDecl>> {
    use erp_sdk::settings::ParameterDecl;
    Ok(vec![
        ParameterDecl::integer(
            KEY_LIMIT,
            "Valued products per stock operation",
            1000,
            (1, 10000),
        )?,
        ParameterDecl::integer(
            UNIT_LIMIT,
            "Maximum valuation transaction duration (ms)",
            120000,
            (30001, 600000),
        )?,
        ParameterDecl::integer(
            LOCK_WAIT,
            "Valuation admission lock wait (ms)",
            5000,
            (1, 30000),
        )?,
    ])
}
erp_sdk::system_parameters!(parameters);

/// Required SQL declarations for opt-in admission.
pub static MANIFEST: SqlObjectManifest =
    SqlObjectManifest::new("valuation", &["control_policy", "admit"]);
#[erp_sdk::sql_objects::linkme::distributed_slice(erp_sdk::sql_objects::SQL_OBJECT_DECLS)]
#[linkme(crate = erp_sdk::sql_objects::linkme)]
static POLICY: SqlObjectDecl = SqlObjectDecl::table_grant(
    "valuation",
    "control_policy",
    SqlTableGrantDecl::new(
        "stock.valuation.control",
        SqlBinding::Definer,
        &[SqlTableOp::Select, SqlTableOp::Insert, SqlTableOp::Update],
    )
    .exclusive_grantees(),
);
#[erp_sdk::sql_objects::linkme::distributed_slice(erp_sdk::sql_objects::SQL_OBJECT_DECLS)]
#[linkme(crate = erp_sdk::sql_objects::linkme)]
static ADMIT: SqlObjectDecl = SqlObjectDecl::function(
    "valuation",
    "admit",
    SqlFunctionDecl::new(
        "stk_admit_receipt_control",
        SqlLanguage::PlPgSql,
        SqlResult::Void,
        include_str!("control.sql"),
    )
    .args(&[
        SqlType::Scalar(SqlScalar::BigInt),
        SqlType::Scalar(SqlScalar::BigInt),
        SqlType::Scalar(SqlScalar::Integer),
    ])
    .deps(&[SqlDep::Model("stock.valuation.control")])
    .security(SqlSecurity::Definer)
    .grant_execute(&[SqlBinding::Request]),
);
