// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{DataError, HtmlImagePolicy, value_error};
use erp_fields::FieldType;
use erp_orm::{Env, Transaction};
use erp_report::DataStage;
use std::sync::Arc;

pub(crate) async fn load(
    tx: &mut Transaction,
    env: &Env,
    stage: &mut DataStage<'_>,
    policy: HtmlImagePolicy,
) -> Result<(), DataError> {
    while let Some(request) = stage.image_request()? {
        let registry = Arc::clone(env.registry());
        let Some(model) = registry.model(request.model()) else {
            stage.reject_images("image target model is unavailable")?;
            continue;
        };
        let Some(field) = model.field(request.field()) else {
            stage.reject_images("image target field is unavailable")?;
            continue;
        };
        if !matches!(field.ty, FieldType::Binary { .. }) {
            stage.reject_images("image target is not a binary field")?;
            continue;
        }
        if field.groups.is_some() && policy == HtmlImagePolicy::UnrestrictedFields {
            return Err(value_error(
                request.model(),
                request.field(),
                "a dynamic group-restricted image requires explicit report attachment acknowledgement",
            ));
        }
        let records = env
            .records(request.model())?
            .browse(request.ids().to_vec())?;
        let (per_value, total) = stage.image_byte_budget()?;
        let rows = records
            .read_binary_bounded(tx, request.field(), per_value, total)
            .await?;
        let bytes = rows
            .iter()
            .filter(|r| r.size() <= per_value)
            .try_fold(0usize, |n, r| n.checked_add(r.size()))
            .ok_or(erp_report::ReportError::Limit {
                name: "html_image_bytes",
                maximum: total,
            })?;
        if bytes > total {
            return Err(erp_report::ReportError::Limit {
                name: "html_image_bytes",
                maximum: total,
            }
            .into());
        }
        let values = rows
            .iter()
            .map(|row| {
                let result = if row.size() > per_value {
                    Err(format!(
                        "report limit image_bytes exceeded: {} > {per_value}",
                        row.size()
                    ))
                } else if let Some(bytes) = row.bytes() {
                    Ok(Arc::clone(bytes))
                } else if row.size() == 0 {
                    Err("image record or value is missing".into())
                } else {
                    return Err(value_error(
                        request.model(),
                        request.field(),
                        "bounded image read omitted an admitted payload",
                    ));
                };
                Ok((row.id(), result))
            })
            .collect::<Result<Vec<_>, DataError>>()?;
        stage.supply_image_results(values)?;
    }
    Ok(())
}
