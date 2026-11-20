-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Move the shipped action off the legacy definition. Preserve any action whose target
-- was customized, and preserve all other action parameters and report definitions.
UPDATE public.ir_act_client AS action
   SET params = jsonb_set(action.params, '{report}', to_jsonb(quantity.res_id))
  FROM public.ir_model_data AS shipped_action,
       public.ir_model_data AS legacy,
       public.ir_model_data AS quantity
 WHERE shipped_action.module = 'stock'
   AND shipped_action.name = 'action_report_stock_position'
   AND shipped_action.model = 'ir.actions.client'
   AND legacy.module = 'stock'
   AND legacy.name = 'report_stock_position'
   AND legacy.model = 'reporting.report'
   AND quantity.module = 'stock'
   AND quantity.name = 'report_stock_quantity'
   AND quantity.model = 'reporting.report'
   AND action.id = shipped_action.res_id
   AND action.tag = 'reporting.report'
   AND action.params->'report' = to_jsonb(legacy.res_id);
