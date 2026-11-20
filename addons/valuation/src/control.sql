-- SPDX-License-Identifier: MIT OR Apache-2.0
DECLARE
    is_sealed boolean;
    previous_wait text;
BEGIN
    IF $1 IS NULL OR $2 IS NULL OR $1 <= 0 OR $2 <= 0
       OR $3 IS NULL OR $3 NOT BETWEEN 1 AND 30000 THEN
        RAISE EXCEPTION 'Invalid valuation admission key or lock timeout' USING ERRCODE='22023';
    END IF;
    previous_wait := pg_catalog.current_setting('lock_timeout');
    PERFORM pg_catalog.set_config('lock_timeout',
        (SELECT least($3,nullif(setting::integer,0))::text FROM pg_catalog.pg_settings
         WHERE name='lock_timeout') || 'ms', true);
    PERFORM pg_catalog.pg_advisory_xact_lock_shared(
        pg_catalog.hashtext('erpforall.stock.valuation.control.v1'),
        pg_catalog.hashtext($1::text || '/' || $2::text));
    SELECT sealed INTO is_sealed FROM public.stk_cost_control
        WHERE company_id=$1 AND product_id=$2 FOR SHARE;
    IF NOT FOUND THEN
        -- Both shared lockers may be first. Do not reuse a row outside this RR snapshot.
        INSERT INTO public.stk_cost_control(company_id,product_id,worker_epoch,seal_revision,
            publication_generation,sealed,recorded_at) VALUES ($1,$2,0,0,0,false,
            pg_catalog.transaction_timestamp() AT TIME ZONE 'UTC')
            ON CONFLICT (company_id,product_id) DO NOTHING;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Retry first valuation control admission' USING ERRCODE='40001';
        END IF;
    ELSIF is_sealed THEN
        RAISE EXCEPTION 'Valuation admission is sealed; retry the stock operation' USING ERRCODE='40001';
    END IF;
    PERFORM pg_catalog.set_config('lock_timeout', previous_wait, true);
END
