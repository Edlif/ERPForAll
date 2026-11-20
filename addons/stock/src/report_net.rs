// SPDX-License-Identifier: MIT OR Apache-2.0
//! Directional net quantities for the dynamic Moves Analysis report.
use erp_addon_reporting::functions::{NativeRequest, NativeValues};
use erp_addon_reporting::options::Options;
use erp_addon_reporting::provider::ReportCx;
use erp_sdk::BoxFuture;
use erp_sdk::prelude::*;
use erp_sdk::serde_json::json;
use std::collections::BTreeMap;

erp_addon_reporting::expression_function_declarations!(
    NET_MOVEMENT,
    "stock.net_movement",
    net,
    sources
);

fn directions(model: &str) -> Result<(Domain, Domain)> {
    if model != "stock.move" {
        return Err(Error::validation("Net movement requires stock.move."));
    }
    let locations = ["internal", "transit"];
    let source = Domain::from_json(&json!([["location_id.usage", "in", locations]]))?;
    let destination = Domain::from_json(&json!([["location_dest_id.usage", "in", locations]]))?;
    let done = Domain::from_json(&json!([["state", "=", "done"]]))?;
    Ok((
        done.clone() & !source.clone() & destination.clone(),
        done & source & !destination,
    ))
}

fn sources<'a>(
    _env: &'a Env,
    _tx: &'a mut Transaction,
    _options: &'a Options,
    model: &'a str,
) -> BoxFuture<'a, Result<Domain>> {
    Box::pin(async move {
        let (incoming, outgoing) = directions(model)?;
        Ok(incoming | outgoing)
    })
}

fn net<'a>(
    cx: &'a mut ReportCx<'_>,
    _options: &'a Options,
    request: NativeRequest<'a>,
) -> BoxFuture<'a, Result<NativeValues>> {
    Box::pin(async move {
        let (incoming, outgoing) = directions(request.model)?;
        let Some(field @ ("quantity_product_uom" | "product_qty")) = request.field else {
            return Err(Error::validation(
                "Net movement requires the actual quantity or demand in the product unit.",
            ));
        };
        if request.group.is_some_and(|g| g != "product_id") {
            return Err(Error::validation(
                "Group net movement by product to keep quantities in their own units.",
            ));
        }
        let records = cx.env().records(request.model)?;
        let mut amounts = BTreeMap::<Option<i64>, Decimal>::new();
        for (domain, received) in [(incoming, true), (outgoing, false)] {
            let args = ReadGroup::new(request.domain.clone() & domain)
                .groupby(request.group.into_iter().map(str::to_owned))
                .aggregates([format!("{field}:sum")]);
            let groups = records._read_group(cx.tx(), args).await?;
            for row in groups.rows() {
                let key = if request.group.is_some() {
                    Some(
                        row.first()
                            .and_then(GroupValue::record_id)
                            .ok_or_else(|| Error::internal("A movement group has no product."))?
                            .get(),
                    )
                } else {
                    None
                };
                let value = match row.last() {
                    Some(GroupValue::Decimal(n)) => *n,
                    Some(GroupValue::False | GroupValue::Null) => Decimal::ZERO,
                    other => {
                        return Err(Error::internal(format!(
                            "Movement quantity expected a decimal, received {other:?}."
                        )));
                    }
                };
                let total = amounts.entry(key).or_default();
                *total = if received {
                    total.checked_add(value)
                } else {
                    total.checked_sub(value)
                }
                .ok_or_else(|| Error::validation("Net movement is too large to represent."))?;
            }
        }
        Ok(amounts.into_iter().collect())
    })
}
