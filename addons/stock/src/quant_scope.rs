// SPDX-License-Identifier: MIT OR Apache-2.0
//! On-hand quant scope uses location hierarchy and the active warehouse ownership projection.
use crate::StockQuant;
use erp_sdk::prelude::*;
use std::collections::BTreeSet;

fn selected(env: &Env, key: &str, alternate: &str) -> Result<Option<Vec<i64>>> {
    let value = env
        .context()
        .get(key)
        .or_else(|| env.context().get(alternate));
    let id = |value: &Value| match value {
        Value::Int(n) if *n > 0 => Ok(*n),
        _ => Err(Error::validation(format!(
            "Select {key} by its record id when filtering on-hand stock."
        ))),
    };
    match value {
        None | Some(Value::False | Value::Bool(false)) => Ok(None),
        Some(Value::List(items)) => items.iter().map(id).collect::<Result<_>>().map(Some),
        Some(item) => Ok(Some(vec![id(&item)?])),
    }
}

fn on_hand(env: &Env) -> Result<Domain> {
    let companies: Vec<i64> = env.companies()?.iter().map(|id| id.get()).collect();
    let warehouses = selected(env, "warehouse_id", "search_warehouse")?;
    let locations = selected(env, "location", "search_location")?;
    let strict = match env.context().get("strict") {
        Some(Value::Bool(flag)) => flag,
        None | Some(Value::False) => false,
        Some(_) => return Err(Error::validation("Stock location strict must be boolean.")),
    };
    let within_warehouse = warehouses.is_some() || locations.is_none();
    let mut scope = Domain::TRUE;
    if within_warehouse {
        let mut warehouse =
            Domain::field("company_id")?.in_(companies) & Domain::field("active")?.eq(true);
        if let Some(ids) = warehouses {
            warehouse = warehouse & Domain::field("id")?.in_(ids);
        }
        let relation = if strict && locations.is_none() {
            "warehouse_view_ids"
        } else {
            "warehouse_id"
        };
        scope = Domain::field("location_id")?.any(Domain::field(relation)?.any(warehouse));
    }
    if let Some(ids) = locations {
        let location = Domain::field("location_id")?;
        scope = scope
            & if strict {
                location.in_(ids)
            } else {
                location.child_of(ids)
            };
    }
    Ok(scope)
}

#[methods(StockQuant, addon = "stock")]
impl StockQuant {
    #[compute(depends = ["location_id.warehouse_id", "location_id.parent_path"], depends_context = ["allowed_company_ids", "warehouse_id", "search_warehouse", "location", "search_location", "strict"])]
    async fn _compute_on_hand(
        this: &RecordSet<StockQuant>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockQuant>,
    ) -> Result<()> {
        let ids = this.as_records().ids();
        let scope = on_hand(this.env())? & Domain::field("id")?.in_(ids);
        let selected: BTreeSet<_> = this
            .as_records()
            .search(tx, &scope)
            .await?
            .rec_ids()
            .iter()
            .copied()
            .collect();
        for (index, id) in this.rec_ids().iter().enumerate() {
            out.set(index, StockQuant::ON_HAND, selected.contains(id))?;
        }
        Ok(())
    }

    #[search_method]
    fn _search_on_hand(
        req: &FieldSearchRequest<'_>,
        cx: &mut FieldSearchContext<'_>,
    ) -> Result<Domain> {
        use erp_sdk::domain::Value as Operand;
        let invalid = || Error::validation("On Hand accepts boolean equality or membership.");
        let mut keep = [false; 2];
        let mut add = |v: &Operand| match v {
            Operand::Bool(b) => {
                keep[usize::from(*b)] = true;
                Ok(())
            }
            _ => Err(invalid()),
        };
        match (req.operator(), req.operand()) {
            (Operator::Eq | Operator::Ne, value) => add(value)?,
            (Operator::In | Operator::NotIn, Operand::List(items)) => {
                for value in items.iter() {
                    add(value)?;
                }
            }
            (Operator::In | Operator::NotIn, Operand::Set(items)) => {
                for value in items.iter() {
                    add(value)?;
                }
            }
            _ => return Err(invalid()),
        }
        if matches!(req.operator(), Operator::Ne | Operator::NotIn) {
            keep = keep.map(|value| !value);
        }
        match keep {
            [false, false] => Ok(Domain::FALSE),
            [true, true] => Ok(Domain::TRUE),
            [false, true] => on_hand(cx.env()),
            [true, false] => Ok(!on_hand(cx.env())?),
        }
    }
}
