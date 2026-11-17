// SPDX-License-Identifier: MIT OR Apache-2.0
//! The unit snapshot of a move (6 with amendment C3,
//! 003, 021, 022).
//!
//! A move keeps its demand and quantity in its own unit and, converted HALF-UP at the Product Unit
//! digits, in the product's unit (`product_qty`, `quantity_product_uom`), next to the factors and
//! digits they were converted with (`uom_factor`, `product_uom_factor`, `product_unit_digits`).
//! The move overrides of [`crate::move_guards`] fill those five columns from the unit service
//! ([`erp_addon_uom::stock::capture`], once per distinct pair of units, which also records both
//! units as used) on every create and on every write naming the product, the unit, the demand or
//! the quantity. A move without a unit takes its product's; a product change
//! without a unit takes the new product's unit and keeps the demand.
//!
//! A caller may name a snapshot column only with the value the unit service gives; any other value
//! is refused by name ([`StockError::UnitSnapshot`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use erp_addon_uom::stock::{ProductUnitSnapshot, capture};
use erp_sdk::prelude::*;

use crate::error::StockError;
use crate::move_counters::cell_id;

/// The move columns the unit service fills.
pub const SNAPSHOT_FIELDS: [&str; 5] = [
    "uom_factor",
    "product_uom_factor",
    "product_unit_digits",
    "product_qty",
    "quantity_product_uom",
];

/// The move columns whose write recomputes the snapshot.
pub const UNIT_INPUTS: [&str; 4] = ["product_id", "uom_id", "product_uom_qty", "quantity"];

/// The move line columns the unit service fills (a line has no demand).
pub const LINE_SNAPSHOT_FIELDS: [&str; 4] = [
    "uom_factor",
    "product_uom_factor",
    "product_unit_digits",
    "quantity_product_uom",
];

/// The move line columns whose write recomputes the snapshot.
pub const LINE_UNIT_INPUTS: [&str; 3] = ["product_id", "uom_id", "quantity"];

/// The record kind a snapshot is stamped on: a move (demand and quantity) or a move line
/// (quantity only).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Move,
    Line,
}

impl Target {
    fn model(self) -> &'static str {
        match self {
            Self::Move => "stock.move",
            Self::Line => "stock.move.line",
        }
    }
}

/// A number named in values or read from a move: `Null` is zero, as the reference's `False`
/// float; a non-numeric cell is a programming error, not zero.
fn number(cell: Option<&Cell>, field: &str) -> Result<Decimal> {
    match cell {
        None | Some(Cell::Null) => Ok(Decimal::ZERO),
        Some(Cell::Decimal(d)) => Ok(*d),
        Some(Cell::Int(i)) => Ok(Decimal::from(*i)),
        Some(Cell::Float(f)) => Decimal::try_from(*f).map_err(|e| {
            Error::validation(format!(
                "The {field} of a stock move is not a finite number."
            ))
            .with_source(e)
        }),
        Some(other) => Err(Error::programming(format!(
            "stock.move {field} received {other:?}, expected a number"
        ))),
    }
}

fn context_id(env: &Env, key: &str) -> Option<i64> {
    match env.context().get(key) {
        Some(Value::Int(id)) => Some(id),
        _ => None,
    }
}

/// The unit of each product of `products`.
pub(crate) async fn product_units(
    env: &Env,
    tx: &mut Transaction,
    products: &BTreeSet<i64>,
) -> Result<HashMap<i64, i64>> {
    if products.is_empty() {
        return Ok(HashMap::new());
    }
    let records = env
        .records("product.product")?
        .browse(products.iter().copied())?;
    let units = records.cells(tx, "uom_id").await?;
    let mut out = HashMap::new();
    for (product, unit) in records.ids().into_iter().zip(units) {
        let unit = cell_id(&unit).ok_or_else(|| {
            Error::validation(format!(
                "Product {product} has no unit of measure; a stock move needs one."
            ))
        })?;
        out.insert(product, unit);
    }
    Ok(out)
}

/// One snapshot per distinct (move unit, product unit) pair.
// shortcut: one capture per pair (a few statements each, bounded by the distinct units of the
// call, not by its moves); a batched capture needs a unit-service API taking several pairs
pub(crate) async fn snapshots(
    env: &Env,
    tx: &mut Transaction,
    pairs: BTreeSet<(i64, i64)>,
) -> Result<BTreeMap<(i64, i64), ProductUnitSnapshot>> {
    let mut out = BTreeMap::new();
    for (line, product) in pairs {
        out.insert((line, product), capture(env, tx, line, product).await?);
    }
    Ok(out)
}

/// The snapshot cells for `demand` (moves only) and `quantity` under `snapshot`. An amount given
/// as `None` stays as stored: its product-unit cell is not part of the result.
fn snapshot_cells(
    target: Target,
    snapshot: &ProductUnitSnapshot,
    demand: Option<Decimal>,
    quantity: Option<Decimal>,
) -> Result<Vec<(&'static str, Cell)>> {
    let digits = i32::try_from(snapshot.digits())
        .map_err(|e| Error::internal("Product Unit digits out of range.").with_source(e))?;
    let mut cells = vec![
        ("uom_factor", Cell::Decimal(snapshot.source_factor())),
        (
            "product_uom_factor",
            Cell::Decimal(snapshot.target_factor()),
        ),
        ("product_unit_digits", Cell::Int(digits)),
    ];
    if let Some(quantity) = quantity {
        cells.push((
            "quantity_product_uom",
            Cell::Decimal(snapshot.product_quantity(quantity)?),
        ));
    }
    if target == Target::Move
        && let Some(demand) = demand
    {
        cells.push((
            "product_qty",
            Cell::Decimal(snapshot.product_quantity(demand)?),
        ));
    }
    Ok(cells)
}

/// True when the stored factors and digits of a record are the ones `snapshot` carries: a
/// product-unit amount converted under them is still right while its own amount is unchanged.
fn stored_under(
    snapshot: &ProductUnitSnapshot,
    factor: &Cell,
    product_factor: &Cell,
    digits: &Cell,
) -> Result<bool> {
    if [factor, product_factor, digits]
        .iter()
        .any(|cell| matches!(cell, Cell::Null))
    {
        return Ok(false);
    }
    Ok(
        number(Some(factor), "uom_factor")? == snapshot.source_factor()
            && number(Some(product_factor), "product_uom_factor")? == snapshot.target_factor()
            && number(Some(digits), "product_unit_digits")? == Decimal::from(snapshot.digits()),
    )
}

/// Put the snapshot cells into `vals`; a supplied cell must have the same value.
fn stamp(
    target: Target,
    tx: &Transaction,
    vals: &mut RawVals,
    snapshot: &ProductUnitSnapshot,
    demand: Option<Decimal>,
    quantity: Option<Decimal>,
) -> Result<()> {
    for (field, cell) in snapshot_cells(target, snapshot, demand, quantity)? {
        if let Some(given) = vals.get(field)
            && number(Some(given), field)? != number(Some(&cell), field)?
        {
            let field = tx
                .registry()
                .model(target.model())
                .and_then(|spec| spec.field(field))
                .map(|spec| spec.string.clone())
                .ok_or_else(|| {
                    Error::internal(format!("{} has no field '{field}'", target.model()))
                })?;
            return Err(StockError::UnitSnapshot { field }.into());
        }
        vals.insert(field.into(), cell);
    }
    Ok(())
}

/// Fill the unit and the snapshot of every row of a move create (before the insert). A row without
/// a product is left to the required-field check.
///
/// # Errors
///
/// [`StockError::UnitSnapshot`] for a supplied snapshot value that differs; the unit service's
/// refusals (units of different roots); read and database errors.
pub async fn fill_create(
    records: &Records,
    tx: &mut Transaction,
    vals_list: &mut [RawVals],
) -> Result<()> {
    fill_create_for(Target::Move, records, tx, vals_list).await
}

/// [`fill_create`] for moves or move lines.
pub(crate) async fn fill_create_for(
    target: Target,
    records: &Records,
    tx: &mut Transaction,
    vals_list: &mut [RawVals],
) -> Result<()> {
    let env = records.env();
    let default_product = context_id(env, "default_product_id");
    let default_unit = context_id(env, "default_uom_id");
    let product_of = |vals: &RawVals| match vals.get("product_id") {
        Some(cell) => cell_id(cell),
        None => default_product,
    };
    let products: BTreeSet<i64> = vals_list.iter().filter_map(product_of).collect();
    let units = product_units(env, tx, &products).await?;
    let mut rows = Vec::with_capacity(vals_list.len());
    for vals in vals_list.iter() {
        let Some(product) = product_of(vals) else {
            rows.push(None);
            continue;
        };
        let product_unit = *units
            .get(&product)
            .ok_or_else(|| Error::internal("A product's unit was not read."))?;
        let unit = match vals.get("uom_id") {
            Some(cell) => cell_id(cell),
            None => default_unit,
        }
        .unwrap_or(product_unit);
        rows.push(Some((unit, product_unit)));
    }
    let snapshots = snapshots(env, tx, rows.iter().flatten().copied().collect()).await?;
    for (vals, row) in vals_list.iter_mut().zip(rows) {
        let Some((unit, product_unit)) = row else {
            continue;
        };
        if !vals.contains("uom_id") {
            vals.insert("uom_id".into(), Cell::Id(unit));
        }
        let snapshot = snapshots
            .get(&(unit, product_unit))
            .ok_or_else(|| Error::internal("A unit snapshot was not captured."))?;
        let demand = number(vals.get("product_uom_qty"), "product_uom_qty")?;
        let quantity = number(vals.get("quantity"), "quantity")?;
        stamp(target, tx, vals, snapshot, Some(demand), Some(quantity))?;
    }
    Ok(())
}

/// The values a move write of `vals` gives `records`, grouped so the records sharing every
/// resulting value are written in one call: the written values, the unit a product change takes,
/// and the snapshot. One group per pair of units when the records keep the factors and digits
/// they were stored with: a product-unit amount whose own amount the write does not name is then
/// left as stored, so records that differ only in it share a group. A record whose unit, product,
/// factors or digits change takes both amounts again and groups by them as well.
///
/// # Errors
///
/// As [`fill_create`].
pub async fn write_groups(
    records: &Records,
    tx: &mut Transaction,
    vals: &RawVals,
) -> Result<Vec<(Vec<i64>, RawVals)>> {
    write_groups_for(Target::Move, records, tx, vals).await
}

/// What the records of one write group share: the unit, the product's unit, and each amount the
/// group converts again (`None`: left as stored).
type GroupKey = (i64, i64, Option<Decimal>, Option<Decimal>);

/// [`write_groups`] for moves or move lines.
pub(crate) async fn write_groups_for(
    target: Target,
    records: &Records,
    tx: &mut Transaction,
    vals: &RawVals,
) -> Result<Vec<(Vec<i64>, RawVals)>> {
    let env = records.env();
    let ids = records.ids();
    let old_products = records.cells(tx, "product_id").await?;
    let old_units = records.cells(tx, "uom_id").await?;
    let old_demands = match target {
        Target::Move => records.cells(tx, "product_uom_qty").await?,
        Target::Line => vec![Cell::Null; ids.len()],
    };
    let old_quantities = records.cells(tx, "quantity").await?;
    let old_factors = records.cells(tx, "uom_factor").await?;
    let old_product_factors = records.cells(tx, "product_uom_factor").await?;
    let old_digits = records.cells(tx, "product_unit_digits").await?;
    let new_product = vals.get("product_id").map(cell_id);
    let new_unit = vals.get("uom_id").map(cell_id);
    let mut rows = Vec::with_capacity(ids.len());
    for index in 0..ids.len() {
        let product = new_product.unwrap_or_else(|| cell_id(&old_products[index]));
        // `None`: a product change without a unit takes the new product's unit
        let unit = match new_unit {
            Some(unit) => Some(unit),
            None if new_product.is_some() => None,
            None => Some(cell_id(&old_units[index])),
        };
        let demand = match vals.get("product_uom_qty") {
            Some(cell) => number(Some(cell), "product_uom_qty")?,
            None => number(Some(&old_demands[index]), "product_uom_qty")?,
        };
        let quantity = match vals.get("quantity") {
            Some(cell) => number(Some(cell), "quantity")?,
            None => number(Some(&old_quantities[index]), "quantity")?,
        };
        rows.push((product, unit, demand, quantity));
    }
    let products: BTreeSet<i64> = rows.iter().filter_map(|row| row.0).collect();
    let units = product_units(env, tx, &products).await?;
    let resolved: Vec<_> = rows
        .into_iter()
        .map(|(product, unit, demand, quantity)| {
            let product_unit = product.and_then(|p| units.get(&p)).copied()?;
            match unit {
                None => Some((product_unit, product_unit, demand, quantity)),
                Some(Some(unit)) => Some((unit, product_unit, demand, quantity)),
                // an emptied unit is left to the required-field check
                Some(None) => None,
            }
        })
        .collect();
    let pairs = resolved.iter().flatten().map(|r| (r.0, r.1)).collect();
    let snapshots = snapshots(env, tx, pairs).await?;
    // an amount the write names (itself or its product-unit column) is always converted again
    let rekeyed = new_product.is_some() || new_unit.is_some();
    let names_demand = vals.contains("product_uom_qty") || vals.contains("product_qty");
    let names_quantity = vals.contains("quantity") || vals.contains("quantity_product_uom");
    let mut groups: BTreeMap<GroupKey, Vec<i64>> = BTreeMap::new();
    let mut untouched = Vec::new();
    for (index, (id, row)) in ids.into_iter().zip(resolved).enumerate() {
        let Some((unit, product_unit, demand, quantity)) = row else {
            untouched.push(id);
            continue;
        };
        let snapshot = snapshots
            .get(&(unit, product_unit))
            .ok_or_else(|| Error::internal("A unit snapshot was not captured."))?;
        let kept = !rekeyed
            && stored_under(
                snapshot,
                &old_factors[index],
                &old_product_factors[index],
                &old_digits[index],
            )?;
        let demand = (names_demand || !kept).then_some(demand);
        let quantity = (names_quantity || !kept).then_some(quantity);
        groups
            .entry((unit, product_unit, demand, quantity))
            .or_default()
            .push(id);
    }
    let mut out = Vec::with_capacity(groups.len() + 1);
    if !untouched.is_empty() {
        out.push((untouched, vals.clone()));
    }
    for ((unit, product_unit, demand, quantity), ids) in groups {
        let mut group = vals.clone();
        if new_product.is_some() && new_unit.is_none() {
            group.insert("uom_id".into(), Cell::Id(unit));
        }
        let snapshot = snapshots
            .get(&(unit, product_unit))
            .ok_or_else(|| Error::internal("A unit snapshot was not captured."))?;
        stamp(target, tx, &mut group, snapshot, demand, quantity)?;
        out.push((ids, group));
    }
    Ok(out)
}
