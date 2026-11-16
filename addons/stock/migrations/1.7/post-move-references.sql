-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Rebuild the transfers' reference projection from the moves' references (topology ADR D3) and
-- fill the new stored delay alert date. Before 1.7 either side of the
-- projection could be written directly, so it is compared with the canonical relation: pairs no
-- move supports go, missing pairs come. A second run changes nothing. As for 1.6, the upgrade's
-- snapshot predates the lock: stop older nodes first, or run this file once more after the upgrade
-- ("Inventory move counters and references").
LOCK TABLE public.stock_picking, public.stock_move, public.stock_reference_move_rel,
    public.stock_reference_picking_rel, public.stock_move_move_rel IN SHARE ROW EXCLUSIVE MODE;
DELETE FROM public.stock_reference_picking_rel p
 WHERE NOT EXISTS (
       SELECT FROM public.stock_reference_move_rel r
         JOIN public.stock_move m ON m.id = r.move_id
        WHERE r.reference_id = p.reference_id AND m.picking_id = p.picking_id);
INSERT INTO public.stock_reference_picking_rel (reference_id, picking_id)
SELECT DISTINCT r.reference_id, m.picking_id
  FROM public.stock_reference_move_rel r
  JOIN public.stock_move m ON m.id = r.move_id
 WHERE m.picking_id IS NOT NULL
ON CONFLICT DO NOTHING;
UPDATE public.stock_move m
   SET delay_alert_date = s.latest
  FROM (SELECT e.move_dest_id AS id, max(o.date) AS latest
          FROM public.stock_move_move_rel e
          JOIN public.stock_move o ON o.id = e.move_orig_id
         WHERE o.state NOT IN ('done', 'cancel')
         GROUP BY e.move_dest_id) s
 WHERE m.id = s.id
   AND m.state NOT IN ('done', 'cancel')
   AND s.latest > m.date
   AND m.delay_alert_date IS DISTINCT FROM s.latest;
