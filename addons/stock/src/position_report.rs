// SPDX-License-Identifier: MIT OR Apache-2.0
//! Product position through the existing dynamic reports backend.

use crate::position::{
    MAX_PRODUCTS, MAX_SCOPE_IDS, PositionMeasure, PositionRequest, PositionScope, stock_position,
};
use erp_addon_reporting::{
    definition::ReportDef,
    line::{
        Cell as ReportCell, CellKind, DrillTarget, ExpandPage, Expansion, Line, LineId, LineKind,
        Segment,
    },
    options::Options,
    provider::{ProviderSpec, ReportCx, ReportProvider},
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, ErrorKind};
use std::collections::{BTreeMap, BTreeSet};

struct ProductPositionReport;
const SPEC: ProviderSpec = ProviderSpec::new("stock.current_product_position", 1).server_search();

struct Filters {
    scope: PositionScope,
    category: Option<i64>,
}

fn id(value: &Json, key: &str) -> Result<i64> {
    value
        .as_i64()
        .filter(|id| RecId::from_real(*id).is_some())
        .ok_or_else(|| {
            Error::validation(format!(
                "Stock position option {key} needs a positive record ID."
            ))
        })
}

fn filters(options: &Options) -> Result<Filters> {
    for key in options.provider.keys() {
        if !matches!(key.as_str(), "location_ids" | "strict" | "category_id") {
            return Err(Error::validation(format!(
                "Stock position does not support option {key}."
            )));
        }
    }
    let strict = options
        .provider
        .get("strict")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| Error::validation("Stock position option strict must be boolean."))
        })
        .transpose()?
        .unwrap_or(false);
    let scope = match options.provider.get("location_ids") {
        Some(values) => {
            let roots = values.as_array().ok_or_else(|| {
                Error::validation("Stock position option location_ids must be a list.")
            })?;
            if roots.len() > MAX_SCOPE_IDS {
                return Err(Error::new(
                    ErrorKind::ResourceExhausted,
                    format!(
                        "A current stock position accepts at most {MAX_SCOPE_IDS} location IDs."
                    ),
                ));
            }
            let roots = roots
                .iter()
                .map(|value| id(value, "location_ids"))
                .collect::<Result<Vec<_>>>()?;
            PositionScope::locations(roots, strict)?
        }
        None if !strict => PositionScope::DefaultWarehouses,
        None => {
            return Err(Error::validation(
                "Stock position option strict needs location_ids.",
            ));
        }
    };
    let category = options
        .provider
        .get("category_id")
        .filter(|value| !value.is_null())
        .map(|value| id(value, "category_id"))
        .transpose()?;
    Ok(Filters { scope, category })
}

fn product_line(report: i64, product: i64) -> LineId {
    LineId::root(report).child(Segment::group(
        "product_id",
        "product.product",
        &product.to_string(),
    ))
}

async fn products(
    cx: &mut ReportCx<'_>,
    options: &Options,
    category: Option<i64>,
    product: Option<i64>,
) -> Result<Records> {
    let (env, tx) = cx.parts();
    let company = Domain::field("company_id")?;
    let mut domain = Domain::field("is_storable")?.eq(true)
        & (company.in_(options.companies.clone()) | company.eq(false));
    if let Some(category) = category {
        let category_visible = env
            .records("product.category")?
            .search_count(tx, &Domain::field("id")?.eq(category), Some(1))
            .await?;
        if category_visible != 1 {
            return Err(Error::new(
                ErrorKind::Access,
                "The selected product category is not accessible.",
            ));
        }
        domain = domain & Domain::field("categ_id")?.child_of(vec![category]);
    }
    if let Some(id) = product {
        // Search narrows rows. The issued product ID, category, companies and stock scope
        // admit the drill independently of the caller's label formatting.
        domain = domain & Domain::field("id")?.eq(id);
    } else if !options.search.is_empty() {
        domain = domain
            & (Domain::field("name")?.ilike(options.search.as_str())
                | Domain::field("default_code")?.ilike(options.search.as_str()));
    }
    let selected = env
        .records("product.product")?
        .search_with(
            tx,
            &domain,
            &SearchOptions::new()
                .order("id")
                .limit(MAX_PRODUCTS as u64 + 1),
        )
        .await?;
    if selected.len() > MAX_PRODUCTS {
        return Err(Error::new(
            ErrorKind::ResourceExhausted,
            format!(
                "Stock position has more than {MAX_PRODUCTS} products. Narrow the search or category."
            ),
        ));
    }
    Ok(selected)
}

fn measure(label: &str) -> Result<PositionMeasure> {
    Ok(match label {
        "on_hand" => PositionMeasure::OnHand,
        "free_to_use" => PositionMeasure::FreeToUse,
        "incoming" => PositionMeasure::Incoming,
        "outgoing" => PositionMeasure::Outgoing,
        "forecasted" => PositionMeasure::Forecasted,
        _ => {
            return Err(Error::validation(format!(
                "Stock position does not have a {label} quantity."
            )));
        }
    })
}

fn label(cell: &Cell) -> Result<&str> {
    match cell {
        Cell::Str(name) => Ok(name.as_ref()),
        _ => Err(Error::internal("A stock position label could not be read.")),
    }
}

async fn report_rows(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
) -> Result<Expansion> {
    let Filters { scope, category } = filters(options)?;
    let products = products(cx, options, category, None).await?;
    let (env, tx) = cx.parts();
    let request = PositionRequest::new(products.ids(), Some(&options.companies), scope)?;
    let positions = stock_position(env, tx, &request).await?;
    let names = products.cells(tx, "display_name").await?;
    let categories: Vec<Option<i64>> = products
        .cells(tx, "categ_id")
        .await?
        .into_iter()
        .map(|cell| match cell {
            Cell::Id(id) => Ok(Some(id)),
            Cell::Null | Cell::Bool(false) => Ok(None),
            _ => Err(Error::internal("A stock product has an invalid category.")),
        })
        .collect::<Result<_>>()?;
    let ids: BTreeSet<_> = categories.iter().copied().flatten().collect();
    let records = env
        .records("product.category")?
        .search(
            tx,
            &Domain::field("id")?.in_(ids.into_iter().collect::<Vec<_>>()),
        )
        .await?;
    let names_by_category: BTreeMap<i64, Cell> = records
        .ids()
        .into_iter()
        .zip(records.cells(tx, "display_name").await?)
        .collect();
    let mut lines = Vec::with_capacity(products.len());
    for ((product, name), category) in products.ids().into_iter().zip(names).zip(categories) {
        let position = positions
            .position(product)
            .ok_or_else(|| Error::internal("A selected stock position disappeared."))?;
        let category = category
            .and_then(|id| names_by_category.get(&id))
            .map(label)
            .transpose()?;
        let cells: Vec<_> = report
            .columns
            .iter()
            .map(|column| {
                if column.label == "category" {
                    return Ok(category
                        .map_or_else(ReportCell::empty, ReportCell::text)
                        .with_drillable(false));
                }
                let selected_measure = measure(&column.label)?;
                let value = position.value(selected_measure);
                let cell = ReportCell::number(CellKind::Decimal, Some(value), position.digits());
                let cell = if column.blank_if_zero {
                    cell.blank_if_zero()
                } else {
                    cell
                };
                Ok(cell.with_drillable(!matches!(selected_measure, PositionMeasure::Forecasted)))
            })
            .collect::<Result<_>>()?;
        let mut row = Line::new(
            product_line(report.id, product),
            label(&name)?,
            LineKind::Detail,
            cells,
        );
        row.caret = vec!["open"];
        lines.push(row);
    }
    Ok(Expansion::new(lines))
}

async fn report_source(
    cx: &mut ReportCx<'_>,
    report: &ReportDef,
    options: &Options,
    line: &LineId,
    target: &DrillTarget,
) -> Result<Json> {
    let refused =
        || Error::validation("The selected product is outside the stock position report.");
    let [_, segment] = line.segments() else {
        return Err(refused());
    };
    let product = segment.value().parse::<i64>().map_err(|_| refused())?;
    if RecId::from_real(product).is_none() || line != &product_line(report.id, product) {
        return Err(refused());
    }
    let Filters { scope, category } = filters(options)?;
    let records = products(cx, options, category, Some(product)).await?;
    if records.is_empty() {
        return Err(refused());
    }
    let (env, tx) = cx.parts();
    let request = PositionRequest::new([product], Some(&options.companies), scope)?;
    let positions = stock_position(env, tx, &request).await?;
    let name = records.cell(tx, "display_name").await?;
    match target {
        DrillTarget::Item(item) if item == "open" => Ok(json!({
            "type": "ir.actions.act_window", "name": label(&name)?, "res_model": "product.product",
            "res_id": product, "target": "new", "views": [[false, "form"]]
        })),
        DrillTarget::Cell(index) => {
            let column = report
                .columns
                .get(*index)
                .ok_or_else(|| Error::validation("Stock position has no such column."))?;
            let sources = positions.drill_sources(product, measure(&column.label)?)?;
            let [source] = sources.as_slice() else {
                return Err(Error::validation(
                    "Forecasted stock combines current stock and open movements. Open its On Hand, Incoming or Outgoing source.",
                ));
            };
            Ok(json!({
                "type": "ir.actions.act_window", "name": column.name, "res_model": source.model(),
                "target": "current", "views": [[false, "list"], [false, "form"]],
                "domain": source.domain().to_json()?, "context": {"active_test": false}
            }))
        }
        _ => Err(Error::validation(
            "Stock position has no such drill action.",
        )),
    }
}

impl ReportProvider for ProductPositionReport {
    fn spec(&self) -> &ProviderSpec {
        &SPEC
    }

    fn init_options<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a mut Options,
        _strict: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if options.period.from.is_some()
                || options.period.to != cx.env().context_today()?
                || options.column_groups.len() != 1
            {
                return Err(Error::validation(
                    "Product Stock Position reads current stock. Open it with Today to refresh.",
                ));
            }
            filters(options)?;
            for column in &report.columns {
                let kind = if column.label == "category" {
                    CellKind::Text
                } else {
                    measure(&column.label)?;
                    CellKind::Decimal
                };
                if column.kind != kind {
                    return Err(Error::validation(format!(
                        "Stock position column {} uses a different value kind.",
                        column.label
                    )));
                }
            }
            Ok(())
        })
    }

    fn lines<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(report_rows(cx, report, options))
    }

    fn expand<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        _options: &'a Options,
        _line: &'a LineId,
        _page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async { Err(Error::validation("A stock position line has no children.")) })
    }

    fn drill<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(report_source(cx, report, options, line, target))
    }
}

erp_addon_reporting::provider_declarations!(ProductPositionReport);
