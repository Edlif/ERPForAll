// SPDX-License-Identifier: MIT OR Apache-2.0
//! Preserve the primary costing method on keys with immutable valuation sources.
use super::*;

object!(
    TRANSITION,
    SqlObjectDecl::condition(
        "stock_account",
        "cost_method_transition",
        SqlConditionDecl::new(
            "ERP67",
            "stock_valuation_cost_method_transition",
            "A costing method cannot change after valuation starts. Keep the current method until dated costing transitions are available.",
        ).kind(SqlConditionKind::Validation)
    )
);

object!(
    DEFAULT_SCOPE,
    SqlObjectDecl::condition(
        "stock_account",
        "cost_method_default_scope",
        SqlConditionDecl::new(
            "ERP68",
            "stock_valuation_cost_method_default_scope",
            "Set a supported default costing method on the company record.",
        )
        .kind(SqlConditionKind::Validation)
    )
);

macro_rules! policy_function {
    ($symbol:ident, $key:literal, $sql:expr) => {
        object!(
            $symbol,
            SqlObjectDecl::function(
                "stock_account",
                $key,
                SqlFunctionDecl::new(
                    concat!("stk_val_", $key),
                    SqlLanguage::PlPgSql,
                    SqlResult::Trigger,
                    $sql
                )
                .deps(&[
                    SqlDep::Object {
                        addon: "stock_account",
                        key: "cost_method_transition"
                    },
                    SqlDep::Object {
                        addon: "stock_account",
                        key: "cost_method_default_scope"
                    },
                    SqlDep::Model("stock.valuation.source"),
                    SqlDep::Model("product.category"),
                    SqlDep::Model("product.template"),
                    SqlDep::Model("product.product"),
                    SqlDep::Model("ir.default"),
                    SqlDep::Model("ir.model.fields"),
                ])
            )
        );
    };
}

policy_function!(
    CATEGORY_POLICY,
    "fn_cost_category",
    r#"
BEGIN
    IF EXISTS (
        SELECT FROM old_rows o JOIN new_rows n USING(id)
        JOIN public.product_template p ON p.categ_id=n.id
        JOIN public.product_product v ON v.product_tmpl_id=p.id
        JOIN public.stk_value_source s ON s.product_id=v.id AND s.cost_partition=''
        CROSS JOIN LATERAL (SELECT COALESCE(p.company_id, s.company_id) AS company_id) policy
        LEFT JOIN LATERAL (
            SELECT d.json_value::jsonb #>> '{}' AS method
            FROM public.ir_default d JOIN public.ir_model_fields f ON f.id=d.field_id
            WHERE f.model='product.category' AND f.name='property_cost_method'
                AND d.company_id=policy.company_id AND d.user_id IS NULL AND d.condition IS NULL
            ORDER BY d.id LIMIT 1
        ) fallback ON true
        WHERE o.property_cost_method IS DISTINCT FROM n.property_cost_method
            AND COALESCE(o.property_cost_method ->> policy.company_id::text, fallback.method, 'standard')
                IS DISTINCT FROM
                COALESCE(n.property_cost_method ->> policy.company_id::text, fallback.method, 'standard')
    ) THEN
        RAISE EXCEPTION 'existing valuation blocks costing method change' USING ERRCODE='ERP67', CONSTRAINT='stock_valuation_cost_method_transition';
    END IF;
    RETURN NULL;
END
"#
);

macro_rules! paired_defaults {
    () => {
        r#"        LEFT JOIN LATERAL (
            SELECT d.json_value::jsonb #>> '{}' AS method
            FROM public.ir_default d JOIN public.ir_model_fields f ON f.id=d.field_id
            WHERE f.model='product.category' AND f.name='property_cost_method'
                AND d.company_id=policy.old_company AND d.user_id IS NULL AND d.condition IS NULL
            ORDER BY d.id LIMIT 1
        ) old_default ON true
        LEFT JOIN LATERAL (
            SELECT d.json_value::jsonb #>> '{}' AS method
            FROM public.ir_default d JOIN public.ir_model_fields f ON f.id=d.field_id
            WHERE f.model='product.category' AND f.name='property_cost_method'
                AND d.company_id=policy.new_company AND d.user_id IS NULL AND d.condition IS NULL
            ORDER BY d.id LIMIT 1
        ) new_default ON true
"#
    };
}

policy_function!(
    PRODUCT_POLICY,
    "fn_cost_product",
    concat!(
        r#"
BEGIN
    IF EXISTS (
        SELECT FROM old_rows o JOIN new_rows n USING(id)
        JOIN public.product_product v ON v.product_tmpl_id=n.id
        JOIN public.stk_value_source s ON s.product_id=v.id AND s.cost_partition=''
        LEFT JOIN public.product_category oc ON oc.id=o.categ_id
        LEFT JOIN public.product_category nc ON nc.id=n.categ_id
        CROSS JOIN LATERAL (
            SELECT COALESCE(o.company_id, s.company_id) AS old_company,
                   COALESCE(n.company_id, s.company_id) AS new_company
        ) policy
"#,
        paired_defaults!(),
        r#"        WHERE (o.categ_id, o.company_id) IS DISTINCT FROM (n.categ_id, n.company_id)
            AND COALESCE(oc.property_cost_method ->> policy.old_company::text, old_default.method, 'standard')
                IS DISTINCT FROM
                COALESCE(nc.property_cost_method ->> policy.new_company::text, new_default.method, 'standard')
    ) THEN
        RAISE EXCEPTION 'existing valuation blocks costing method change' USING ERRCODE='ERP67', CONSTRAINT='stock_valuation_cost_method_transition';
    END IF;
    RETURN NULL;
END
"#
    )
);

policy_function!(
    VARIANT_POLICY,
    "fn_cost_variant",
    concat!(
        r#"
BEGIN
    IF EXISTS (
        SELECT FROM old_rows o JOIN new_rows n USING(id)
        JOIN public.product_template ot ON ot.id=o.product_tmpl_id
        JOIN public.product_template nt ON nt.id=n.product_tmpl_id
        JOIN public.stk_value_source s ON s.product_id=n.id AND s.cost_partition=''
        LEFT JOIN public.product_category oc ON oc.id=ot.categ_id
        LEFT JOIN public.product_category nc ON nc.id=nt.categ_id
        CROSS JOIN LATERAL (
            SELECT COALESCE(ot.company_id, s.company_id) AS old_company,
                   COALESCE(nt.company_id, s.company_id) AS new_company
        ) policy
"#,
        paired_defaults!(),
        r#"        WHERE o.product_tmpl_id IS DISTINCT FROM n.product_tmpl_id
            AND COALESCE(oc.property_cost_method ->> policy.old_company::text, old_default.method, 'standard')
                IS DISTINCT FROM
                COALESCE(nc.property_cost_method ->> policy.new_company::text, new_default.method, 'standard')
    ) THEN
        RAISE EXCEPTION 'existing valuation blocks costing method change' USING ERRCODE='ERP67', CONSTRAINT='stock_valuation_cost_method_transition';
    END IF;
    RETURN NULL;
END
"#
    )
);

// Inspect OLD before foreign-key actions clear the template category or cascade to children.
policy_function!(
    CATEGORY_DELETE_POLICY,
    "fn_cost_category_delete",
    r#"
BEGIN
    IF EXISTS (
        SELECT FROM public.product_template p
        JOIN public.product_product v ON v.product_tmpl_id=p.id
        JOIN public.stk_value_source s ON s.product_id=v.id AND s.cost_partition=''
        CROSS JOIN LATERAL (SELECT COALESCE(p.company_id,s.company_id) AS company_id) policy
        LEFT JOIN LATERAL (
            SELECT d.json_value::jsonb #>> '{}' AS method
            FROM public.ir_default d JOIN public.ir_model_fields f ON f.id=d.field_id
            WHERE f.model='product.category' AND f.name='property_cost_method'
                AND d.company_id=policy.company_id AND d.user_id IS NULL AND d.condition IS NULL
            ORDER BY d.id LIMIT 1
        ) fallback ON true
        WHERE p.categ_id=OLD.id AND OLD.property_cost_method ->> policy.company_id::text IS NOT NULL
            AND OLD.property_cost_method ->> policy.company_id::text
                IS DISTINCT FROM COALESCE(fallback.method, 'standard')
    ) THEN
        RAISE EXCEPTION 'existing valuation blocks costing method change' USING ERRCODE='ERP67', CONSTRAINT='stock_valuation_cost_method_transition';
    END IF;
    RETURN OLD;
END
"#
);

object!(
    CATEGORY_DELETE,
    SqlObjectDecl::trigger(
        "stock_account",
        "cost_category_delete",
        SqlTriggerDecl::new(
            "stk_val_cost_category_delete",
            "product.category",
            ("stock_account", "fn_cost_category_delete"),
            SqlTiming::Before,
            &[SqlEvent::Delete],
            SqlLevel::Row,
        )
    )
);
update!(
    VARIANT_UPDATE,
    "cost_variant",
    "product.product",
    "fn_cost_variant"
);

policy_function!(
    COMPANY_POLICY,
    "fn_cost_default",
    r#"
DECLARE
    previous jsonb := '[]';
    proposed jsonb := '[]';
BEGIN
    IF TG_OP <> 'INSERT' THEN
        SELECT COALESCE(jsonb_agg(jsonb_build_object('company_id', d.company_id, 'method', d.json_value::jsonb #>> '{}')), '[]')
        INTO previous FROM old_rows d JOIN public.ir_model_fields f ON f.id=d.field_id
        WHERE f.model='product.category' AND f.name='property_cost_method';
    END IF;
    IF TG_OP <> 'DELETE' THEN
        IF EXISTS (
            SELECT FROM new_rows d JOIN public.ir_model_fields f ON f.id=d.field_id
            WHERE f.model='product.category' AND f.name='property_cost_method'
                AND (d.company_id IS NULL OR d.user_id IS NOT NULL OR d.condition IS NOT NULL
                    OR (d.json_value::jsonb #>> '{}') IS NULL
                    OR (d.json_value::jsonb #>> '{}') NOT IN ('standard','average','fifo'))
        ) THEN
            RAISE EXCEPTION 'cost method default scope' USING ERRCODE='ERP68', CONSTRAINT='stock_valuation_cost_method_default_scope';
        END IF;
        SELECT COALESCE(jsonb_agg(jsonb_build_object('company_id', d.company_id, 'method', d.json_value::jsonb #>> '{}')), '[]')
        INTO proposed FROM new_rows d JOIN public.ir_model_fields f ON f.id=d.field_id
        WHERE f.model='product.category' AND f.name='property_cost_method';
    END IF;
    IF previous=proposed THEN RETURN NULL; END IF;
    IF EXISTS (
        WITH changed AS (
            SELECT COALESCE(o.company_id, n.company_id) AS company_id
            FROM jsonb_to_recordset(previous) AS o(company_id bigint, method text)
            FULL JOIN jsonb_to_recordset(proposed) AS n(company_id bigint, method text) USING(company_id)
            WHERE COALESCE(o.method, 'standard') IS DISTINCT FROM n.method
        )
        SELECT FROM changed k
        JOIN public.stk_value_source s ON s.cost_partition=''
        JOIN public.product_product v ON v.id=s.product_id
        JOIN public.product_template p ON p.id=v.product_tmpl_id
            AND COALESCE(p.company_id, s.company_id)=k.company_id
        LEFT JOIN public.product_category c ON c.id=p.categ_id
        WHERE c.property_cost_method ->> k.company_id::text IS NULL
    ) THEN
        RAISE EXCEPTION 'existing valuation blocks costing method change' USING ERRCODE='ERP67', CONSTRAINT='stock_valuation_cost_method_transition';
    END IF;
    RETURN NULL;
END
"#
);

update!(
    CATEGORY_UPDATE,
    "cost_category",
    "product.category",
    "fn_cost_category"
);
update!(
    PRODUCT_UPDATE,
    "cost_product",
    "product.template",
    "fn_cost_product"
);
update!(
    DEFAULT_UPDATE,
    "cost_default",
    "ir.default",
    "fn_cost_default"
);
macro_rules! default_trigger {
    ($symbol:ident, $event:ident, $name:literal, $transition:ident, $table:literal) => {
        object!(
            $symbol,
            SqlObjectDecl::trigger(
                "stock_account",
                $name,
                SqlTriggerDecl::new(
                    concat!("stk_val_", $name),
                    "ir.default",
                    ("stock_account", "fn_cost_default"),
                    SqlTiming::After,
                    &[SqlEvent::$event],
                    SqlLevel::Statement,
                )
                .$transition($table)
            )
        );
    };
}
default_trigger!(
    DEFAULT_INSERT,
    Insert,
    "cost_default_insert",
    new_table,
    "new_rows"
);
default_trigger!(
    DEFAULT_DELETE,
    Delete,
    "cost_default_delete",
    old_table,
    "old_rows"
);
