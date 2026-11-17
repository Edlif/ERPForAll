// SPDX-License-Identifier: MIT OR Apache-2.0
//! The order number (B2, "Numbering"): the next number of the sequence record with code
//! [`crate::SEQUENCE_CODE`] for the order's company, drawn through `erp_sdk::sequence`. Prefix,
//! padding and counter are fields of that record (`data/ir_sequence_data.xml`), never code.

use erp_sdk::CompanyId;
use erp_sdk::prelude::*;

use crate::order::SEQUENCE_CODE;

/// The next order number of `company` on `date` (the order date; today when `None`).
///
/// # Errors
/// `User` naming the company when neither it nor all companies have a sequence with the order
/// code (the order is never saved without a number); the draw's own errors.
pub(crate) async fn next_number(
    tx: &mut Transaction,
    env: &Env,
    company: i64,
    date: Option<Date>,
) -> Result<String> {
    let scope = Domain::condition("code", Operator::Eq, SEQUENCE_CODE)?
        & (Domain::condition("company_id", Operator::Eq, company)?
            | Domain::condition("company_id", Operator::Eq, false)?);
    let exists = env
        .sudo(true)
        .records("ir.sequence")?
        .search_count(tx, &scope, None)
        .await?
        > 0;
    if !exists {
        // read as the caller: the refusal never names a company the caller may not read
        let name = match env
            .records("res.company")?
            .browse(vec![company])?
            .cell(tx, "name")
            .await?
        {
            Cell::Str(name) => name.to_string(),
            _ => company.to_string(),
        };
        return Err(Error::user(format!(
            "No sequence with the code \"{SEQUENCE_CODE}\" exists for the company \"{name}\". \
             Add one under Technical, Sequences."
        )));
    }
    erp_sdk::sequence::draw_by_code(tx, env, SEQUENCE_CODE, CompanyId::new(company)?, date).await
}
