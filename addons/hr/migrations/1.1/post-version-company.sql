-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Before 1.1 the company of a contract history row was typed by hand; it is now computed from the
-- employee. The schema sync keeps the stored column as it is, so rows filed under another company
-- are moved to their employee's company here. A second run changes nothing.
UPDATE public.hr_employee_version v
   SET company_id = e.company_id
  FROM public.hr_employee e
 WHERE e.id = v.employee_id
   AND v.company_id IS DISTINCT FROM e.company_id;
