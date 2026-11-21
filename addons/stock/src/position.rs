// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exact current product positions and contributing source domains (E1/E2).

use erp_sdk::ErrorKind;
use erp_sdk::money::{Increment, RoundingMode};
use erp_sdk::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) const MAX_PRODUCTS: usize = 1_000;
pub(super) const MAX_SCOPE_IDS: usize = 256;
const OPEN_STATES: [&str; 4] = ["waiting", "confirmed", "assigned", "partially_available"];
pub(super) const FACT_LIMIT_KEY: &str = "stock.position_max_source_rows";

/// The current position quantities shown for a product.
#[derive(Clone, Copy, Debug)]
pub enum PositionMeasure {
    /// Physical quantity within the selected locations.
    OnHand,
    /// Physical quantity less current reservations.
    FreeToUse,
    /// Scheduled demand entering the selected locations.
    Incoming,
    /// Scheduled demand leaving the selected locations.
    Outgoing,
    /// Physical quantity plus incoming and less outgoing.
    Forecasted,
}

/// The warehouse or location selection for a current position.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PositionScope {
    /// The union of the caller-visible active warehouse roots of the selected companies.
    DefaultWarehouses,
    /// Explicit locations, with or without their descendants.
    Locations(LocationScope),
}

/// A bounded, normalized set of explicit location roots.
#[derive(Clone, Debug)]
pub struct LocationScope {
    roots: Vec<i64>,
    strict: bool,
}

impl PositionScope {
    /// Select roots and include descendants unless `strict` is true.
    ///
    /// # Errors
    /// Invalid or too many location IDs.
    pub fn locations(roots: impl IntoIterator<Item = i64>, strict: bool) -> Result<Self> {
        Ok(Self::Locations(LocationScope {
            roots: normalize(roots, MAX_SCOPE_IDS, "location")?,
            strict,
        }))
    }
}

/// A bounded request for the current positions of selected products.
#[derive(Debug)]
pub struct PositionRequest {
    products: Vec<i64>,
    companies: Option<Vec<i64>>,
    scope: PositionScope,
}

impl PositionRequest {
    /// Build a request. An omitted company set uses the caller's companies. An explicit empty set
    /// reads zero positions for the requested, visible products.
    ///
    /// # Errors
    /// Invalid or too many product or company IDs.
    pub fn new(
        products: impl IntoIterator<Item = i64>,
        companies: Option<&[i64]>,
        scope: PositionScope,
    ) -> Result<Self> {
        Ok(Self {
            products: normalize(products, MAX_PRODUCTS, "product")?,
            companies: companies
                .map(|ids| normalize(ids.iter().copied(), MAX_SCOPE_IDS, "company"))
                .transpose()?,
            scope,
        })
    }
}

fn normalize(ids: impl IntoIterator<Item = i64>, max: usize, kind: &str) -> Result<Vec<i64>> {
    let mut unique = BTreeSet::new();
    for (index, id) in ids.into_iter().enumerate() {
        if index == max {
            return Err(Error::new(
                ErrorKind::ResourceExhausted,
                format!("A current stock position accepts at most {max} {kind} IDs."),
            ));
        }
        if RecId::from_real(id).is_none() {
            return Err(Error::validation(format!(
                "A current stock position needs positive {kind} IDs."
            )));
        }
        unique.insert(id);
    }
    Ok(unique.into_iter().collect())
}

fn scope_denied() -> Error {
    Error::new(
        ErrorKind::Access,
        "The selected stock scope is not accessible.",
    )
}

fn too_many_sources(limit: u64) -> Error {
    Error::new(
        ErrorKind::ResourceExhausted,
        format!("Current stock position exceeds {limit} source rows (setting {FACT_LIMIT_KEY})."),
    )
}

/// Exact quantities in the product unit before display rounding.
#[derive(Debug, Default)]
struct Quantities {
    on_hand: Decimal,
    reserved: Decimal,
    incoming: Decimal,
    outgoing: Decimal,
}

/// Quantities for one product, rounded at the configured Product Unit precision.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProductPosition {
    values: [Decimal; 5],
    digits: u32,
}

impl ProductPosition {
    /// Display value of one measure in the product unit.
    pub fn value(&self, measure: PositionMeasure) -> Decimal {
        self.values[measure as usize]
    }

    /// The configured Product Unit display precision.
    pub fn digits(&self) -> u32 {
        self.digits
    }

    fn round(raw: Quantities, increment: &Increment) -> Result<Self> {
        let round = |quantity| {
            increment
                .quantize(quantity, RoundingMode::HalfAwayFromZero)
                .map_err(|err| Error::validation(format!("Stock quantity rounding failed: {err}")))
        };
        let overflow =
            || Error::validation("Current stock quantity exceeds the supported precision.");
        let free = raw.on_hand.checked_sub(raw.reserved).ok_or_else(overflow)?;
        let incoming = round(raw.incoming)?;
        let outgoing = round(raw.outgoing)?;
        let forecast = raw
            .on_hand
            .checked_add(incoming)
            .and_then(|total| total.checked_sub(outgoing))
            .ok_or_else(overflow)?;
        Ok(Self {
            digits: increment.scale(),
            values: [
                round(raw.on_hand)?,
                round(free)?,
                incoming,
                outgoing,
                round(forecast)?,
            ],
        })
    }
}

/// A source model and the same predicate the quantity used. Search it with the caller's rights.
#[derive(Debug)]
#[non_exhaustive]
pub struct PositionSource {
    model: &'static str,
    domain: Domain,
}

impl PositionSource {
    /// The stock model containing the contributing records.
    pub fn model(&self) -> &'static str {
        self.model
    }

    /// The same source predicate used for the selected product and measure.
    pub fn domain(&self) -> &Domain {
        &self.domain
    }
}

#[derive(Debug)]
struct Domains {
    quants: Domain,
    incoming: Domain,
    outgoing: Domain,
}

impl Domains {
    fn new(
        request: &PositionRequest,
        companies: Vec<i64>,
        roots: Vec<i64>,
        strict: bool,
    ) -> Result<Self> {
        let selected = Domain::field("company_id")?.in_(companies);
        let contains = |field| -> Result<Domain> {
            let field = Domain::field(field)?;
            if roots.is_empty() {
                return Ok(Domain::FALSE);
            }
            Ok(if strict {
                field.in_(roots.clone())
            } else {
                field.child_of(roots.clone())
            })
        };
        let from = contains("location_id")?;
        let destination = contains("location_dest_id")?;
        let to = if strict {
            destination
        } else {
            contains("forecasted_location_id")?
                | (Domain::field("forecasted_location_id")?.eq(false) & destination)
        };
        let base = selected & Domain::field("product_id")?.in_(request.products.clone());
        let open = base.clone() & Domain::field("state")?.in_(OPEN_STATES.to_vec());
        Ok(Self {
            quants: base & from.clone(),
            incoming: open.clone() & to.clone() & !from.clone(),
            outgoing: open & from & !to,
        })
    }
}

/// Current quantities and the domains from which they were read.
#[derive(Debug)]
#[non_exhaustive]
pub struct PositionBatch {
    positions: BTreeMap<i64, ProductPosition>,
    domains: Domains,
}

impl PositionBatch {
    /// An admitted product's current position.
    pub fn position(&self, product: i64) -> Option<&ProductPosition> {
        self.positions.get(&product)
    }

    /// Return each contributing source of the measure. Forecast uses all three source predicates.
    ///
    /// # Errors
    /// The product was not admitted to this result.
    pub fn drill_sources(
        &self,
        product: i64,
        measure: PositionMeasure,
    ) -> Result<Vec<PositionSource>> {
        if !self.positions.contains_key(&product) {
            return Err(scope_denied());
        }
        let product = Domain::field("product_id")?.eq(product);
        let source = |model, domain: &Domain| PositionSource {
            model,
            domain: domain.clone() & product.clone(),
        };
        Ok(match measure {
            PositionMeasure::OnHand | PositionMeasure::FreeToUse => {
                vec![source("stock.quant", &self.domains.quants)]
            }
            PositionMeasure::Incoming => vec![source("stock.move", &self.domains.incoming)],
            PositionMeasure::Outgoing => vec![source("stock.move", &self.domains.outgoing)],
            PositionMeasure::Forecasted => vec![
                source("stock.quant", &self.domains.quants),
                source("stock.move", &self.domains.incoming),
                source("stock.move", &self.domains.outgoing),
            ],
        })
    }
}

async fn admitted_roots(
    env: &Env,
    tx: &mut Transaction,
    companies: &[i64],
    scope: &PositionScope,
) -> Result<(Vec<i64>, bool)> {
    if companies.is_empty() {
        return Ok((Vec::new(), false));
    }
    let (roots, strict) = match scope {
        PositionScope::Locations(explicit) => (explicit.roots.clone(), explicit.strict),
        PositionScope::DefaultWarehouses => {
            let warehouses = env
                .records("stock.warehouse")?
                .search_with(
                    tx,
                    &(Domain::field("company_id")?.in_(companies.to_vec())
                        & Domain::field("active")?.eq(true)),
                    &SearchOptions::new()
                        .limit(MAX_SCOPE_IDS as u64 + 1)
                        .fetch_fields(["view_location_id"]),
                )
                .await?;
            let mut roots = BTreeSet::new();
            if warehouses.len() > MAX_SCOPE_IDS {
                return Err(Error::new(
                    ErrorKind::ResourceExhausted,
                    format!("Current stock position exceeds {MAX_SCOPE_IDS} warehouse roots."),
                ));
            }
            for root in warehouses.cells(tx, "view_location_id").await? {
                let Cell::Id(root) = root else {
                    return Err(Error::internal(
                        "A stock warehouse is missing its location root.",
                    ));
                };
                roots.insert(root);
            }
            (roots.into_iter().collect(), false)
        }
    };
    let company = Domain::field("company_id")?;
    let visible = env
        .records("stock.location")?
        .search(
            tx,
            &(Domain::field("id")?.in_(roots.clone())
                & (company.in_(companies.to_vec()) | company.eq(false))),
        )
        .await?;
    if visible.len() != roots.len() {
        return Err(scope_denied());
    }
    Ok((roots, strict))
}

async fn admitted_groups(
    env: &Env,
    tx: &mut Transaction,
    model: &str,
    group_domain: &Domain,
    aggregates: &[&str],
    max_rows: u64,
    remaining: &mut u64,
) -> Result<Groups> {
    let source = env.records(model)?;
    let count = source
        .search_count(tx, group_domain, Some(*remaining + 1))
        .await?;
    *remaining = remaining
        .checked_sub(count)
        .ok_or_else(|| too_many_sources(max_rows))?;
    source
        ._read_group(
            tx,
            ReadGroup::new(group_domain.clone())
                .groupby(["product_id"])
                .aggregates(aggregates.iter().copied()),
        )
        .await
}

fn group_product(value: &GroupValue) -> Result<i64> {
    if let GroupValue::Records(records) = value
        && let [id] = records.rec_ids()
    {
        return Ok(id.get());
    }
    Err(Error::internal(
        "Current stock position received an invalid product group.",
    ))
}

fn sum(value: &GroupValue) -> Result<Decimal> {
    match value {
        GroupValue::Decimal(value) => Ok(*value),
        _ => Err(Error::internal(
            "Current stock position received an invalid quantity sum.",
        )),
    }
}

/// Read current positions with the caller's ACL and record rules on every source.
///
/// Call inside a repeatable-read transaction on the primary so locations, quantities and source
/// counts share a snapshot.
///
/// # Errors
/// Refuses an inaccessible product or scope, a conflicting database route or isolation, excessive
/// source rows, and numeric or database errors.
pub async fn stock_position(
    env: &Env,
    tx: &mut Transaction,
    request: &PositionRequest,
) -> Result<PositionBatch> {
    let route = tx.query(
        "SELECT pg_is_in_recovery(), current_setting('transaction_isolation') = 'repeatable read'",
        &[],
    ).await?;
    let [route] = route.as_slice() else {
        return Err(Error::internal(
            "Could not determine the stock position read route.",
        ));
    };
    let read_flag = |col| {
        route.try_get::<_, bool>(col).map_err(|error| {
            Error::internal("Could not determine the stock position read route.").with_source(error)
        })
    };
    if read_flag(0_usize)? {
        return Err(Error::validation(
            "Current stock position requires a primary database transaction.",
        ));
    }
    if !read_flag(1_usize)? {
        return Err(Error::validation(
            "Current stock position requires a repeatable-read transaction.",
        ));
    }
    let allowed: Vec<i64> = env.companies()?.iter().map(|id| id.get()).collect();
    let companies = match &request.companies {
        Some(companies) if companies.iter().all(|id| allowed.contains(id)) => companies.clone(),
        Some(_) => return Err(scope_denied()),
        None => normalize(allowed, MAX_SCOPE_IDS, "company")?,
    };
    let env = env.with_context([("active_test", Value::Bool(false))]);
    let products = env
        .records("product.product")?
        .search(tx, &Domain::field("id")?.in_(request.products.clone()))
        .await?;
    if products.len() != request.products.len() {
        return Err(scope_denied());
    }
    let (roots, strict) = admitted_roots(&env, tx, &companies, &request.scope).await?;
    let domains = Domains::new(request, companies, roots, strict)?;
    let mut raw: BTreeMap<i64, Quantities> = request
        .products
        .iter()
        .map(|id| (*id, Quantities::default()))
        .collect();
    let max_rows =
        u64::try_from(erp_sdk::params::get_int(tx, &crate::ADDON, FACT_LIMIT_KEY).await?)
            .map_err(|_| Error::internal("The stock position source limit is invalid."))?;
    let mut remaining = max_rows;

    let quants = admitted_groups(
        &env,
        tx,
        "stock.quant",
        &domains.quants,
        &["quantity:sum", "reserved_quantity:sum"],
        max_rows,
        &mut remaining,
    )
    .await?;
    for row in quants.rows() {
        let [product, on_hand, reserved] = row.as_slice() else {
            return Err(Error::internal(
                "Current stock position received an invalid quant group.",
            ));
        };
        let quantity = raw
            .get_mut(&group_product(product)?)
            .ok_or_else(scope_denied)?;
        quantity.on_hand = sum(on_hand)?;
        quantity.reserved = sum(reserved)?;
    }

    for (domain, into_stock) in [(&domains.incoming, true), (&domains.outgoing, false)] {
        let moves = admitted_groups(
            &env,
            tx,
            "stock.move",
            domain,
            &["product_qty:sum"],
            max_rows,
            &mut remaining,
        )
        .await?;
        for row in moves.rows() {
            let [product, qty] = row.as_slice() else {
                return Err(Error::internal(
                    "Current stock position received an invalid move group.",
                ));
            };
            let quantity = raw
                .get_mut(&group_product(product)?)
                .ok_or_else(scope_denied)?;
            if into_stock {
                quantity.incoming = sum(qty)?;
            } else {
                quantity.outgoing = sum(qty)?;
            }
        }
    }
    let digits = erp_addon_base::decimal_precision::precision_get(&env, tx, "Product Unit").await?;
    let increment = Increment::from_places(digits)
        .map_err(|error| Error::validation(format!("Invalid Product Unit precision: {error}")))?;
    let positions = raw
        .into_iter()
        .map(|(id, amount)| Ok((id, ProductPosition::round(amount, &increment)?)))
        .collect::<Result<_>>()?;
    Ok(PositionBatch { positions, domains })
}
