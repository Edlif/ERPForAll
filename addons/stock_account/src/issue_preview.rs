// SPDX-License-Identifier: MIT OR Apache-2.0
//! Advisory financial layer allocation over the explicitly unbooked current FIFO projection.
//! write/undo/close/book/posting gates remain unimplemented.
use erp_addon_stock::StockQuant;
use erp_sdk::money::{CanonicalFraction, CanonicalInteger, ExactRatio, GridValue, parse_decimal};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{Load, Operation};
use std::collections::{BTreeMap, BTreeSet};

const SCOPE: &str = "primary_unbooked_receipt_projection";

/// Adds a read-only costing comparison to the existing caller-readable stock quant.
#[extend(StockQuant, addon = "stock_account")]
pub struct QuantCostPreview {}

struct Request {
    quantity: ExactRatio,
    lifo: bool,
}
impl Request {
    fn parse(input: &Value) -> Result<Self> {
        let Value::Map(object) = input else {
            return Err(Error::validation(
                "Issue preview request must be an object.",
            ));
        };
        let text = |key: &str| match object.get(key) {
            Some(Value::Str(value)) => Some(value.as_ref()),
            _ => None,
        };
        if object.len() != 3
            || object
                .keys()
                .any(|key| !matches!(key.as_str(), "scope" | "quantity" | "method"))
        {
            return Err(Error::validation(
                "Issue preview request requires exactly scope, method and decimal-text quantity.",
            ));
        }
        if text("scope") != Some(SCOPE) {
            return Err(Error::validation(
                "Issue preview supports only the primary unbooked receipt projection; named cost books are not available.",
            ));
        }
        let lifo = match text("method") {
            Some("fifo") => false,
            Some("lifo") => true,
            _ => {
                return Err(Error::validation(
                    "Issue preview method must be fifo or lifo.",
                ));
            }
        };
        let quantity =
            parse_decimal(text("quantity").ok_or_else(|| {
                Error::validation("Issue preview quantity must be decimal text.")
            })?)?;
        if quantity <= Decimal::ZERO {
            return Err(Error::validation(
                "Issue preview quantity must be greater than zero.",
            ));
        }
        Ok(Self {
            quantity: ExactRatio::from_decimal(quantity)?,
            lifo,
        })
    }
}
fn identifier(row: &BTreeMap<String, Value>, field: &str) -> Result<Option<i64>> {
    match row.get(field) {
        Some(Value::Int(id)) => Ok(Some(*id)),
        Some(Value::False) => Ok(None),
        _ => Err(Error::internal(format!(
            "Issue preview could not read {field}."
        ))),
    }
}
fn required_id(row: &BTreeMap<String, Value>, field: &str) -> Result<i64> {
    identifier(row, field)?
        .ok_or_else(|| Error::internal(format!("Issue preview is missing {field}.")))
}
fn origin_refusal() -> Error {
    Error::access("Issue preview requires readable original cost source records.")
}

fn origin_access_error(error: Error) -> Error {
    match error.kind() {
        erp_sdk::ErrorKind::Access | erp_sdk::ErrorKind::Missing => origin_refusal(),
        _ => error,
    }
}

async fn admit_pending_origins(
    env: &Env,
    tx: &mut Transaction,
    company: i64,
    product: i64,
    max: usize,
    limit: i64,
) -> Result<bool> {
    // Work without a receipt or a layer can still prevent publication for the product.
    let pending = tx
        .query(
            "SELECT DISTINCT s.origin_model,s.origin_id
             FROM stk_value_source s
             LEFT JOIN stk_value_receipt r ON r.source_id=s.id
             WHERE s.company_id=$1 AND s.product_id=$2 AND r.state IS DISTINCT FROM 'applied'
             LIMIT $3",
            &[&company, &product, &limit],
        )
        .await?;
    if pending.len() > max {
        return Err(origin_refusal());
    }

    let mut origins: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for row in &pending {
        let model = row.try_get(0).map_err(|_| origin_refusal())?;
        let id = row.try_get(1).map_err(|_| origin_refusal())?;
        origins.entry(model).or_default().insert(id);
    }
    admit_origins(env, tx, origins).await?;
    Ok(!pending.is_empty())
}

async fn admit_origins(
    env: &Env,
    tx: &mut Transaction,
    origins: BTreeMap<String, BTreeSet<i64>>,
) -> Result<()> {
    for (model, ids) in origins {
        let expected = ids.len();
        let records = env
            .records(&model)
            .and_then(|records| records.browse(ids))
            .map_err(|_| origin_refusal())?;
        records
            .read_rows(tx, &["id"], Load::Raw)
            .await
            .map_err(origin_access_error)?;
        // An id-only read enforces access but can project browsed IDs without checking existence.
        let existing = records.exists(tx).await.map_err(origin_access_error)?;
        if existing.len() != expected {
            return Err(origin_refusal());
        }
    }
    Ok(())
}

fn fraction(value: &ExactRatio) -> Json {
    let canonical = CanonicalFraction::from_ratio(value.clone());
    json!({"numerator":canonical.numerator().to_string(),"denominator":canonical.denominator().to_string()})
}
struct Layer {
    id: i64,
    source: i64,
    time: DateTime,
    quantity: ExactRatio,
    value: ExactRatio,
    unit: ExactRatio,
    origin_model: String,
    origin_id: i64,
    component: String,
    revision: i32,
    consumed_residual: ExactRatio,
}
#[methods(StockQuant, addon = "stock_account")]
impl QuantCostPreview {
    /// Compare exact FIFO/LIFO shares without reserving, consuming, posting or selecting a cost book.
    /// The selected quant authorizes a company/product/lot cost pool, not location availability.
    #[method]
    #[rpc(effect = read)]
    async fn preview_issue_cost(
        this: &RecordSet<StockQuant>,
        tx: &mut Transaction,
        request: Value,
    ) -> Result<Value> {
        let env = this.env();
        if !tx.has_group(env, "stock.group_stock_manager").await? {
            return Err(Error::access(
                "Only stock managers may preview issue costs.",
            ));
        }
        let request = Request::parse(&request)?;
        let records = this.as_records();
        records.ensure_one()?;
        records.check_access(tx, Operation::Read).await?;
        let rows = records
            .read_with(
                tx,
                &[
                    "company_id",
                    "product_id",
                    "lot_id",
                    "location_id",
                    "owner_id",
                ],
                Load::Raw,
            )
            .await?;
        let quant = rows.first().ok_or_else(|| {
            Error::missing("Issue preview needs an existing readable stock line.")
        })?;
        let company = required_id(quant, "company_id")?;
        if !env.companies()?.iter().any(|id| id.get() == company) {
            return Err(Error::access(
                "Issue preview requires a stock line in an allowed company.",
            ));
        }
        let product = required_id(quant, "product_id")?;
        let lot = identifier(quant, "lot_id")?;
        let owner = identifier(quant, "owner_id")?;
        let location = required_id(quant, "location_id")?;
        let companies = env
            .records("res.company")?
            .browse([company])?
            .read_with(tx, &["partner_id", "currency_id"], Load::Raw)
            .await?;
        let company_row = companies
            .first()
            .ok_or_else(|| Error::access("Issue preview requires a readable company."))?;
        let currency = required_id(company_row, "currency_id")?;
        if owner.is_some() && owner != identifier(company_row, "partner_id")? {
            return Err(Error::validation(
                "Issue preview requires company-owned stock in an internal or transit location.",
            ));
        }
        let locations = env
            .records("stock.location")?
            .browse([location])?
            .read_with(tx, &["company_id", "usage"], Load::Raw)
            .await?;
        let location_row = locations
            .first()
            .ok_or_else(|| Error::access("Issue preview requires a readable location."))?;
        if identifier(location_row, "company_id")? != Some(company)
            || !matches!(location_row.get("usage"),Some(Value::Str(usage)) if matches!(usage.as_ref(),"internal"|"transit"))
        {
            return Err(Error::validation(
                "Issue preview requires company-owned stock in an internal or transit location.",
            ));
        }
        let visible_product = env
            .records("product.product")?
            .browse([product])?
            .read_with(tx, &["id"], Load::Raw)
            .await?;
        if visible_product.len() != 1 {
            return Err(Error::access("Issue preview requires a readable product."));
        }
        if let Some(lot) = lot {
            let visible_lot = env
                .records("stock.lot")?
                .browse([lot])?
                .read_with(tx, &["id"], Load::Raw)
                .await?;
            if visible_lot.len() != 1 {
                return Err(Error::access("Issue preview requires a readable lot."));
            }
        }
        let max = erp_addon_reporting::settings::read(env, tx)
            .await?
            .export_max_rows;
        let limit = i64::try_from(
            max.checked_add(1)
                .ok_or_else(|| Error::internal("Issue preview row bound overflow."))?,
        )
        .map_err(|_| Error::internal("Issue preview row bound is out of range."))?;
        // Record access above requires repeatable read. Pending and layer origin checks and
        // their SQL operands all share that snapshot. No row lock or mutation is performed.
        let pending = admit_pending_origins(env, tx, company, product, max, limit).await?;
        // Source IDs come from stk_value_source_seq; they are not the layer's table identity.
        let rows = tx
            .query(
                "SELECT s.generation,s.quantity,s.currency_id,s.method,s.policy,
             l.id,l.source_id,l.kind,l.effective_time,l.original_quantity,l.original_value,
             l.consumed_quantity,l.consumed_value,l.residual_num,l.residual_den,l.generation,
             v.origin_model,v.origin_id,v.component,v.source_revision,v.cost_partition,
             v.method,v.policy,v.effective_time,r.state,r.applied_generation,s.grid_value
             FROM stk_cost_state s
             LEFT JOIN stk_cost_layer l ON l.company_id=s.company_id AND l.product_id=s.product_id
                  AND l.lot_id IS NOT DISTINCT FROM s.lot_id
             LEFT JOIN stk_value_source v ON v.id=l.source_id
             LEFT JOIN stk_value_receipt r ON r.source_id=v.id
             WHERE s.company_id=$1 AND s.product_id=$2 AND s.lot_id IS NOT DISTINCT FROM $3
             ORDER BY l.effective_time,l.source_id LIMIT $4",
                &[&company, &product, &lot, &limit],
            )
            .await?;
        let decode = |error| {
            Error::internal("Issue preview could not decode a cost operand.").with_source(error)
        };
        // Admission precedes all diagnostics driven by protected receipt data.
        let mut origins: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
        for row in &rows {
            if row.try_get::<_, Option<i64>>(5).map_err(decode)?.is_some() {
                origins
                    .entry(row.try_get(16).map_err(decode)?)
                    .or_default()
                    .insert(row.try_get(17).map_err(decode)?);
            }
        }
        admit_origins(env, tx, origins).await?;
        if rows.len() > max {
            return Err(Error::validation(format!(
                "Issue preview exceeds {max} receipt rows (setting reporting.export_max_rows)."
            )));
        }
        let first = rows.first().ok_or_else(|| Error::validation("Issue preview needs a published FIFO cost state for this exact company, product and lot."))?;
        let generation: i32 = first.try_get(0).map_err(decode)?;
        let pool_quantity: Decimal = first.try_get(1).map_err(decode)?;
        let state_currency: i64 = first.try_get(2).map_err(decode)?;
        let method: String = first.try_get(3).map_err(decode)?;
        let policy: String = first.try_get(4).map_err(decode)?;
        let published_grid: GridValue = first.try_get(26).map_err(decode)?;
        let published_grid = published_grid.rescale(18)?.to_ratio()?;
        if pending {
            return Err(Error::validation(
                "Issue preview requires a fully applied receipt cost pool; valuation work is pending.",
            ));
        }
        if method != "fifo" || policy != "cost_scale_v1" || state_currency != currency {
            return Err(Error::validation(
                "Issue preview requires a FIFO cost-scale-v1 state in the company currency.",
            ));
        }
        let mut layers = Vec::with_capacity(rows.len());
        let mut available = ExactRatio::zero();
        let mut pool_value = ExactRatio::zero();
        let mut consumed_residual_sum = ExactRatio::zero();
        for row in &rows {
            let layer_id: Option<i64> = row.try_get(5).map_err(decode)?;
            let Some(id) = layer_id else {
                continue;
            }; // LEFT JOIN explicitly represents an empty pool.
            let source: i64 = row.try_get(6).map_err(decode)?;
            let kind: String = row.try_get(7).map_err(decode)?;
            let time: DateTime = row.try_get(8).map_err(decode)?;
            let original: Decimal = row.try_get(9).map_err(decode)?;
            let original_value: GridValue = row.try_get(10).map_err(decode)?;
            let consumed: Decimal = row.try_get(11).map_err(decode)?;
            let consumed_value: GridValue = row.try_get(12).map_err(decode)?;
            let numerator: CanonicalInteger = row.try_get(13).map_err(decode)?;
            let denominator: CanonicalInteger = row.try_get(14).map_err(decode)?;
            let layer_generation: i32 = row.try_get(15).map_err(decode)?;
            let origin_model: String = row.try_get(16).map_err(decode)?;
            let origin_id: i64 = row.try_get(17).map_err(decode)?;
            let component: String = row.try_get(18).map_err(decode)?;
            let revision: i32 = row.try_get(19).map_err(decode)?;
            let partition: String = row.try_get(20).map_err(decode)?;
            let source_method: String = row.try_get(21).map_err(decode)?;
            let source_policy: String = row.try_get(22).map_err(decode)?;
            let source_time: DateTime = row.try_get(23).map_err(decode)?;
            let applied: Option<String> = row.try_get(24).map_err(decode)?;
            let applied_generation: Option<i32> = row.try_get(25).map_err(decode)?;
            if applied.as_deref() != Some("applied")
                || !matches!(applied_generation,Some(value) if value >= 0 && value <= generation)
            {
                return Err(Error::validation(
                    "Issue preview requires applied source receipts within the published generation.",
                ));
            }
            if !partition.is_empty()
                || kind != "receipt"
                || source_method != "fifo"
                || source_policy != policy
                || time != source_time
            {
                return Err(Error::validation(
                    "Issue preview supports only primary unbooked FIFO receipt sources with matching policy and effective time.",
                ));
            }
            if layer_generation != generation || generation < 0 {
                return Err(Error::validation(
                    "Issue preview cost state and receipt layers have different generations.",
                ));
            }
            if original <= Decimal::ZERO || consumed < Decimal::ZERO || consumed > original {
                return Err(Error::validation(
                    "Issue preview requires positive receipt quantities and valid cumulative consumption.",
                ));
            }
            let original_value = original_value.rescale(18)?.to_ratio()?;
            let consumed_value = consumed_value.rescale(18)?.to_ratio()?;
            let residual = CanonicalFraction::new(numerator, denominator)?;
            consumed_residual_sum = consumed_residual_sum.checked_add(residual.as_ratio())?;
            let unit = original_value.checked_div(&ExactRatio::from_decimal(original)?)?;
            let exact_consumed = unit.checked_mul(&ExactRatio::from_decimal(consumed)?)?;
            if consumed_value.checked_add(residual.as_ratio())? != exact_consumed {
                return Err(Error::validation(
                    "A receipt layer has inconsistent consumed value or fractional residual.",
                ));
            }
            let quantity = ExactRatio::from_decimal(original)?
                .checked_sub(&ExactRatio::from_decimal(consumed)?)?;
            let value = original_value.checked_sub(&exact_consumed)?;
            available = available.checked_add(&quantity)?;
            pool_value = pool_value.checked_add(&value)?;
            layers.push(Layer {
                id,
                source,
                time,
                quantity,
                value,
                unit,
                origin_model,
                origin_id,
                component,
                revision,
                consumed_residual: residual.as_ratio().clone(),
            });
        }
        if available != ExactRatio::from_decimal(pool_quantity)? {
            return Err(Error::validation(
                "Issue preview receipt quantities do not reconcile to the published cost state.",
            ));
        }
        // A consumed residual is exact cumulative consumption minus its grid consumption.
        // Therefore the carrying-value component is its negation: published grid - residual.
        // Receipt-only scope cannot explain a separate valuation adjustment; refuse it.
        let exact_published_pool = published_grid.checked_sub(&consumed_residual_sum)?;
        if pool_value != exact_published_pool {
            return Err(Error::validation(
                "Issue preview receipt values do not reconcile to the published cost state; valuation-only adjustments are not supported.",
            ));
        }
        if request.quantity > available {
            return Err(Error::validation(
                "The receipt cost pool does not cover the requested issue quantity.",
            ));
        }
        if request.lifo {
            layers.reverse();
        }
        let mut remaining = request.quantity.clone();
        let mut total = ExactRatio::zero();
        let mut allocations = Vec::new();
        for layer in layers {
            if remaining.is_zero() {
                break;
            }
            if layer.quantity.is_zero() {
                continue;
            }
            let take = if remaining < layer.quantity {
                remaining.clone()
            } else {
                layer.quantity.clone()
            };
            let value = if take == layer.quantity {
                layer.value.clone()
            } else {
                layer.unit.checked_mul(&take)?
            };
            remaining = remaining.checked_sub(&take)?;
            total = total.checked_add(&value)?;
            allocations.push(json!({
                "layer_id": layer.id,
                "source_id": layer.source,
                "source_sequence": layer.source,
                "prior_consumed_residual": fraction(&layer.consumed_residual),
                "effective_time": layer.time.to_string(),
                "origin": {"model": layer.origin_model, "id": layer.origin_id,
                           "component": layer.component, "revision": layer.revision},
                "quantity": fraction(&take),
                "value": fraction(&value),
                "remaining_quantity": fraction(&layer.quantity.checked_sub(&take)?),
                "remaining_value": fraction(&layer.value.checked_sub(&value)?),
            }));
        }
        Ok(Value::Json(Arc::new(json!({
            "scope": SCOPE,
            "comparison_only": true,
            "quantity_scope": "company_product_lot_cost_pool",
            "location_availability_checked": false,
            "cost_book": null,
            "method": if request.lifo {"lifo"} else {"fifo"},
            "cost_key": {"company_id":company,"product_id":product,"lot_id":lot},
            "currency_id": currency,
            "generation": generation,
            "quantity": fraction(&request.quantity),
            "total": fraction(&total),
            "pool_remaining_quantity": fraction(&available.checked_sub(&request.quantity)?),
            "pool_remaining_value": fraction(&pool_value.checked_sub(&total)?),
            "allocations": allocations,
        }))))
    }
}
