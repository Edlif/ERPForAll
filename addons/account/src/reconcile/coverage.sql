-- SPDX-License-Identifier: MIT OR Apache-2.0
INSERT INTO account_reconcile_coverage (name, safe_from)
SELECT 'partial_matches', CASE WHEN m.latest_version IS NULL
    AND NOT EXISTS (SELECT FROM account_move_line) THEN NULL
    ELSE (statement_timestamp() AT TIME ZONE 'UTC')::date + 1 END
  FROM ir_module_module m WHERE m.name = 'account'
ON CONFLICT (name) DO NOTHING
