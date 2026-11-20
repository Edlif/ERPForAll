// SPDX-License-Identifier: MIT OR Apache-2.0
//! Additive, committed-visibility ledger epochs. No posting-wide shared-row lock.
use erp_sdk::sql_objects::{
    SqlEvent, SqlFunctionDecl, SqlLanguage, SqlLevel, SqlObjectDecl, SqlObjectManifest, SqlResult,
    SqlTiming, SqlTriggerDecl,
};
/// Independently required by composition sites, so a dropped registration fails loudly.
pub static MANIFEST: SqlObjectManifest = SqlObjectManifest::new(
    "account_reporting",
    &[
        "ledger_epoch_function",
        "ledger_epoch_move_insert",
        "ledger_epoch_move_update",
        "ledger_epoch_move_delete",
        "ledger_epoch_move_truncate",
        "ledger_epoch_item_insert",
        "ledger_epoch_item_update",
        "ledger_epoch_item_delete",
        "ledger_epoch_item_truncate",
        "ledger_epoch_journal_insert",
        "ledger_epoch_journal_update",
        "ledger_epoch_journal_delete",
        "ledger_epoch_journal_truncate",
        "ledger_epoch_account_insert",
        "ledger_epoch_account_update",
        "ledger_epoch_account_delete",
        "ledger_epoch_account_truncate",
        "ledger_epoch_company_insert",
        "ledger_epoch_company_update",
        "ledger_epoch_company_delete",
        "ledger_epoch_company_truncate",
        "ledger_epoch_currency_insert",
        "ledger_epoch_currency_update",
        "ledger_epoch_currency_delete",
        "ledger_epoch_currency_truncate",
        "ledger_epoch_report_insert",
        "ledger_epoch_report_update",
        "ledger_epoch_report_delete",
        "ledger_epoch_report_truncate",
        "ledger_epoch_column_insert",
        "ledger_epoch_column_update",
        "ledger_epoch_column_delete",
        "ledger_epoch_column_truncate",
        "ledger_epoch_line_insert",
        "ledger_epoch_line_update",
        "ledger_epoch_line_delete",
        "ledger_epoch_line_truncate",
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
    EPOCH,
    SqlObjectDecl::function(
        "account_reporting",
        "ledger_epoch_function",
        SqlFunctionDecl::new(
            "account_reporting_ledger_epoch",
            SqlLanguage::PlPgSql,
            SqlResult::Trigger,
            r#"
BEGIN
    IF current_setting('erp.account_reporting_ledger_bumped', true) IS DISTINCT FROM pg_current_xact_id()::text THEN
        INSERT INTO public.erp_signal_generation (key) VALUES ('cache:account_reporting.ledger');
        PERFORM set_config('erp.account_reporting_ledger_bumped', pg_current_xact_id()::text, true);
    END IF;
    RETURN NULL;
END
"#
        )
    )
);
macro_rules! source {
    ($insert:ident, $update:ident, $delete:ident, $truncate:ident, $key:literal, $model:literal) => {
        object!(
            $insert,
            SqlObjectDecl::trigger(
                "account_reporting",
                concat!($key, "_insert"),
                SqlTriggerDecl::new(
                    "account_reporting_ledger_epoch_insert",
                    $model,
                    ("account_reporting", "ledger_epoch_function"),
                    SqlTiming::After,
                    &[SqlEvent::Insert],
                    SqlLevel::Statement
                )
                .new_table("new_rows")
            )
        );
        object!(
            $update,
            SqlObjectDecl::trigger(
                "account_reporting",
                concat!($key, "_update"),
                SqlTriggerDecl::new(
                    "account_reporting_ledger_epoch_update",
                    $model,
                    ("account_reporting", "ledger_epoch_function"),
                    SqlTiming::After,
                    &[SqlEvent::Update],
                    SqlLevel::Statement
                )
                .old_table("old_rows")
                .new_table("new_rows")
            )
        );
        object!(
            $delete,
            SqlObjectDecl::trigger(
                "account_reporting",
                concat!($key, "_delete"),
                SqlTriggerDecl::new(
                    "account_reporting_ledger_epoch_delete",
                    $model,
                    ("account_reporting", "ledger_epoch_function"),
                    SqlTiming::After,
                    &[SqlEvent::Delete],
                    SqlLevel::Statement
                )
                .old_table("old_rows")
            )
        );
        object!(
            $truncate,
            SqlObjectDecl::trigger(
                "account_reporting",
                concat!($key, "_truncate"),
                SqlTriggerDecl::new(
                    "account_reporting_ledger_epoch_truncate",
                    $model,
                    ("account_reporting", "ledger_epoch_function"),
                    SqlTiming::Before,
                    &[SqlEvent::Truncate],
                    SqlLevel::Statement
                )
            )
        );
    };
}
source!(
    MOVE_INSERT,
    MOVE_UPDATE,
    MOVE_DELETE,
    MOVE_TRUNCATE,
    "ledger_epoch_move",
    "account.move"
);
source!(
    ITEM_INSERT,
    ITEM_UPDATE,
    ITEM_DELETE,
    ITEM_TRUNCATE,
    "ledger_epoch_item",
    "account.move.line"
);
source!(
    JOURNAL_INSERT,
    JOURNAL_UPDATE,
    JOURNAL_DELETE,
    JOURNAL_TRUNCATE,
    "ledger_epoch_journal",
    "account.journal"
);
source!(
    ACCOUNT_INSERT,
    ACCOUNT_UPDATE,
    ACCOUNT_DELETE,
    ACCOUNT_TRUNCATE,
    "ledger_epoch_account",
    "account.account"
);
source!(
    COMPANY_INSERT,
    COMPANY_UPDATE,
    COMPANY_DELETE,
    COMPANY_TRUNCATE,
    "ledger_epoch_company",
    "res.company"
);
source!(
    CURRENCY_INSERT,
    CURRENCY_UPDATE,
    CURRENCY_DELETE,
    CURRENCY_TRUNCATE,
    "ledger_epoch_currency",
    "res.currency"
);
source!(
    REPORT_INSERT,
    REPORT_UPDATE,
    REPORT_DELETE,
    REPORT_TRUNCATE,
    "ledger_epoch_report",
    "reporting.report"
);
source!(
    COLUMN_INSERT,
    COLUMN_UPDATE,
    COLUMN_DELETE,
    COLUMN_TRUNCATE,
    "ledger_epoch_column",
    "reporting.report.column"
);
source!(
    LINE_INSERT,
    LINE_UPDATE,
    LINE_DELETE,
    LINE_TRUNCATE,
    "ledger_epoch_line",
    "reporting.report.line"
);
