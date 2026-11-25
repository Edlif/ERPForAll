-- SPDX-License-Identifier: MIT OR Apache-2.0
-- 1.1 makes the badge code unique per company. Before it, two employees of one company could carry the
-- same code, and the constraint cannot be created over them. This runs before the schema update: the
-- employee with the lowest id keeps the code, the others lose it (they can generate a new one). A
-- database whose employees predate the badge code has no column yet and nothing to clean. A second run
-- changes nothing.
DO $$
BEGIN
    IF EXISTS (SELECT FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'hr_employee' AND column_name = 'barcode') THEN
        UPDATE public.hr_employee e
           SET barcode = NULL
          FROM (SELECT id, row_number() OVER (PARTITION BY company_id, barcode ORDER BY id) AS n
                  FROM public.hr_employee
                 WHERE barcode IS NOT NULL AND company_id IS NOT NULL) d
         WHERE d.id = e.id
           AND d.n > 1;
    END IF;
END
$$;
