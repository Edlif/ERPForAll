// SPDX-License-Identifier: MIT OR Apache-2.0
//! Quant display names use the balance dimensions in the reader's language.
use crate::StockQuant;
use erp_sdk::prelude::*;
use std::collections::BTreeMap;

async fn labels(
    records: &Records,
    tx: &mut Transaction,
    dimension: &str,
    field: &str,
) -> Result<BTreeMap<RecId, Arc<str>>> {
    let targets = records.related(tx, dimension).await?;
    targets
        .rec_ids()
        .iter()
        .copied()
        .zip(targets.values(tx, field).await?)
        .map(|(id, value)| match value {
            Value::Str(name) => Ok((id, name)),
            other => Err(Error::programming(format!(
                "Stock quant {dimension}.{field} expected a label, received {other:?}."
            ))),
        })
        .collect()
}

#[methods(StockQuant, addon = "stock")]
impl StockQuant {
    /// The readable standard label follows location, lot, package and owner. The transfer picker
    /// asks for a compact location, package and lot label through formatted_display_name.
    #[compute(depends = ["location_id.display_name", "location_id.name", "lot_id.name", "package_id.display_name", "owner_id.name"], depends_context = ["formatted_display_name", "lang"])]
    async fn _compute_quant_name(
        this: &RecordSet<StockQuant>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockQuant>,
    ) -> Result<()> {
        let formatted = match this.env().context().get("formatted_display_name") {
            None | Some(Value::False | Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            Some(other) => {
                return Err(Error::validation(format!(
                    "formatted_display_name must be boolean, received {other:?}."
                )));
            }
        };
        let dimensions = if formatted {
            [
                ("location_id", "name"),
                ("package_id", "display_name"),
                ("lot_id", "name"),
            ]
            .as_slice()
        } else {
            [
                ("location_id", "display_name"),
                ("lot_id", "name"),
                ("package_id", "display_name"),
                ("owner_id", "name"),
            ]
            .as_slice()
        };
        let records = this.as_records();
        let mut parts = vec![Vec::new(); records.len()];
        for &(dimension, field) in dimensions {
            let names = labels(records, tx, dimension, field).await?;
            for (parts, id) in parts.iter_mut().zip(records.values(tx, dimension).await?) {
                match id {
                    Value::Int(id) => {
                        let key = RecId::from_real(id).ok_or_else(|| {
                            Error::programming("Invalid stock balance dimension id.")
                        })?;
                        let label = names.get(&key).ok_or_else(|| {
                            Error::programming(format!(
                                "Missing {dimension} label for stock balance."
                            ))
                        })?;
                        parts.push(label.as_ref().to_owned());
                    }
                    Value::False => {}
                    other => {
                        return Err(Error::programming(format!(
                            "Quant field {dimension} expected a relation, received {other:?}."
                        )));
                    }
                }
            }
        }
        for (index, parts) in parts.into_iter().enumerate() {
            let name = if formatted {
                parts.join("\t")
            } else {
                parts.join(" - ")
            };
            out.set(
                index,
                StockQuant::DISPLAY_NAME,
                Some(Arc::<str>::from(name)),
            )?;
        }
        Ok(())
    }
}
