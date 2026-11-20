// SPDX-License-Identifier: MIT OR Apache-2.0
//! Company and category costing defaults and the normal product method read.

use erp_addon_base::ResCompany;
use erp_addon_product::{ProductCategory, ProductTemplate};
use erp_sdk::prelude::*;
use erp_sdk::{Load, Value};
use std::collections::{BTreeMap, BTreeSet};

#[extend(ResCompany, addon = "account")]
struct CompanyCosting {
    #[field(selection, string = "Default costing method", selection = [("standard", "Standard price"), ("average", "Average cost")], default = "standard", required = true, company_default_for = "product.category.property_cost_method")]
    cost_method: Option<Arc<str>>,
}

#[extend(ProductCategory, addon = "account")]
struct CategoryCosting {
    #[field(selection, string = "Costing method", selection = [("standard", "Standard price"), ("average", "Average cost")], company_dependent = true)]
    property_cost_method: Option<Arc<str>>,
}

#[extend(ProductTemplate, addon = "account")]
struct ProductCosting {
    #[field(selection, string = "Costing method", selection = [("standard", "Standard price"), ("average", "Average cost")], compute = "_compute_cost_method")]
    cost_method: Option<Arc<str>>,
}

#[methods(ProductTemplate, addon = "account")]
impl ProductCosting {
    #[compute(depends = ["company_id", "categ_id", "categ_id.property_cost_method"], depends_context = ["company", "allowed_company_ids"])]
    async fn _compute_cost_method(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductTemplate>,
    ) -> Result<()> {
        let env = this.env();
        let active = env.company_id()?;
        let owners = this.mapped(tx, ProductTemplate::COMPANY_ID).await?;
        let categories = this.mapped(tx, ProductTemplate::CATEG_ID).await?;
        let mut by_company = BTreeMap::<i64, Vec<(usize, Option<i64>)>>::new();
        for (index, (owner, category)) in owners.into_iter().zip(categories).enumerate() {
            let company = owner.map(RecId::get).or(active).ok_or_else(|| {
                Error::validation("Select a company to read the product costing method.")
            })?;
            by_company
                .entry(company)
                .or_default()
                .push((index, category.map(RecId::get)));
        }
        for (company, products) in by_company {
            let company_env = env.with_company(company)?;
            let category_ids: BTreeSet<i64> = products.iter().filter_map(|(_, id)| *id).collect();
            let rows = company_env
                .records("product.category")?
                .browse(category_ids)?
                .read_with(tx, &["id", "property_cost_method"], Load::Raw)
                .await?;
            let mut costs = BTreeMap::new();
            for row in rows {
                let (Some(Value::Int(id)), Some(cost)) =
                    (row.get("id"), row.get("property_cost_method"))
                else {
                    return Err(Error::invariant(
                        "The product category has no readable costing setting.",
                    ));
                };
                costs.insert(*id, method(cost)?);
            }
            let mut fallback: Option<Arc<str>> = None;
            for (index, category) in products {
                let cost = match category {
                    Some(category) => costs
                        .get(&category)
                        .ok_or_else(|| {
                            Error::access(
                                "The product costing category is not readable in this company.",
                            )
                        })?
                        .clone(),
                    None => None,
                };
                let cost = match cost {
                    Some(cost) => cost,
                    None => match &fallback {
                        Some(fallback) => Arc::clone(fallback),
                        None => {
                            let value = company_env
                                .records("res.company")?
                                .browse([company])?
                                .value(tx, "cost_method")
                                .await?;
                            let value = method(&value)?.ok_or_else(|| {
                                Error::validation("Set a default costing method for this company.")
                            })?;
                            fallback = Some(Arc::clone(&value));
                            value
                        }
                    },
                };
                out.set(index, ProductCosting::COST_METHOD, Some(cost))?;
            }
        }
        Ok(())
    }
}

fn method(value: &Value) -> Result<Option<Arc<str>>> {
    match value {
        Value::Str(method) => Ok(Some(Arc::clone(method))),
        Value::False => Ok(None),
        _ => Err(Error::invariant(
            "The costing method is not a selection value.",
        )),
    }
}
