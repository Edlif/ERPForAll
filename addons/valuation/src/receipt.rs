// SPDX-License-Identifier: MIT OR Apache-2.0
//! Plan incoming ownership crossings before stock takes its first physical lock.
use erp_addon_stock::{StockMoveLine, StockPicking};
use erp_sdk::{Operation, prelude::*};
use std::collections::BTreeSet;

type Key = (i64, i64);

// Draft demand is materialized as a line by stock's fill_demand. Existing open lines use their
// own endpoints and ownership; the picking defaults are not reliable evidence of a crossing.
const PLAN: &str = "
WITH candidates AS (
    SELECT m.id AS move_id, l.id AS line_id, l.company_id, l.product_id, l.location_id,
           l.location_dest_id, l.owner_id
    FROM public.stock_move m JOIN public.stock_picking p ON p.id=m.picking_id
    JOIN public.stock_move_line l ON l.move_id=m.id AND l.picking_id=p.id
    WHERE p.id=ANY($1) AND p.state NOT IN ('done','cancel') AND m.state NOT IN ('done','cancel')
      AND l.membership='open' AND l.active AND l.done_at IS NULL AND l.quantity_product_uom>0
    UNION ALL
    SELECT m.id,NULL,m.company_id,m.product_id,m.location_id,m.location_dest_id,NULL
    FROM public.stock_move m JOIN public.stock_picking p ON p.id=m.picking_id
    WHERE p.id=ANY($1) AND p.state='draft' AND m.state='draft'
      AND m.quantity=0 AND m.product_uom_qty>0
)
SELECT c.move_id,c.line_id,c.company_id,c.product_id,c.location_id,c.location_dest_id
FROM candidates c
JOIN public.product_product v ON v.id=c.product_id JOIN public.product_template t ON t.id=v.product_tmpl_id
JOIN public.stock_location src ON src.id=c.location_id JOIN public.stock_location dst ON dst.id=c.location_dest_id
JOIN public.res_company co ON co.id=c.company_id
WHERE t.is_storable AND src.usage='supplier' AND dst.usage IN ('internal','transit')
  AND dst.company_id=c.company_id AND (c.owner_id IS NULL OR c.owner_id=co.partner_id)";

const COMPLETED: &str = "
SELECT DISTINCT l.company_id,l.product_id FROM public.stock_move_line l
JOIN public.product_product v ON v.id=l.product_id JOIN public.product_template t ON t.id=v.product_tmpl_id
JOIN public.stock_location src ON src.id=l.location_id JOIN public.stock_location dst ON dst.id=l.location_dest_id
JOIN public.res_company co ON co.id=l.company_id
WHERE l.id=ANY($1) AND l.membership='done' AND l.done_at IS NOT NULL AND l.quantity_product_uom>0
  AND t.is_storable AND src.usage='supplier' AND dst.usage IN ('internal','transit')
  AND dst.company_id=l.company_id AND (l.owner_id IS NULL OR l.owner_id=co.partner_id)";

#[derive(Default)]
struct Admitted(Option<BTreeSet<Key>>);

// A savepoint-aware marker makes the immutable first plan live only as long as its
// PostgreSQL locks. The unmarked payload can be replaced after an enclosing rollback.
#[derive(Default)]
struct FirstPlan(BTreeSet<Key>);

async fn first_picking_plan(
    tx: &mut Transaction,
    records: &Records,
    keys: &BTreeSet<Key>,
) -> Result<bool> {
    if !tx.scoped_ids::<FirstPlan>().is_empty() {
        if !keys.is_subset(&tx.extension::<FirstPlan>().0) {
            return Err(Error::new(
                erp_sdk::ErrorKind::Concurrency,
                "Pass all affected pickings in one validation before taking physical stock locks.",
            )
            .with_sqlstate(Some("40001".to_owned())));
        }
        return Ok(false);
    }
    if keys.is_empty() {
        let rows = tx.query("SELECT id FROM public.stock_picking WHERE id=ANY($1) AND state NOT IN ('done','cancel') LIMIT 1", &[&records.ids()]).await?;
        if rows.is_empty() {
            return Ok(false);
        }
    }
    tx.extension::<FirstPlan>().0.clone_from(keys);
    tx.mark_scoped_id::<FirstPlan>(1);
    Ok(true)
}

async fn authorize_plan(tx: &mut Transaction, records: &Records) -> Result<BTreeSet<Key>> {
    let mut keys = BTreeSet::new();
    let mut moves = BTreeSet::new();
    let mut lines = BTreeSet::new();
    let mut companies = BTreeSet::new();
    let mut products = BTreeSet::new();
    let mut locations = BTreeSet::new();
    let decode = |e| Error::db("Invalid receipt admission source.").with_source(e);
    for row in tx.query(PLAN, &[&records.ids()]).await? {
        let company: i64 = row.try_get(2).map_err(decode)?;
        let product: i64 = row.try_get(3).map_err(decode)?;
        moves.insert(row.try_get::<_, i64>(0).map_err(decode)?);
        lines.extend(row.try_get::<_, Option<i64>>(1).map_err(decode)?);
        companies.insert(company);
        products.insert(product);
        locations.insert(row.try_get::<_, i64>(4).map_err(decode)?);
        locations.insert(row.try_get::<_, i64>(5).map_err(decode)?);
        keys.insert((company, product));
    }
    if keys.is_empty() {
        return Ok(keys);
    }
    let env = records.env();
    if !tx.has_group(env, "stock.group_stock_manager").await? {
        return Err(Error::access(
            "Only stock managers may admit a valued stock receipt.",
        ));
    }
    let active: BTreeSet<i64> = env.companies()?.iter().map(|id| id.get()).collect();
    if !companies.is_subset(&active) {
        return Err(Error::access(
            "The stock receipt belongs to a company outside the active companies.",
        ));
    }
    for (model, ids) in [
        ("stock.move", moves),
        ("stock.move.line", lines),
        ("res.company", companies),
        ("product.product", products),
        ("stock.location", locations),
    ] {
        env.records(model)?
            .browse(ids)?
            .check_access(tx, Operation::Read)
            .await?;
    }
    Ok(keys)
}

// The ORM arms the ordinary transaction timer at BEGIN. Tighten only a looser bound,
// subtracting the time already spent in this transaction before resetting PostgreSQL's timer.
async fn enforce_deadline(tx: &mut Transaction) -> Result<()> {
    let limit = erp_sdk::params::get_int(tx, &crate::ADDON, crate::UNIT_LIMIT).await?;
    let row=&tx.query("SELECT setting::bigint,
        $1::bigint - ceil(extract(epoch FROM (clock_timestamp()-transaction_timestamp()))*1000)::bigint
        FROM pg_catalog.pg_settings WHERE name='transaction_timeout'", &[&limit]).await?;
    let [row] = row.as_slice() else {
        return Err(Error::invariant(
            "The valuation transaction backstop is unavailable.",
        ));
    };
    let installed: i64 = row
        .try_get(0)
        .map_err(|e| Error::db("Invalid transaction backstop.").with_source(e))?;
    let remaining: i64 = row
        .try_get(1)
        .map_err(|e| Error::db("Invalid remaining valuation duration.").with_source(e))?;
    if remaining <= 0 {
        return Err(Error::user(
            "The stock receipt exceeded the valuation transaction deadline.",
        ));
    }
    if installed == 0 || installed > limit {
        tx.query(
            "SELECT pg_catalog.set_config('transaction_timeout','0',true)",
            &[],
        )
        .await?;
        tx.query(
            "SELECT pg_catalog.set_config('transaction_timeout',$1,true)",
            &[&format!("{remaining}ms")],
        )
        .await?;
    }
    Ok(())
}

#[extend(StockPicking, addon = "valuation")]
struct ReceiptPicking {}

#[methods(StockPicking, addon = "valuation")]
impl ReceiptPicking {
    #[method(override)]
    async fn button_validate(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records().clone();
        records.check_access(tx, Operation::Write).await?;
        let result = tx
            .savepoint(async move |tx| {
                tx.flush().await?;
                let keys = authorize_plan(tx, &records).await?;
                if first_picking_plan(tx, &records, &keys).await? && !keys.is_empty() {
                    enforce_deadline(tx).await?;
                    let limit =
                        erp_sdk::params::get_int(tx, &crate::ADDON, crate::KEY_LIMIT).await?;
                    if keys.len() as u64 > limit as u64 {
                        return Err(Error::validation(format!(
                            "Too many valued products in one operation: {} exceeds {limit}.",
                            keys.len()
                        )));
                    }
                    let wait = i32::try_from(
                        erp_sdk::params::get_int(tx, &crate::ADDON, crate::LOCK_WAIT).await?,
                    )
                    .map_err(|_| Error::invariant("Invalid valuation admission lock timeout."))?;
                    for (company, product) in &keys {
                        tx.query(
                            "SELECT public.stk_admit_receipt_control($1,$2,$3)",
                            &[company, product, &wait],
                        )
                        .await
                        .map_err(Error::retry_whole_unit_on_lock_timeout)?;
                    }
                }
                let prior = tx.extension::<Admitted>().0.replace(keys);
                let result = sup.call(tx, &records, ()).await;
                tx.extension::<Admitted>().0 = prior;
                result
            })
            .await;
        if let Err(error) = &result {
            tx.require_transaction_retry(error);
        }
        result
    }
}

#[extend(StockMoveLine, addon = "valuation")]
struct ReceiptLine {}

#[methods(StockMoveLine, addon = "valuation")]
impl ReceiptLine {
    #[method(override)]
    async fn _after_stock_completion(
        this: &RecordSet<StockMoveLine>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        for row in tx.query(COMPLETED, &[&records.ids()]).await? {
            let key = (
                row.try_get(0)
                    .map_err(|e| Error::db("Invalid receipt company.").with_source(e))?,
                row.try_get(1)
                    .map_err(|e| Error::db("Invalid receipt product.").with_source(e))?,
            );
            if !tx
                .extension::<Admitted>()
                .0
                .as_ref()
                .is_some_and(|keys| keys.contains(&key))
            {
                return Err(Error::new(erp_sdk::ErrorKind::Concurrency, "The completed receipt differs from its admitted valuation keys. Retry the operation.").with_sqlstate(Some("40001".to_owned())));
            }
        }
        sup.call(tx, records, ()).await
    }
}
