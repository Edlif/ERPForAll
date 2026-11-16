-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Fill each transfer's move counter vector and state from its moves (lead condition
-- M3). The counter triggers may already exist when this runs, so the vector is SET from a fold of
-- the moves, never added to, and a row already equal to its fold is not written: a second run
-- changes nothing. One statement fills every transfer; the state comes from the same expression the
-- triggers use. The lock keeps writers out while the fill runs, but the upgrade transaction's
-- REPEATABLE READ snapshot predates it: a move an older node committed during the upgrade is not in
-- the fold. Stop older nodes first ("Inventory move counters and
-- references"); otherwise run this file once more after the upgrade, in one READ COMMITTED
-- transaction (psql --single-transaction), where the snapshot follows the lock.
LOCK TABLE public.stock_picking, public.stock_move IN SHARE ROW EXCLUSIVE MODE;
WITH fold AS (
    SELECT p.id,
           count(m.id)::int4 AS c_move,
           count(m.id) FILTER (WHERE m.state = 'draft')::int4 AS c_draft,
           count(m.id) FILTER (WHERE m.state = 'waiting')::int4 AS c_waiting,
           count(m.id) FILTER (WHERE m.state = 'confirmed')::int4 AS c_confirmed,
           count(m.id) FILTER (WHERE m.state = 'partially_available')::int4 AS c_partial,
           count(m.id) FILTER (WHERE m.state = 'assigned')::int4 AS c_assigned,
           count(m.id) FILTER (WHERE m.state = 'done')::int4 AS c_done,
           count(m.id) FILTER (WHERE m.state = 'cancel')::int4 AS c_cancel,
           count(m.id) FILTER (WHERE m.state = 'done' AND coalesce(m.is_scrap, false))::int4 AS c_done_scrap,
           count(m.id) FILTER (WHERE m.state = 'cancel' AND coalesce(m.is_scrap, false))::int4 AS c_cancel_scrap,
           count(m.id) FILTER (WHERE m.state = 'assigned' AND coalesce(m.product_uom_qty, 0) = 0)::int4 AS c_assigned_zero,
           count(m.id) FILTER (WHERE m.state NOT IN ('done', 'cancel') AND coalesce(m.product_uom_qty, 0) <> 0)::int4 AS c_open_demand,
           count(m.id) FILTER (WHERE m.state = 'confirmed' AND coalesce(m.product_uom_qty, 0) > 0)::int4 AS c_confirmed_demand,
           count(m.id) FILTER (WHERE NOT coalesce(m.is_scrap, false) AND m.procure_method = 'make_to_order')::int4 AS c_mto
      FROM public.stock_picking p
      LEFT JOIN public.stock_move m ON m.picking_id = p.id
     GROUP BY p.id
), target AS (
    -- the state expression rendered by `picking_state_sql!` (addons/stock/src/move_counter_sql.rs);
    -- a test there checks this text equals its rendering for these operands
    SELECT f.*,
           CASE
               WHEN f.c_move = 0 OR f.c_draft > 0 THEN 'draft'
               WHEN f.c_cancel = f.c_move THEN 'cancel'
               WHEN f.c_done + f.c_cancel = f.c_move THEN CASE WHEN f.c_done = f.c_done_scrap AND f.c_cancel > f.c_cancel_scrap THEN 'cancel' ELSE 'done' END
               WHEN coalesce(l.usage IN ('supplier', 'customer', 'inventory', 'production'), false) AND f.c_mto = 0 THEN 'assigned'
               WHEN f.c_waiting + f.c_confirmed + f.c_partial + f.c_assigned - f.c_assigned_zero = 0 THEN 'assigned'
               WHEN p.move_type::text = 'one' THEN CASE
                   WHEN f.c_open_demand = 0 THEN 'assigned'
                   WHEN f.c_confirmed > 0 OR f.c_partial > 0 THEN 'confirmed'
                   WHEN f.c_waiting > 0 THEN 'waiting'
                   ELSE 'assigned'
               END
               WHEN f.c_waiting + f.c_confirmed + f.c_partial > 0 AND f.c_partial + f.c_assigned - f.c_assigned_zero > 0 THEN 'assigned'
               WHEN f.c_waiting + f.c_confirmed + f.c_partial = 0 THEN 'assigned'
               WHEN f.c_waiting > 0 THEN 'waiting'
               WHEN f.c_confirmed_demand > 0 THEN 'confirmed'
               WHEN f.c_confirmed > f.c_open_demand THEN 'assigned'
               ELSE 'confirmed'
           END AS c_state
      FROM fold f
      JOIN public.stock_picking p ON p.id = f.id
      JOIN public.stock_location l ON l.id = p.location_id
)
UPDATE public.stock_picking t SET
    move_count = g.c_move,
    draft_count = g.c_draft,
    waiting_count = g.c_waiting,
    confirmed_count = g.c_confirmed,
    partially_available_count = g.c_partial,
    assigned_count = g.c_assigned,
    done_count = g.c_done,
    cancel_count = g.c_cancel,
    done_scrap_count = g.c_done_scrap,
    cancel_scrap_count = g.c_cancel_scrap,
    assigned_zero_demand_count = g.c_assigned_zero,
    open_positive_demand_count = g.c_open_demand,
    confirmed_positive_demand_count = g.c_confirmed_demand,
    mto_nonscrap_count = g.c_mto,
    state = g.c_state
  FROM target g
 WHERE t.id = g.id
   AND (t.move_count, t.draft_count, t.waiting_count, t.confirmed_count,
        t.partially_available_count, t.assigned_count, t.done_count, t.cancel_count,
        t.done_scrap_count, t.cancel_scrap_count, t.assigned_zero_demand_count,
        t.open_positive_demand_count, t.confirmed_positive_demand_count, t.mto_nonscrap_count,
        t.state::text)
       IS DISTINCT FROM
       (g.c_move, g.c_draft, g.c_waiting, g.c_confirmed, g.c_partial, g.c_assigned, g.c_done,
        g.c_cancel, g.c_done_scrap, g.c_cancel_scrap, g.c_assigned_zero, g.c_open_demand,
        g.c_confirmed_demand, g.c_mto, g.c_state);
