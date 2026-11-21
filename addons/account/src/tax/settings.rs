// SPDX-License-Identifier: MIT OR Apache-2.0
//! Database-owned admission bound for each tax's invoice/refund distribution.
use erp_sdk::prelude::*;
use erp_sdk::settings::ParameterDecl;

/// Maximum lines per tax and document kind, including the base line.
pub const REPARTITION_LINES_MAX: &str = "account.tax_repartition_lines_max";

pub(crate) fn repartition_lines_parameter() -> Result<ParameterDecl> {
    ParameterDecl::integer(
        REPARTITION_LINES_MAX,
        "Taxes: maximum distribution lines per document kind",
        16,
        (2, 64),
    )
}

fn parameters() -> Result<Vec<ParameterDecl>> {
    Ok(vec![repartition_lines_parameter()?])
}
erp_sdk::system_parameters!(parameters);

pub(crate) async fn limit(tx: &mut Transaction) -> Result<usize> {
    let value = erp_sdk::params::get_int(tx, &crate::ADDON, REPARTITION_LINES_MAX).await?;
    usize::try_from(value)
        .map_err(|error| Error::internal(format!("{REPARTITION_LINES_MAX}: {error}")))
}
