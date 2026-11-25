// SPDX-License-Identifier: MIT OR Apache-2.0
//! Draft returns keep the completed transfer and move identities. Completing a return takes
//! the normal stock path and fences the source transfer before checking the quantity balance.

use std::collections::BTreeMap;

use erp_sdk::prelude::*;
use erp_sdk::{FieldOp, Operation, Row, ValueMap};

use crate::StockPicking;
use crate::move_counters::{lock_move_roots, lock_pickings};

const EXCESS: &str = "The return exceeds the quantity completed on its source transfer.";
const BAD_SOURCE: &str = "Return moves must belong to a completed source transfer in the same company and keep its products and units.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reversal {
    Return,
    Exchange,
}

fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Reading the stock return sources failed.").with_source(error)
}

fn one(records: &Records) -> Result<i64> {
    match records.ids().as_slice() {
        [id] => Ok(*id),
        _ => Err(Error::validation("Select one transfer for a stock return.")),
    }
}

fn optional_id(id: Option<i64>) -> Cell {
    id.map_or(Cell::Null, Cell::Id)
}

async fn check_read(records: &Records, tx: &mut Transaction, fields: &[&str]) -> Result<()> {
    records.check_access(tx, Operation::Read).await?;
    for field in fields {
        if !tx
            .has_field_access(records.env(), records.spec().name, field, FieldOp::Read)
            .await?
        {
            return Err(Error::access(
                "Stock returns require readable source stock records.",
            ));
        }
    }
    Ok(())
}

async fn create_reversal(
    records: &Records,
    tx: &mut Transaction,
    kind: Reversal,
) -> Result<Records> {
    let source = one(records)?;
    check_read(
        records,
        tx,
        &[
            "state",
            "company_id",
            "picking_type_id",
            "location_id",
            "location_dest_id",
            "partner_id",
            "owner_id",
            "name",
            "show_return",
            "return_id",
        ],
    )
    .await?;
    tx.flush().await?;
    let env = records.env().without_default_overrides();
    let types = env.records("stock.picking.type")?;
    let Cell::Id(original) = records.cell(tx, "picking_type_id").await? else {
        return Err(Error::validation(
            "The source transfer needs an operation type.",
        ));
    };
    let original_type = types.browse([original])?;
    let operation = match original_type.cell(tx, "return_picking_type_id").await? {
        Cell::Null => original,
        Cell::Id(id) => id,
        _ => {
            return Err(Error::internal(
                "The return operation type must be a record reference.",
            ));
        }
    };
    check_read(
        &types.browse([operation])?,
        tx,
        &["code", "default_location_dest_id", "company_id", "active"],
    )
    .await?;
    let downstream = downstream_moves(tx, source).await?;
    lock_move_roots(tx, &downstream, &[source]).await?;
    let headers = tx
        .query(
            "SELECT p.state, p.company_id, p.location_dest_id,
                CASE WHEN t.code='incoming' THEN t.default_location_dest_id ELSE p.location_id END,
                p.partner_id, p.owner_id, p.name, p.show_return, p.return_id
           FROM stock_picking p
           JOIN stock_picking_type t ON t.id=$2
                AND t.company_id=p.company_id AND t.active
          WHERE p.id=$1",
            &[&source, &operation],
        )
        .await?;
    let [header] = headers.as_slice() else {
        return Err(Error::validation(
            "The source transfer needs an active return operation type.",
        ));
    };
    let state: &str = header.try_get(0).map_err(decode)?;
    let source_return: Option<i64> = header.try_get(8).map_err(decode)?;
    let exchange = kind == Reversal::Exchange;
    if exchange {
        if source_return.is_none() || state == "cancel" {
            return Err(Error::validation("An exchange requires a return transfer."));
        }
    } else if state != "done" {
        return Err(Error::validation(
            "A stock return requires a completed transfer.",
        ));
    }
    let company: i64 = header.try_get(1).map_err(decode)?;
    let from: i64 = header.try_get(2).map_err(decode)?;
    let to: Option<i64> = header.try_get(3).map_err(decode)?;
    let to =
        to.ok_or_else(|| Error::validation("The return operation needs a destination location."))?;
    let partner: Option<i64> = header.try_get(4).map_err(decode)?;
    let owner: Option<i64> = header.try_get(5).map_err(decode)?;
    let reference: &str = header.try_get(6).map_err(decode)?;
    let show_return: bool = header.try_get(7).map_err(decode)?;
    let source_rows = tx.query(
        "SELECT m.id,m.product_id,m.uom_id,m.restrict_partner_id,m.quantity,m.origin_returned_move_id
           FROM stock_move m WHERE m.picking_id=$1 AND m.company_id=$2 AND m.is_scrap IS NOT TRUE
            AND CASE WHEN $3 THEN m.state<>'cancel' AND m.quantity>0 ELSE m.state='done' END
          ORDER BY m.id",
        &[&source, &company, &exchange],
    ).await?;
    if source_rows.is_empty() {
        return Err(Error::validation(if exchange {
            "Reserve or enter the return quantities before an exchange."
        } else {
            "The source transfer has no completed moves to return."
        }));
    }
    let ids = source_rows
        .iter()
        .map(|row| row.try_get(0).map_err(decode))
        .collect::<Result<Vec<i64>>>()?;
    let moves = env.records("stock.move")?;
    check_read(
        &moves.browse(ids)?,
        tx,
        &[
            "product_id",
            "uom_id",
            "restrict_partner_id",
            "quantity",
            "origin_returned_move_id",
        ],
    )
    .await?;
    if !downstream.is_empty() {
        moves
            .browse(downstream.clone())?
            .check_access(tx, Operation::Write)
            .await?;
        crate::reservation::unreserve_moves(records, tx, &downstream).await?;
    }
    let picking = env
        .records("stock.picking")?
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("picking_type_id", Cell::Id(operation))
                    .set_cell("location_id", Cell::Id(from))
                    .set_cell("location_dest_id", Cell::Id(to))
                    .set_cell("partner_id", optional_id(partner))
                    .set_cell("owner_id", optional_id(owner))
                    .set_cell("user_id", Cell::Null)
                    .set_cell("return_id", Cell::Id(source))
                    .set_cell("show_return", Cell::Bool(exchange || !show_return))
                    .set_cell("origin", Cell::Str(transfer_origin(reference, kind).into())),
            ],
        )
        .await?;
    let return_id = one(&picking)?;
    let values = source_rows
        .iter()
        .map(|row| {
            let source_move: i64 = row.try_get(0).map_err(decode)?;
            let source_origin: Option<i64> = row.try_get(5).map_err(decode)?;
            let quantity = if exchange {
                row.try_get(4).map_err(decode)?
            } else {
                Decimal::ZERO
            };
            let mut vals = RawVals::new()
                .set_cell("company_id", Cell::Id(company))
                .set_cell("picking_id", Cell::Id(return_id))
                .set_cell("picking_type_id", Cell::Id(operation))
                .set_cell("product_id", Cell::Id(row.try_get(1).map_err(decode)?))
                .set_cell("uom_id", Cell::Id(row.try_get(2).map_err(decode)?))
                .set_cell(
                    "origin_returned_move_id",
                    optional_id((!exchange).then_some(source_move)),
                )
                .set_cell("location_id", Cell::Id(from))
                .set_cell("location_dest_id", Cell::Id(to))
                .set_cell("partner_id", optional_id(partner))
                .set_cell(
                    "restrict_partner_id",
                    optional_id(row.try_get(3).map_err(decode)?),
                )
                .set_cell("product_uom_qty", Cell::Decimal(quantity));
            if !exchange && source_origin.is_none() {
                let source_move = RecId::from_real(source_move)
                    .ok_or_else(|| Error::invariant("Source stock move has no real identity."))?;
                vals = vals.set_commands("move_orig_ids", [Command::Link(source_move)]);
            }
            Ok(vals)
        })
        .collect::<Result<Vec<_>>>()?;
    moves.create(tx, values).await?;
    Ok(picking)
}

async fn downstream_moves(tx: &mut Transaction, picking: i64) -> Result<Vec<i64>> {
    tx.query(
        "SELECT DISTINCT downstream.id FROM stock_move original
           JOIN stock_move_move_rel rel ON rel.move_orig_id=original.id
           JOIN stock_move downstream ON downstream.id=rel.move_dest_id
          WHERE original.picking_id=$1 AND original.state<>'cancel'
            AND original.is_scrap IS NOT TRUE AND downstream.is_scrap IS NOT TRUE
            AND downstream.state NOT IN ('done','cancel') ORDER BY downstream.id",
        &[&picking],
    )
    .await?
    .iter()
    .map(|row| row.try_get(0).map_err(decode))
    .collect()
}

fn transfer_origin(reference: &str, kind: Reversal) -> String {
    match kind {
        Reversal::Return => format!("Return of {reference}"),
        Reversal::Exchange => format!("Exchange for {reference}"),
    }
}

fn form_action(picking: &Records, kind: Reversal) -> Result<Value> {
    let name = match kind {
        Reversal::Return => "Returned Transfer",
        Reversal::Exchange => "Exchange Transfer",
    };
    let return_id = one(picking)?;
    Ok(Value::Map(
        ValueMap::from([
            ("type".into(), Value::str("ir.actions.act_window")),
            ("name".into(), Value::str(name)),
            ("res_model".into(), Value::str("stock.picking")),
            ("res_id".into(), Value::Int(return_id)),
            (
                "views".into(),
                Value::list([Value::list([Value::False, Value::str("form")])]),
            ),
            ("target".into(), Value::str("current")),
        ])
        .into(),
    ))
}

async fn return_demands(records: &Records, tx: &mut Transaction, clear: bool) -> Result<()> {
    let picking = one(records)?;
    records.check_access(tx, Operation::Write).await?;
    tx.flush().await?;
    let origins = completion_origins(records, tx, &[picking]).await?;
    let [origin] = origins.as_slice() else {
        return Err(Error::validation("Select an open return transfer."));
    };
    lock_pickings(tx, &[picking, *origin]).await?;
    let rows = tx.query(
        "SELECT m.id, source.id, source.quantity - COALESCE((
             SELECT SUM(other.quantity) FROM stock_move other
               JOIN stock_picking sibling ON sibling.id=other.picking_id AND sibling.return_id=source.picking_id
              WHERE other.origin_returned_move_id=source.id AND other.id<>m.id AND other.state<>'cancel'
                AND other.is_scrap IS NOT TRUE
         ),0), m.uom_id
           FROM stock_move m
           LEFT JOIN stock_move source ON source.id=m.origin_returned_move_id AND source.picking_id=$2
                AND source.company_id=m.company_id AND source.product_id=m.product_id
                AND source.uom_id=m.uom_id AND source.state='done' AND source.is_scrap IS NOT TRUE
          WHERE m.picking_id=$1 AND m.state NOT IN ('done','cancel') AND m.is_scrap IS NOT TRUE
          ORDER BY m.id",
        &[&picking, origin],
    ).await?;
    let moves = records.env().records("stock.move")?;
    check_read(
        &moves.browse(source_ids(&rows, 1)?)?,
        tx,
        &["quantity", "uom_id"],
    )
    .await?;
    let units = records.env().records("uom.uom")?;
    for row in rows {
        let id: i64 = row.try_get(0).map_err(decode)?;
        let remaining: Decimal = row.try_get(2).map_err(decode)?;
        let quantity = if clear {
            Decimal::ZERO
        } else {
            if remaining < Decimal::ZERO {
                return Err(Error::validation(EXCESS));
            }
            let unit: i64 = row.try_get(3).map_err(decode)?;
            units
                .browse([unit])?
                .call::<(Decimal,), Decimal>(tx, "round", (remaining,))
                .await?
        };
        moves
            .browse([id])?
            .write(
                tx,
                RawVals::new().set_cell("product_uom_qty", Cell::Decimal(quantity)),
            )
            .await?;
    }
    Ok(())
}

/// Find source transfers before acquiring the normal ascending transfer roots.
pub(crate) async fn completion_origins(
    records: &Records,
    tx: &mut Transaction,
    pickings: &[i64],
) -> Result<Vec<i64>> {
    let ids = tx.query(
        "SELECT DISTINCT return_id FROM stock_picking
          WHERE id=ANY($1) AND state NOT IN ('done','cancel') AND return_id IS NOT NULL ORDER BY return_id",
        &[&pickings],
    ).await?.iter().map(|row| row.try_get(0).map_err(decode)).collect::<Result<Vec<i64>>>()?;
    if !ids.is_empty() {
        records
            .env()
            .records("stock.picking")?
            .browse(ids.clone())?
            .check_access(tx, Operation::Read)
            .await?;
    }
    Ok(ids)
}

fn source_ids(rows: &[Row], column: usize) -> Result<Vec<i64>> {
    rows.iter()
        .map(|row| {
            row.try_get::<_, Option<i64>>(column)
                .map_err(decode)?
                .ok_or_else(|| Error::validation(BAD_SOURCE))
        })
        .collect()
}

/// Check completed and proposed line quantities in the physical product unit. A source tuple
/// version prevents a second return from checking an earlier snapshot after the first commits.
pub(crate) async fn guard_completion(
    records: &Records,
    tx: &mut Transaction,
    origins: &[i64],
    lines: &[i64],
) -> Result<()> {
    if origins.is_empty() {
        return Ok(());
    }
    let source_moves = tx
        .query(
            "SELECT DISTINCT CASE WHEN src.id IS NOT NULL THEN src.id ELSE exchange.id END
           FROM stock_move_line l
           JOIN stock_move m ON m.id=l.move_id
           JOIN stock_picking p ON p.id=m.picking_id
           LEFT JOIN stock_picking source ON source.id=p.return_id AND source.company_id=p.company_id
           LEFT JOIN stock_move src ON src.id=m.origin_returned_move_id
                AND src.picking_id=source.id AND source.state='done'
                AND src.state='done' AND src.company_id=m.company_id AND src.is_scrap IS NOT TRUE
                AND src.product_id=m.product_id AND src.uom_id=m.uom_id
           LEFT JOIN LATERAL (
                SELECT e.id FROM stock_move e
                 WHERE e.picking_id=source.id AND e.company_id=m.company_id
                   AND e.product_id=m.product_id AND e.uom_id=m.uom_id
                   AND e.state<>'cancel' AND e.quantity>0 AND e.is_scrap IS NOT TRUE
                 ORDER BY e.id LIMIT 1
           ) exchange ON m.origin_returned_move_id IS NULL AND source.return_id IS NOT NULL
                AND source.state<>'cancel' AND p.show_return
                AND p.location_id=source.location_dest_id AND m.location_id=p.location_id
          WHERE l.id=ANY($1) AND p.return_id IS NOT NULL AND m.is_scrap IS NOT TRUE",
            &[&lines],
        )
        .await?;
    check_read(
        &records
            .env()
            .records("stock.move")?
            .browse(source_ids(&source_moves, 0)?)?,
        tx,
        &["quantity", "product_id", "uom_id"],
    )
    .await?;
    // Transfer roots were already locked ascending by the validation entry point.
    tx.execute(
        "UPDATE stock_picking SET write_date=write_date WHERE id=ANY($1)",
        &[&origins],
    )
    .await?;
    let exceeded = tx
        .query(
            "WITH proposed AS (
             SELECT m.origin_returned_move_id AS origin, SUM(l.quantity_product_uom) AS qty
               FROM stock_move_line l JOIN stock_move m ON m.id=l.move_id
               JOIN stock_picking p ON p.id=m.picking_id
              WHERE l.id=ANY($1) AND p.return_id IS NOT NULL AND m.origin_returned_move_id IS NOT NULL
                AND m.is_scrap IS NOT TRUE
              GROUP BY m.origin_returned_move_id
         )
         SELECT 1 FROM proposed pr JOIN stock_move src ON src.id=pr.origin
          WHERE pr.qty + COALESCE((
                SELECT SUM(l.quantity_product_uom) FROM stock_move m
                  JOIN stock_picking p ON p.id=m.picking_id AND p.return_id=src.picking_id
                  JOIN stock_move_line l ON l.move_id=m.id AND l.membership='done'
                 WHERE m.origin_returned_move_id=src.id AND m.company_id=src.company_id
                   AND m.product_id=src.product_id AND m.state='done' AND m.is_scrap IS NOT TRUE
          ),0) > COALESCE((
                SELECT SUM(l.quantity_product_uom) FROM stock_move_line l
                 WHERE l.move_id=src.id AND l.membership='done'
          ),0)
          LIMIT 1",
            &[&lines],
        )
        .await?;
    if !exceeded.is_empty() {
        return Err(Error::validation(EXCESS));
    }
    Ok(())
}

#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    #[compute(depends = ["return_ids"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_return_count(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockPicking>,
    ) -> Result<()> {
        let records = this.as_records();
        let ids = records.ids();
        let mut counts = BTreeMap::new();
        if !ids.is_empty() {
            let groups = this
                .env()
                .records("stock.picking")?
                ._read_group(
                    tx,
                    ReadGroup::new(Domain::field("return_id")?.in_(ids))
                        .groupby(["return_id"])
                        .aggregates(["__count"]),
                )
                .await?;
            for (key, quantity) in groups.column(0).iter().zip(groups.column(1)) {
                let id = key.record_id().ok_or_else(|| {
                    Error::invariant("Return count group has no source transfer.")
                })?;
                let GroupValue::Int(count) = quantity else {
                    return Err(Error::invariant(
                        "Return count aggregate is not an integer.",
                    ));
                };
                counts.insert(
                    id.get(),
                    i32::try_from(*count).map_err(|e| {
                        Error::validation("Return count exceeds the supported integer range.")
                            .with_source(e)
                    })?,
                );
            }
        }
        for (index, rec_id) in records.rec_ids().iter().enumerate() {
            // New records and source transfers with no visible returns get 0.
            let count = rec_id
                .as_real()
                .and_then(|id| counts.get(&id.get()).copied())
                .unwrap_or(0);
            out.set(index, StockPicking::RETURN_COUNT, count)?;
        }
        Ok(())
    }

    /// Open a new draft return of a completed transfer.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_return(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records().clone();
        tx.savepoint(async move |tx| {
            let picking = create_reversal(&records, tx, Reversal::Return).await?;
            form_action(&picking, Reversal::Return)
        })
        .await
    }

    /// Create and assign the replacement transfer for the quantities on a return.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_exchange(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records().clone();
        tx.savepoint(async move |tx| {
            let picking = create_reversal(&records, tx, Reversal::Exchange).await?;
            picking.call::<(), ()>(tx, "action_confirm", ()).await?;
            picking.call::<(), ()>(tx, "action_assign", ()).await?;
            form_action(&picking, Reversal::Exchange)
        })
        .await
    }

    /// Set the outstanding demands on a return using its original completed quantities.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_return_all(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records().clone();
        tx.savepoint(async move |tx| return_demands(&records, tx, false).await)
            .await
    }

    /// Clear the demands on an unfinished return.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_clear_return(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records().clone();
        tx.savepoint(async move |tx| return_demands(&records, tx, true).await)
            .await
    }
}
