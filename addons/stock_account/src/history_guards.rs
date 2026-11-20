// SPDX-License-Identifier: MIT OR Apache-2.0
//! Database invariants independent of writer identity.
use erp_sdk::sql_objects::{
    SqlConditionDecl, SqlConditionKind, SqlDep, SqlEvent, SqlFunctionDecl, SqlLanguage, SqlLevel,
    SqlObjectDecl, SqlObjectManifest, SqlResult, SqlTiming, SqlTriggerDecl,
};

/// Inventory of required linked declarations, verified when composing this bridge.
pub static MANIFEST: SqlObjectManifest = SqlObjectManifest::new(
    "stock_account",
    &[
        "row_immutable",
        "receipt_final",
        "control_keys",
        "layer_original",
        "fn_history",
        "fn_receipt",
        "fn_control",
        "fn_layer",
        "source_update",
        "source_delete",
        "source_truncate",
        "alloc_update",
        "alloc_delete",
        "alloc_truncate",
        "journal_update",
        "journal_delete",
        "journal_truncate",
        "remainder_update",
        "remainder_delete",
        "remainder_truncate",
        "consumption_update",
        "consumption_delete",
        "consumption_truncate",
        "receipt_update",
        "receipt_delete",
        "receipt_truncate",
        "control_update",
        "control_delete",
        "control_truncate",
        "layer_update",
    ],
);

macro_rules! object {
    ($name:ident, $decl:expr) => {
        #[erp_sdk::sql_objects::linkme::distributed_slice(erp_sdk::sql_objects::SQL_OBJECT_DECLS)]
        #[linkme(crate = erp_sdk::sql_objects::linkme)]
        static $name: SqlObjectDecl = $decl;
    };
}

macro_rules! condition {
    ($name:ident, $code:literal, $key:literal, $msg:literal) => {
        object!(
            $name,
            SqlObjectDecl::condition(
                "stock_account",
                $key,
                SqlConditionDecl::new($code, concat!("stock_valuation_", $key), $msg)
                    .kind(SqlConditionKind::Programming)
            )
        );
    };
}
condition!(
    HISTORY,
    "ERP61",
    "row_immutable",
    "Valuation history cannot be changed after insertion."
);
condition!(
    RECEIPT,
    "ERP62",
    "receipt_final",
    "A valuation receipt can be finalized once and must keep its identity."
);
condition!(
    CONTROL,
    "ERP63",
    "control_keys",
    "A valuation control row must keep its identity and its counters may only advance."
);
condition!(
    LAYER,
    "ERP64",
    "layer_original",
    "An existing valuation layer must keep its identity and original receipt values."
);

macro_rules! function {
    ($name:ident, $key:literal, $condition:literal, $sql:literal) => {
        object!(
            $name,
            SqlObjectDecl::function(
                "stock_account",
                $key,
                SqlFunctionDecl::new(
                    concat!("stk_val_", $key),
                    SqlLanguage::PlPgSql,
                    SqlResult::Trigger,
                    $sql
                )
                .deps(&[SqlDep::Object {
                    addon: "stock_account",
                    key: $condition
                }])
            )
        );
    };
}

// A TRUNCATE trigger has no transition relation. Keep its unconditional refusal in its own statement.
function!(
    HISTORY_FN,
    "fn_history",
    "row_immutable",
    r#"
BEGIN
    IF TG_OP = 'TRUNCATE' THEN
        RAISE EXCEPTION 'immutable valuation history' USING ERRCODE='ERP61', CONSTRAINT='stock_valuation_row_immutable';
    ELSIF EXISTS (SELECT FROM old_rows) THEN
        RAISE EXCEPTION 'immutable valuation history' USING ERRCODE='ERP61', CONSTRAINT='stock_valuation_row_immutable';
    END IF;
    RETURN NULL;
END
"#
);
function!(
    RECEIPT_FN,
    "fn_receipt",
    "receipt_final",
    r#"
BEGIN
    IF TG_OP = 'TRUNCATE' THEN
        RAISE EXCEPTION 'immutable valuation receipt' USING ERRCODE='ERP62', CONSTRAINT='stock_valuation_receipt_final';
    ELSIF TG_OP = 'DELETE' THEN
        IF EXISTS (SELECT FROM old_rows) THEN
            RAISE EXCEPTION 'immutable valuation receipt' USING ERRCODE='ERP62', CONSTRAINT='stock_valuation_receipt_final';
        END IF;
    ELSIF EXISTS (
        SELECT FROM old_rows o LEFT JOIN new_rows n USING(id)
        WHERE n.id IS NULL OR o.state <> 'pending' OR n.state <> 'applied'
           OR n.applied_mode NOT IN ('inline', 'replay')
           OR (n.source_id,n.company_id,n.product_id,n.recorded_at) IS DISTINCT FROM
              (o.source_id,o.company_id,o.product_id,o.recorded_at)
    ) THEN
        RAISE EXCEPTION 'invalid valuation receipt transition' USING ERRCODE='ERP62', CONSTRAINT='stock_valuation_receipt_final';
    END IF;
    RETURN NULL;
END
"#
);
function!(
    CONTROL_FN,
    "fn_control",
    "control_keys",
    r#"
BEGIN
    IF TG_OP = 'TRUNCATE' THEN
        RAISE EXCEPTION 'permanent valuation control' USING ERRCODE='ERP63', CONSTRAINT='stock_valuation_control_keys';
    ELSIF TG_OP = 'DELETE' THEN
        IF EXISTS (SELECT FROM old_rows) THEN
            RAISE EXCEPTION 'permanent valuation control' USING ERRCODE='ERP63', CONSTRAINT='stock_valuation_control_keys';
        END IF;
    ELSIF EXISTS (
        SELECT FROM old_rows o LEFT JOIN new_rows n USING(id)
        WHERE n.id IS NULL OR (n.company_id,n.product_id,n.recorded_at) IS DISTINCT FROM (o.company_id,o.product_id,o.recorded_at)
           OR n.worker_epoch < o.worker_epoch OR n.seal_revision < o.seal_revision OR n.publication_generation < o.publication_generation
    ) THEN
        RAISE EXCEPTION 'invalid valuation control change' USING ERRCODE='ERP63', CONSTRAINT='stock_valuation_control_keys';
    END IF;
    RETURN NULL;
END
"#
);
function!(
    LAYER_FN,
    "fn_layer",
    "layer_original",
    r#"
BEGIN
    IF EXISTS (
        SELECT FROM old_rows o LEFT JOIN new_rows n USING(id)
        WHERE n.id IS NULL OR
            (n.company_id,n.product_id,n.lot_id,n.source_id,n.kind,n.effective_time,
             n.original_quantity,n.original_value,n.shortage_source_id,n.shortage_policy)
            IS DISTINCT FROM
            (o.company_id,o.product_id,o.lot_id,o.source_id,o.kind,o.effective_time,
             o.original_quantity,o.original_value,o.shortage_source_id,o.shortage_policy)
    ) THEN
        RAISE EXCEPTION 'valuation layer original changed' USING ERRCODE='ERP64', CONSTRAINT='stock_valuation_layer_original';
    END IF;
    RETURN NULL;
END
"#
);

macro_rules! update {
    ($symbol:ident, $table:literal, $model:literal, $function:literal) => {
        object!(
            $symbol,
            SqlObjectDecl::trigger(
                "stock_account",
                concat!($table, "_update"),
                SqlTriggerDecl::new(
                    concat!("stk_val_", $table, "_update"),
                    $model,
                    ("stock_account", $function),
                    SqlTiming::After,
                    &[SqlEvent::Update],
                    SqlLevel::Statement
                )
                .old_table("old_rows")
                .new_table("new_rows")
            )
        );
    };
}
macro_rules! permanent {
    ($module:ident, $table:literal, $model:literal, $function:literal) => {
        mod $module {
            use super::*;
            update!(UPDATE, $table, $model, $function);
            object!(
                DELETE,
                SqlObjectDecl::trigger(
                    "stock_account",
                    concat!($table, "_delete"),
                    SqlTriggerDecl::new(
                        concat!("stk_val_", $table, "_delete"),
                        $model,
                        ("stock_account", $function),
                        SqlTiming::After,
                        &[SqlEvent::Delete],
                        SqlLevel::Statement
                    )
                    .old_table("old_rows")
                )
            );
            object!(
                TRUNCATE,
                SqlObjectDecl::trigger(
                    "stock_account",
                    concat!($table, "_truncate"),
                    SqlTriggerDecl::new(
                        concat!("stk_val_", $table, "_truncate"),
                        $model,
                        ("stock_account", $function),
                        SqlTiming::Before,
                        &[SqlEvent::Truncate],
                        SqlLevel::Statement
                    )
                )
            );
        }
    };
}
permanent!(source, "source", "stock.valuation.source", "fn_history");
permanent!(alloc, "alloc", "stock.valuation.source.alloc", "fn_history");
permanent!(journal, "journal", "stock.valuation.journal", "fn_history");
permanent!(
    remainder,
    "remainder",
    "stock.valuation.remainder",
    "fn_history"
);
permanent!(
    consumption,
    "consumption",
    "stock.valuation.layer.consumption",
    "fn_history"
);
permanent!(receipt, "receipt", "stock.valuation.receipt", "fn_receipt");
permanent!(control, "control", "stock.valuation.control", "fn_control");
update!(LAYER_UPDATE, "layer", "stock.valuation.layer", "fn_layer");
