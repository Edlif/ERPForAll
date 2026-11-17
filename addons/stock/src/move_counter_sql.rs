// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SQL objects that keep the transfer's counter vector and state (3,
//! lead conditions M1 to M3). Listed in [`crate::schema_guards::MANIFEST`].
//!
//! `stock_move_counts`: AFTER INSERT, UPDATE and DELETE statement triggers on `stock_move` that
//!   add each statement's net contribution per transfer, locking the transfers ascending;
//! - `stock_picking_eval_state`: AFTER INSERT and UPDATE statement triggers on `stock_picking`
//!   that store the evaluated state wherever it differs, so a counter change, a shipping-policy
//!   change and a source-location change are all re-evaluated in their own statement;
//! - `stock_guard_location_bypass`: a location may not switch between a reserving and a
//!   non-reserving usage while a transfer that leaves from it is open (the refusal
//!   path chosen for the source-usage change: no location-to-transfer lock inversion).
use erp_sdk::sql_objects::{
    SqlConditionDecl, SqlConditionKind, SqlDep, SqlEvent, SqlFunctionDecl, SqlLanguage, SqlLevel,
    SqlObjectDecl, SqlResult, SqlTiming, SqlTriggerDecl,
};

use crate::schema_guards::object;

object!(
    BYPASS_CONDITION,
    SqlObjectDecl::condition(
        "stock",
        "location_bypass_open_transfers",
        SqlConditionDecl::new(
            "ERP04",
            "stock_location_bypass_open_transfers",
            "The location type cannot switch between a type that reserves stock and one that \
             does not while transfers leaving from it are open."
        )
        .kind(SqlConditionKind::Validation)
    )
);

/// The transfer state from its counter vector (amendment 2026-11-17): the one copy of
/// the state expression, rendered as SQL text for the operands each caller names, in this order:
/// moves, draft, waiting, confirmed, partially available, assigned, done, cancelled, done scrap,
/// cancelled scrap, assigned with zero demand, open with non-zero demand, confirmed with positive
/// demand, non-scrap make-to-order, shipping policy (text), the source location bypasses
/// reservation (boolean). Not a database function: the invoker would need an EXECUTE grant to the
/// request role, and role bindings are not deployed (Proposed).
/// shortcut: operands are pasted unparenthesized, so each must be atomic (a column, a cast or a
/// function call); parenthesize them here if a caller ever needs a compound operand.
#[rustfmt::skip] // one SQL CASE arm per source line
macro_rules! picking_state_sql {
    ($moves:literal, $draft:literal, $waiting:literal, $confirmed:literal, $partial:literal,
     $assigned:literal, $done:literal, $cancel:literal, $done_scrap:literal,
     $cancel_scrap:literal, $assigned_zero:literal, $open_demand:literal,
     $confirmed_demand:literal, $mto:literal, $move_type:literal, $bypass:literal) => {
        concat!(
            "CASE\n",
            "    WHEN ", $moves, " = 0 OR ", $draft, " > 0 THEN 'draft'\n",
            "    WHEN ", $cancel, " = ", $moves, " THEN 'cancel'\n",
            "    WHEN ", $done, " + ", $cancel, " = ", $moves, " THEN CASE WHEN ", $done, " = ",
            $done_scrap, " AND ", $cancel, " > ", $cancel_scrap, " THEN 'cancel' ELSE 'done' END\n",
            "    WHEN ", $bypass, " AND ", $mto, " = 0 THEN 'assigned'\n",
            "    WHEN ", $waiting, " + ", $confirmed, " + ", $partial, " + ", $assigned, " - ",
            $assigned_zero, " = 0 THEN 'assigned'\n",
            "    WHEN ", $move_type, " = 'one' THEN CASE\n",
            "        WHEN ", $open_demand, " = 0 THEN 'assigned'\n",
            "        WHEN ", $confirmed, " > 0 OR ", $partial, " > 0 THEN 'confirmed'\n",
            "        WHEN ", $waiting, " > 0 THEN 'waiting'\n",
            "        ELSE 'assigned'\n",
            "    END\n",
            "    WHEN ", $waiting, " + ", $confirmed, " + ", $partial, " > 0 AND ", $partial, " + ",
            $assigned, " - ", $assigned_zero, " > 0 THEN 'assigned'\n",
            "    WHEN ", $waiting, " + ", $confirmed, " + ", $partial, " = 0 THEN 'assigned'\n",
            "    WHEN ", $waiting, " > 0 THEN 'waiting'\n",
            "    WHEN ", $confirmed_demand, " > 0 THEN 'confirmed'\n",
            "    WHEN ", $confirmed, " > ", $open_demand, " THEN 'assigned'\n",
            "    ELSE 'confirmed'\n",
            "END"
        )
    };
}

object!(
    COUNTS_FUNCTION,
    SqlObjectDecl::function(
        "stock",
        "fn_move_counts",
        SqlFunctionDecl::new(
            "stock_move_counts",
            SqlLanguage::PlPgSql,
            SqlResult::Trigger,
            r#"
DECLARE
    v_picking int8[];
    v_sign int4[];
    v_state text[];
    v_demand numeric[];
    v_scrap boolean[];
    v_method text[];
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT array_agg(n.picking_id), array_agg(1), array_agg(n.state::text),
               array_agg(coalesce(n.product_uom_qty, 0)), array_agg(coalesce(n.is_scrap, false)),
               array_agg(n.procure_method::text)
          INTO v_picking, v_sign, v_state, v_demand, v_scrap, v_method
          FROM new_rows n
         WHERE n.picking_id IS NOT NULL;
    ELSIF TG_OP = 'DELETE' THEN
        SELECT array_agg(o.picking_id), array_agg(-1), array_agg(o.state::text),
               array_agg(coalesce(o.product_uom_qty, 0)), array_agg(coalesce(o.is_scrap, false)),
               array_agg(o.procure_method::text)
          INTO v_picking, v_sign, v_state, v_demand, v_scrap, v_method
          FROM old_rows o
         WHERE o.picking_id IS NOT NULL;
    ELSE
        SELECT array_agg(r.picking_id), array_agg(r.sign), array_agg(r.state),
               array_agg(r.demand), array_agg(r.scrap), array_agg(r.method)
          INTO v_picking, v_sign, v_state, v_demand, v_scrap, v_method
          FROM (
                SELECT x.picking_id, x.sign, x.state::text AS state,
                       coalesce(x.product_uom_qty, 0) AS demand,
                       coalesce(x.is_scrap, false) AS scrap, x.procure_method::text AS method
                  FROM (
                        SELECT o.picking_id, -1 AS sign, o.state, o.product_uom_qty, o.is_scrap,
                               o.procure_method
                          FROM old_rows o JOIN new_rows n ON n.id = o.id
                         WHERE (o.picking_id, o.state, o.product_uom_qty, o.is_scrap, o.procure_method)
                               IS DISTINCT FROM
                               (n.picking_id, n.state, n.product_uom_qty, n.is_scrap, n.procure_method)
                        UNION ALL
                        SELECT n.picking_id, 1, n.state, n.product_uom_qty, n.is_scrap,
                               n.procure_method
                          FROM old_rows o JOIN new_rows n ON n.id = o.id
                         WHERE (o.picking_id, o.state, o.product_uom_qty, o.is_scrap, o.procure_method)
                               IS DISTINCT FROM
                               (n.picking_id, n.state, n.product_uom_qty, n.is_scrap, n.procure_method)
                       ) x
                 WHERE x.picking_id IS NOT NULL
               ) r;
    END IF;
    IF v_picking IS NULL THEN
        RETURN NULL;
    END IF;
    -- the class-2 root order; a typed writer already holds these locks
    PERFORM 1 FROM public.stock_picking WHERE id = ANY(v_picking) ORDER BY id FOR NO KEY UPDATE;
    UPDATE public.stock_picking t SET
        move_count = t.move_count + d.c_move,
        draft_count = t.draft_count + d.c_draft,
        waiting_count = t.waiting_count + d.c_waiting,
        confirmed_count = t.confirmed_count + d.c_confirmed,
        partially_available_count = t.partially_available_count + d.c_partial,
        assigned_count = t.assigned_count + d.c_assigned,
        done_count = t.done_count + d.c_done,
        cancel_count = t.cancel_count + d.c_cancel,
        done_scrap_count = t.done_scrap_count + d.c_done_scrap,
        cancel_scrap_count = t.cancel_scrap_count + d.c_cancel_scrap,
        assigned_zero_demand_count = t.assigned_zero_demand_count + d.c_assigned_zero,
        open_positive_demand_count = t.open_positive_demand_count + d.c_open_demand,
        confirmed_positive_demand_count = t.confirmed_positive_demand_count + d.c_confirmed_demand,
        mto_nonscrap_count = t.mto_nonscrap_count + d.c_mto
      FROM (
            SELECT r.picking_id,
                   sum(r.sign) AS c_move,
                   sum(CASE WHEN r.state = 'draft' THEN r.sign ELSE 0 END) AS c_draft,
                   sum(CASE WHEN r.state = 'waiting' THEN r.sign ELSE 0 END) AS c_waiting,
                   sum(CASE WHEN r.state = 'confirmed' THEN r.sign ELSE 0 END) AS c_confirmed,
                   sum(CASE WHEN r.state = 'partially_available' THEN r.sign ELSE 0 END) AS c_partial,
                   sum(CASE WHEN r.state = 'assigned' THEN r.sign ELSE 0 END) AS c_assigned,
                   sum(CASE WHEN r.state = 'done' THEN r.sign ELSE 0 END) AS c_done,
                   sum(CASE WHEN r.state = 'cancel' THEN r.sign ELSE 0 END) AS c_cancel,
                   sum(CASE WHEN r.state = 'done' AND r.scrap THEN r.sign ELSE 0 END) AS c_done_scrap,
                   sum(CASE WHEN r.state = 'cancel' AND r.scrap THEN r.sign ELSE 0 END) AS c_cancel_scrap,
                   sum(CASE WHEN r.state = 'assigned' AND r.demand = 0 THEN r.sign ELSE 0 END) AS c_assigned_zero,
                   sum(CASE WHEN r.state NOT IN ('done', 'cancel') AND r.demand <> 0 THEN r.sign ELSE 0 END) AS c_open_demand,
                   sum(CASE WHEN r.state = 'confirmed' AND r.demand > 0 THEN r.sign ELSE 0 END) AS c_confirmed_demand,
                   sum(CASE WHEN NOT r.scrap AND r.method = 'make_to_order' THEN r.sign ELSE 0 END) AS c_mto
              FROM unnest(v_picking, v_sign, v_state, v_demand, v_scrap, v_method)
                   AS r(picking_id, sign, state, demand, scrap, method)
             GROUP BY r.picking_id
           ) d
     WHERE t.id = d.picking_id
       AND (d.c_move, d.c_draft, d.c_waiting, d.c_confirmed, d.c_partial, d.c_assigned, d.c_done,
            d.c_cancel, d.c_done_scrap, d.c_cancel_scrap, d.c_assigned_zero, d.c_open_demand,
            d.c_confirmed_demand, d.c_mto)
           <> (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    RETURN NULL;
END
"#
        )
        .deps(&[SqlDep::Model("stock.move"), SqlDep::Model("stock.picking")])
    )
);

object!(
    EVAL_FUNCTION,
    SqlObjectDecl::function(
        "stock",
        "fn_picking_eval_state",
        SqlFunctionDecl::new(
            "stock_picking_eval_state",
            SqlLanguage::PlPgSql,
            SqlResult::Trigger,
            concat!(
                r#"
BEGIN
    -- the state update below fires this trigger again; an empty statement ends the chain
    IF NOT EXISTS (SELECT 1 FROM new_rows) THEN
        RETURN NULL;
    END IF;
    UPDATE public.stock_picking t
       SET state = s.state
      FROM (
            SELECT p.id, "#,
                picking_state_sql!(
                    "p.move_count", "p.draft_count", "p.waiting_count", "p.confirmed_count",
                    "p.partially_available_count", "p.assigned_count", "p.done_count",
                    "p.cancel_count", "p.done_scrap_count", "p.cancel_scrap_count",
                    "p.assigned_zero_demand_count", "p.open_positive_demand_count",
                    "p.confirmed_positive_demand_count", "p.mto_nonscrap_count",
                    "p.move_type::text",
                    "coalesce(l.usage IN ('supplier', 'customer', 'inventory', 'production'), false)"
                ),
                r#" AS state
              FROM new_rows n
              JOIN public.stock_picking p ON p.id = n.id
              JOIN public.stock_location l ON l.id = p.location_id
           ) s
     WHERE t.id = s.id AND t.state IS DISTINCT FROM s.state;
    RETURN NULL;
END
"#
            )
        )
        .deps(&[
            SqlDep::Model("stock.picking"),
            SqlDep::Model("stock.location"),
        ])
    )
);

object!(
    BYPASS_FUNCTION,
    SqlObjectDecl::function(
        "stock",
        "fn_location_bypass",
        SqlFunctionDecl::new(
            "stock_guard_location_bypass",
            SqlLanguage::PlPgSql,
            SqlResult::Trigger,
            r#"
BEGIN
    -- a transfer created or re-sourced through the ORM holds its source location shared before
    -- its row is written, so it either waits on this lock and is retried, or made this statement
    -- fail its own lock first; a transfer committed between this transaction's snapshot and this
    -- lock is not seen below (its state is evaluated again at its next update)
    PERFORM 1 FROM public.stock_location l
      JOIN old_rows o ON o.id = l.id
      JOIN new_rows n ON n.id = o.id
     WHERE coalesce(o.usage IN ('supplier', 'customer', 'inventory', 'production'), false)
           <> coalesce(n.usage IN ('supplier', 'customer', 'inventory', 'production'), false)
     ORDER BY l.id FOR UPDATE OF l;
    IF EXISTS (
        SELECT 1 FROM old_rows o
          JOIN new_rows n ON n.id = o.id
          JOIN public.stock_picking p ON p.location_id = n.id
         WHERE coalesce(o.usage IN ('supplier', 'customer', 'inventory', 'production'), false)
               <> coalesce(n.usage IN ('supplier', 'customer', 'inventory', 'production'), false)
           AND p.state NOT IN ('done', 'cancel')
    ) THEN
        RAISE EXCEPTION 'reservation bypass changed under open transfers'
            USING ERRCODE = 'ERP04', CONSTRAINT = 'stock_location_bypass_open_transfers';
    END IF;
    RETURN NULL;
END
"#
        )
        .deps(&[
            SqlDep::Object {
                addon: "stock",
                key: "location_bypass_open_transfers"
            },
            SqlDep::Model("stock.location"),
            SqlDep::Model("stock.picking"),
        ])
    )
);

const fn after(
    name: &'static str,
    model: &'static str,
    function: &'static str,
    event: &'static [SqlEvent],
) -> SqlTriggerDecl {
    SqlTriggerDecl::new(
        name,
        model,
        ("stock", function),
        SqlTiming::After,
        event,
        SqlLevel::Statement,
    )
}

object!(
    MOVE_COUNTS_INSERT,
    SqlObjectDecl::trigger(
        "stock",
        "move_counts_insert",
        after(
            "stock_move_counts_insert",
            "stock.move",
            "fn_move_counts",
            &[SqlEvent::Insert]
        )
        .new_table("new_rows")
    )
);
object!(
    MOVE_COUNTS_UPDATE,
    SqlObjectDecl::trigger(
        "stock",
        "move_counts_update",
        after(
            "stock_move_counts_update",
            "stock.move",
            "fn_move_counts",
            &[SqlEvent::Update]
        )
        .old_table("old_rows")
        .new_table("new_rows")
    )
);
object!(
    MOVE_COUNTS_DELETE,
    SqlObjectDecl::trigger(
        "stock",
        "move_counts_delete",
        after(
            "stock_move_counts_delete",
            "stock.move",
            "fn_move_counts",
            &[SqlEvent::Delete]
        )
        .old_table("old_rows")
    )
);
object!(
    PICKING_STATE_INSERT,
    SqlObjectDecl::trigger(
        "stock",
        "picking_state_insert",
        after(
            "stock_picking_state_insert",
            "stock.picking",
            "fn_picking_eval_state",
            &[SqlEvent::Insert]
        )
        .new_table("new_rows")
    )
);
object!(
    PICKING_STATE_UPDATE,
    SqlObjectDecl::trigger(
        "stock",
        "picking_state_update",
        after(
            "stock_picking_state_update",
            "stock.picking",
            "fn_picking_eval_state",
            &[SqlEvent::Update]
        )
        .old_table("old_rows")
        .new_table("new_rows")
    )
);
object!(
    LOCATION_BYPASS_UPDATE,
    SqlObjectDecl::trigger(
        "stock",
        "location_bypass_update",
        after(
            "stock_location_bypass_guard",
            "stock.location",
            "fn_location_bypass",
            &[SqlEvent::Update]
        )
        .old_table("old_rows")
        .new_table("new_rows")
    )
);

#[cfg(test)]
mod tests {
    fn words(sql: &str) -> String {
        sql.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The upgrade backfill cannot render Rust, so it carries the expression as text: it must be
    /// the macro's rendering for its operands, or the backfill and the triggers decide differently.
    #[test]
    fn backfill_state_expression_is_the_rendered_one() {
        let migration = include_str!("../migrations/1.6/post-move-counters.sql");
        let rendered = picking_state_sql!(
            "f.c_move",
            "f.c_draft",
            "f.c_waiting",
            "f.c_confirmed",
            "f.c_partial",
            "f.c_assigned",
            "f.c_done",
            "f.c_cancel",
            "f.c_done_scrap",
            "f.c_cancel_scrap",
            "f.c_assigned_zero",
            "f.c_open_demand",
            "f.c_confirmed_demand",
            "f.c_mto",
            "p.move_type::text",
            "coalesce(l.usage IN ('supplier', 'customer', 'inventory', 'production'), false)"
        );
        let expected = format!("SELECT f.*, {} AS c_state", words(rendered));
        assert!(
            words(migration).contains(&expected),
            "migration 1.6 no longer holds the rendered state expression:\n{expected}"
        );
    }
}
