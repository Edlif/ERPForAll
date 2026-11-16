-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Posting arrives: entries and their items may now be posted or cancelled. The two
-- draft-only CHECK constraints of the draft part are no longer declared; schema sync does not drop
-- a constraint whose declaration was removed, so this drops them. Rows are not touched.
ALTER TABLE IF EXISTS public.account_move
    DROP CONSTRAINT IF EXISTS account_move_draft_until_posting;
ALTER TABLE IF EXISTS public.account_move_line
    DROP CONSTRAINT IF EXISTS account_move_line_draft_until_posting;
