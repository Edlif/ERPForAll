// SPDX-License-Identifier: MIT OR Apache-2.0
//! Continue a short direct retail transfer under its operation type's Always policy.

use std::collections::BTreeMap;

use erp_sdk::prelude::*;
use erp_sdk::{FieldOp, Operation};

pub(super) struct ShortMove {
    pub id: i64,
    pub completed: Decimal,
    pub remaining: Decimal,
}

const HEADER: &[&str] = &[
    "company_id",
    "picking_type_id",
    "location_id",
    "location_dest_id",
    "partner_id",
    "owner_id",
    "origin",
    "scheduled_date",
    "date_deadline",
    "priority",
    "move_type",
];
const MOVEMENT: &[&str] = &[
    "company_id",
    "picking_type_id",
    "location_id",
    "location_dest_id",
    "product_id",
    "uom_id",
    "partner_id",
    "restrict_partner_id",
    "origin",
    "date_deadline",
    "date",
    "priority",
    "sequence",
    "price_unit",
    "rule_id",
    "warehouse_id",
    "description_picking_manual",
];

async fn readable(tx: &mut Transaction, records: &Records, fields: &[&str]) -> Result<()> {
    records.check_access(tx, Operation::Read).await?;
    for field in fields {
        if !tx
            .has_field_access(records.env(), records.spec().name, field, FieldOp::Read)
            .await?
        {
            return Err(Error::access(
                "The source fields of this backorder must be readable.",
            ));
        }
    }
    Ok(())
}

async fn values(
    tx: &mut Transaction,
    rows: &Records,
    fields: &[&'static str],
) -> Result<Vec<RawVals>> {
    readable(tx, rows, fields).await?;
    let mut result: Vec<RawVals> = (0..rows.len()).map(|_| RawVals::new()).collect();
    for &field in fields {
        for (vals, cell) in result.iter_mut().zip(rows.cells(tx, field).await?) {
            vals.insert(field.into(), cell);
        }
    }
    Ok(result)
}

pub(super) async fn split(
    tx: &mut Transaction,
    records: &Records,
    shortages: &BTreeMap<i64, Vec<ShortMove>>,
) -> Result<Vec<i64>> {
    let env = records.env().without_default_overrides();
    let picking_model = env.records("stock.picking")?;
    let move_model = env.records("stock.move")?;
    let mut new_moves = Vec::new();
    for (parent, shortages) in shortages {
        let transfer = picking_model.browse([*parent])?;
        let ids = shortages.iter().map(|m| m.id).collect::<Vec<_>>();
        let source = move_model.browse(ids.clone())?;
        source.check_access(tx, Operation::Write).await?;
        readable(
            tx,
            &source,
            &[
                "product_uom_qty",
                "quantity",
                "backorder_root_move_id",
                "move_orig_ids",
                "move_dest_ids",
                "is_scrap",
                "is_inventory",
                "procure_method",
                "rule_id",
                "route_ids",
                "reference_ids",
                "never_product_template_attribute_value_ids",
                "packaging_uom_id",
                "origin_returned_move_id",
            ],
        )
        .await?;
        readable(tx, &transfer, &["picking_type_id", "return_id"]).await?;
        let types = transfer.related(tx, "picking_type_id").await?;
        readable(tx, &types, &["code", "create_backorder"]).await?;
        for field in ["location_id", "location_dest_id"] {
            readable(tx, &source, &[field]).await?;
            let locations = source.related(tx, field).await?;
            readable(tx, &locations, &["usage"]).await?;
        }
        // Never silently drop a route, product configuration or return allocation while splitting.
        let unsupported = tx.query(
            "SELECT m.id FROM stock_move m JOIN stock_picking p ON p.id=m.picking_id
             JOIN stock_picking_type t ON t.id=p.picking_type_id
             JOIN stock_location src ON src.id=m.location_id JOIN stock_location dst ON dst.id=m.location_dest_id
             WHERE m.id=ANY($1) AND (p.return_id IS NOT NULL OR m.origin_returned_move_id IS NOT NULL
               OR m.is_scrap IS TRUE OR m.is_inventory IS TRUE
               OR m.packaging_uom_id IS NOT NULL OR m.procure_method IS DISTINCT FROM 'make_to_stock'
               OR NOT ((t.code='incoming' AND src.usage='supplier' AND dst.usage='internal')
                    OR (t.code='outgoing' AND src.usage='internal' AND dst.usage='customer'))
               OR EXISTS (SELECT 1 FROM stock_move_move_rel r WHERE r.move_orig_id=m.id OR r.move_dest_id=m.id))
             LIMIT 1", &[&ids],
        ).await?;
        if !unsupported.is_empty() {
            return Err(Error::user(
                "Automatic backorders support direct vendor receipts and customer deliveries. Split routed or return transfers separately.",
            ));
        }
        // A constraint on an already-authorized source move survives archiving and target
        // record rules. Inspect only the existence of its links, never the hidden target rows.
        let constrained = tx.query(
            "SELECT m.id FROM stock_move m WHERE m.id=ANY($1) AND (
                EXISTS (SELECT 1 FROM stock_route_move r WHERE r.move_id=m.id)
                OR EXISTS (SELECT 1 FROM template_attribute_value_stock_move_rel a WHERE a.move_id=m.id)
                OR EXISTS (SELECT 1 FROM stock_reference_move_rel r WHERE r.move_id=m.id))
             LIMIT 1",
            &[&ids],
        ).await?;
        if !constrained.is_empty() {
            return Err(Error::user(
                "The automatic retail backorder cannot split moves with routes, attribute exclusions or stock references.",
            ));
        }
        let mut header = values(tx, &transfer, HEADER)
            .await?
            .pop()
            .ok_or_else(|| Error::invariant("Missing source transfer."))?;
        header.insert("backorder_id".into(), Cell::Id(*parent));
        header.insert("user_id".into(), Cell::Null);
        let mut child_values = values(tx, &source, MOVEMENT).await?;
        let child = picking_model.create(tx, vec![header]).await?;
        let child_id = child.ensure_one()?.int()?;
        for (vals, shortage) in child_values.iter_mut().zip(shortages) {
            vals.insert("picking_id".into(), Cell::Id(child_id));
            vals.insert("product_uom_qty".into(), Cell::Decimal(shortage.remaining));
        }
        let children = move_model.create(tx, child_values).await?;
        let child_ids = children.ids();
        if child_ids.len() != ids.len() {
            return Err(Error::invariant(
                "The backorder move count changed during creation.",
            ));
        }
        tx.flush().await?;
        let linked = tx.execute(
            "UPDATE stock_move child SET backorder_root_move_id=COALESCE(source.backorder_root_move_id,source.id)
             FROM unnest($1::bigint[], $2::bigint[]) AS pair(source_id,child_id)
             JOIN stock_move source ON source.id=pair.source_id
             WHERE child.id=pair.child_id AND child.company_id=source.company_id",
            &[&ids,&child_ids],
        ).await?;
        if linked != ids.len() as u64 {
            return Err(Error::invariant(
                "Some backorder movements could not be linked to their original demand.",
            ));
        }
        tx.invalidate_model("stock.move", Some(&["backorder_root_move_id"]))
            .await?;
        // Keep the native UoM projection and counter triggers in sync with the completed fraction.
        for m in shortages {
            move_model
                .browse([m.id])?
                .write(
                    tx,
                    RawVals::new().set_cell("product_uom_qty", Cell::Decimal(m.completed)),
                )
                .await?;
        }
        new_moves.extend(child_ids);
    }
    Ok(new_moves)
}

/// A generated demand cannot be reassigned to a different product, company or transfer.
pub(super) async fn guard_identity(
    tx: &mut Transaction,
    moves: &Records,
    vals: &RawVals,
) -> Result<()> {
    let fields = [
        "company_id",
        "product_id",
        "picking_id",
        "picking_type_id",
        "location_id",
        "location_dest_id",
    ];
    if !fields.iter().any(|field| vals.contains(field)) {
        return Ok(());
    }
    moves.check_access(tx, Operation::Write).await?;
    let linked: Vec<i64> = tx.query(
        "SELECT id FROM stock_move WHERE id=ANY($1) AND backorder_root_move_id IS NOT NULL ORDER BY id",
        &[&moves.ids()],
    ).await?.iter().map(|row| row.try_get(0)).collect::<std::result::Result<_,_>>()
     .map_err(|e| Error::db("Could not read the original demand of this stock move.").with_source(e))?;
    if linked.is_empty() {
        return Ok(());
    }
    let linked = moves.browse(linked)?;
    for field in fields {
        if let Some(value) = vals.get(field)
            && linked
                .cells(tx, field)
                .await?
                .iter()
                .any(|old| old != value)
        {
            return Err(Error::validation(
                "The product, company, transfer and route of an automatic backorder remain with its original demand.",
            ));
        }
    }
    Ok(())
}
