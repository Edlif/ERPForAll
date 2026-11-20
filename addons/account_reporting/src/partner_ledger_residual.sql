-- SPDX-License-Identifier: MIT OR Apache-2.0
-- $1 contains only caller-visible, posted statutory partner items of company $2.
WITH source AS (
    SELECT l.id, l.company_id, l.partner_id, l.account_id, l.currency_id, l.balance, c.currency_id AS book_currency
      FROM account_move_line l JOIN res_company c ON c.id=l.company_id
     WHERE l.id=ANY($1::bigint[]) AND l.company_id=$2::bigint
), live AS (
    SELECT s.id, p.id AS partial, p.max_date, p.create_date, -p.amount AS delta,
           p.debit_currency_id=s.book_currency AND p.credit_currency_id=s.book_currency AS valid
      FROM source s JOIN account_partial_reconcile p ON p.debit_move_id=s.id
    UNION ALL
    SELECT s.id, p.id, p.max_date, p.create_date, p.amount,
           p.debit_currency_id=s.book_currency AND p.credit_currency_id=s.book_currency
      FROM source s JOIN account_partial_reconcile p ON p.credit_move_id=s.id
), removed AS (
    SELECT s.id, h.original_partial_id AS partial, h.matched_on, h.removed_on, h.create_date,
           h.matched_delta AS delta, h.company_id=s.company_id AND h.currency_id=s.book_currency
           AND h.account_id=s.account_id AND h.partner_id IS NOT DISTINCT FROM s.partner_id
           AND EXISTS (SELECT FROM account_reconcile_removal twin WHERE twin.original_partial_id=h.original_partial_id
                       AND twin.line_id<>s.id AND twin.currency_id=h.currency_id AND twin.matched_delta=-h.matched_delta) AS valid
      FROM source s JOIN account_reconcile_removal h ON h.line_id=s.id
     WHERE h.matched_on <= $3::date AND $3::date < h.removed_on
), facts AS (
    SELECT l.id,l.delta, l.valid AND l.max_date IS NOT NULL AND NOT EXISTS (
        SELECT FROM account_reconcile_removal h WHERE h.line_id=l.id
          AND (l.create_date IS NULL OR $3::date < l.create_date::date + 1)
          AND (l.create_date IS NULL OR h.create_date IS NULL OR l.create_date>h.create_date
               OR (l.create_date=h.create_date AND l.partial>h.original_partial_id))
    ) AS valid FROM live l WHERE l.max_date <= $3::date OR l.max_date IS NULL
    UNION ALL
    -- An archived match lacks its original creation timestamp. Two overlapping removed matches
    -- on one item might be replacements, so their pre-detach residual cannot be certified.
    SELECT h.id,h.delta,h.valid AND NOT EXISTS (
        SELECT FROM account_reconcile_removal other WHERE other.line_id=h.id
            AND other.original_partial_id<>h.partial AND (other.original_partial_id<h.partial
                 OR (other.matched_on <= $3::date AND $3::date < other.removed_on))
    ) FROM removed h
)
SELECT s.id::bigint, COALESCE(s.currency_id=s.book_currency AND s.balance IS NOT NULL AND
       bool_and(CASE WHEN f.id IS NULL THEN true ELSE COALESCE(f.valid,false) AND f.delta IS NOT NULL END),false),
       COALESCE(s.balance+COALESCE(sum(f.delta),0)<>0,false)
  FROM source s LEFT JOIN facts f ON f.id=s.id
 GROUP BY s.id,s.currency_id,s.book_currency,s.balance
