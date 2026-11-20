// SPDX-License-Identifier: MIT OR Apache-2.0
//! Storage and uninstall guards for dated reconciliation facts.
use erp_sdk::sql_objects::{
    SqlConditionDecl, SqlConditionKind, SqlDep, SqlEvent, SqlFunctionDecl, SqlLanguage, SqlLevel,
    SqlObjectDecl, SqlObjectManifest, SqlResult, SqlTiming, SqlTriggerDecl,
};
use erp_sdk::sql_uninstall::{SqlGuardRead, SqlUninstallGuard, SqlUninstallGuardManifest};
use erp_sdk::{BoxFuture, Result};

/// Required, linked SQL declarations for removal history.
pub static MANIFEST: SqlObjectManifest = SqlObjectManifest::new(
    "account",
    &[
        "removal_immutable",
        "removal_guard",
        "removal_update",
        "removal_delete",
        "removal_truncate",
        "coverage_update",
        "coverage_delete",
        "coverage_truncate",
        "partial_capture_required",
        "partial_capture_guard",
        "partial_capture_delete",
        "partial_capture_truncate",
    ],
);
macro_rules! object {
    ($name:ident, $decl:expr) => {
        #[erp_sdk::sql_objects::linkme::distributed_slice(erp_sdk::sql_objects::SQL_OBJECT_DECLS)]
        #[linkme(crate = erp_sdk::sql_objects::linkme)]
        static $name: SqlObjectDecl = $decl;
    };
}
object!(
    IMMUTABLE,
    SqlObjectDecl::condition(
        "account",
        "removal_immutable",
        SqlConditionDecl::new(
            "ERP65",
            "account_removal_immutable",
            "Reconciliation removal history cannot be changed after insertion."
        )
        .kind(SqlConditionKind::Programming)
    )
);
object!(GUARD, SqlObjectDecl::function("account", "removal_guard", SqlFunctionDecl::new(
    "acc_removal_guard", SqlLanguage::PlPgSql, SqlResult::Trigger, r#"
BEGIN
    IF TG_OP = 'TRUNCATE' THEN
        RAISE EXCEPTION 'Reconciliation removal history cannot be truncated.' USING ERRCODE='ERP65', CONSTRAINT='account_removal_immutable';
    ELSIF EXISTS (SELECT FROM old_rows) THEN
        RAISE EXCEPTION 'Reconciliation removal history cannot be changed.' USING ERRCODE='ERP65', CONSTRAINT='account_removal_immutable';
    END IF;
    RETURN NULL;
END
"#).deps(&[SqlDep::Object {addon: "account", key: "removal_immutable"}])));
macro_rules! guard_change {
    ($name:ident, $key:literal, $model:literal, $event:expr $(, $new:literal)?) => {
        object!(
            $name,
            SqlObjectDecl::trigger(
                "account",
                $key,
                SqlTriggerDecl::new(
                    concat!("acc_", $key),
                    $model,
                    ("account", "removal_guard"),
                    SqlTiming::After,
                    &[$event],
                    SqlLevel::Statement
                )
                .old_table("old_rows")$(.new_table($new))?
            )
        );
    };
}
guard_change!(
    UPDATE,
    "removal_update",
    "account.reconcile.removal",
    SqlEvent::Update,
    "new_rows"
);
guard_change!(
    DELETE,
    "removal_delete",
    "account.reconcile.removal",
    SqlEvent::Delete
);
object!(
    TRUNCATE,
    SqlObjectDecl::trigger(
        "account",
        "removal_truncate",
        SqlTriggerDecl::new(
            "acc_removal_truncate",
            "account.reconcile.removal",
            ("account", "removal_guard"),
            SqlTiming::Before,
            &[SqlEvent::Truncate],
            SqlLevel::Statement
        )
    )
);

guard_change!(
    COVERAGE_UPDATE,
    "coverage_update",
    "account.reconcile.coverage",
    SqlEvent::Update,
    "new_rows"
);
guard_change!(
    COVERAGE_DELETE,
    "coverage_delete",
    "account.reconcile.coverage",
    SqlEvent::Delete
);
object!(
    COVERAGE_TRUNCATE,
    SqlObjectDecl::trigger(
        "account",
        "coverage_truncate",
        SqlTriggerDecl::new(
            "acc_coverage_truncate",
            "account.reconcile.coverage",
            ("account", "removal_guard"),
            SqlTiming::Before,
            &[SqlEvent::Truncate],
            SqlLevel::Statement
        )
    )
);

// The earlier binary deletes the source without inserting endpoint history. Reject that write
// even during a rolling upgrade. AFTER statement triggers see both effects of the service CTE.
object!(
    CAPTURE_REQUIRED,
    SqlObjectDecl::condition(
        "account",
        "partial_capture_required",
        SqlConditionDecl::new(
            "ERP66",
            "account_capture_required",
            "Cannot remove a match without dated company-currency facts for both journal items."
        )
        .kind(SqlConditionKind::Validation)
    )
);
object!(CAPTURE_GUARD,SqlObjectDecl::function("account","partial_capture_guard",
    SqlFunctionDecl::new("acc_partial_capture_guard",SqlLanguage::PlPgSql,SqlResult::Trigger,r#"
BEGIN
    IF TG_OP='TRUNCATE' THEN
        RAISE EXCEPTION 'Historical match capture required.' USING ERRCODE='ERP66', CONSTRAINT='account_capture_required';
    END IF;
    IF EXISTS (
        SELECT FROM old_rows p CROSS JOIN LATERAL (VALUES
            (p.debit_move_id,p.debit_currency_id,-p.amount),
            (p.credit_move_id,p.credit_currency_id,p.amount)) side(line_id,currency_id,delta)
        WHERE NOT EXISTS (SELECT FROM account_reconcile_removal h
            WHERE h.original_partial_id=p.id AND h.line_id=side.line_id AND h.currency_id=side.currency_id
              AND h.matched_delta=side.delta AND h.matched_on=p.max_date)
    ) THEN
        RAISE EXCEPTION 'Historical match capture required.' USING ERRCODE='ERP66', CONSTRAINT='account_capture_required';
    END IF;
    RETURN NULL;
END
"#).deps(&[SqlDep::Object{addon:"account",key:"partial_capture_required"}])));
object!(
    CAPTURE_DELETE,
    SqlObjectDecl::trigger(
        "account",
        "partial_capture_delete",
        SqlTriggerDecl::new(
            "acc_partial_capture_delete",
            "account.partial.reconcile",
            ("account", "partial_capture_guard"),
            SqlTiming::After,
            &[SqlEvent::Delete],
            SqlLevel::Statement
        )
        .old_table("old_rows")
    )
);
object!(
    CAPTURE_TRUNCATE,
    SqlObjectDecl::trigger(
        "account",
        "partial_capture_truncate",
        SqlTriggerDecl::new(
            "acc_partial_capture_truncate",
            "account.partial.reconcile",
            ("account", "partial_capture_guard"),
            SqlTiming::Before,
            &[SqlEvent::Truncate],
            SqlLevel::Statement
        )
    )
);

/// Lifecycle must retain the historical facts after the originating match has been removed.
pub static UNINSTALL: SqlUninstallGuardManifest =
    SqlUninstallGuardManifest::new("account", &["reconciliation_removal_history"]);

#[erp_sdk::sql_uninstall::linkme::distributed_slice(erp_sdk::sql_uninstall::SQL_UNINSTALL_GUARDS)]
#[linkme(crate = erp_sdk::sql_uninstall::linkme)]
static HISTORY: SqlUninstallGuard = SqlUninstallGuard::new(
    "account",
    "reconciliation_removal_history",
    &["account.reconcile.removal"],
    "account_reconciliation_removal_present",
    "Dated reconciliation history prevents uninstalling Accounting.",
    history_rows,
);
fn history_rows<'a>(read: &'a mut SqlGuardRead<'_>) -> BoxFuture<'a, Result<u64>> {
    Box::pin(async move { read.count_all("account.reconcile.removal").await })
}
