// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared helpers of the non-blocking warning messages that addons add to partners, products and
//! documents: the length bound (one system parameter), text and id decoding of read values, and the
//! readable-record filter of the elevated warning computes.

use erp_sdk::prelude::*;
use erp_sdk::{AddonDescriptor, Operation};

/// The system parameter bounding the length of one warning message, in characters.
pub const MAX_CHARS_KEY: &str = "base.warning_message_max_chars";
/// Default of [`MAX_CHARS_KEY`].
pub const MAX_CHARS_DEFAULT: i64 = 4_000;
/// Floor of [`MAX_CHARS_KEY`].
pub const MAX_CHARS_FLOOR: i64 = 1;
/// Hard bound of [`MAX_CHARS_KEY`].
pub const MAX_CHARS_HARD: i64 = 20_000;

/// The configured maximum length of a warning message, read as addon `addon` (its `crate::ADDON`).
///
/// # Errors
///
/// A stored value outside the declaration; database errors.
pub async fn max_chars(tx: &mut Transaction, addon: &AddonDescriptor) -> Result<usize> {
    let value = erp_sdk::params::get_int(tx, addon, MAX_CHARS_KEY).await?;
    usize::try_from(value).map_err(|error| Error::internal(format!("{MAX_CHARS_KEY}: {error}")))
}

/// Refuse a warning message longer than [`MAX_CHARS_KEY`] allows. `values` are the raw values of
/// the message field; an empty value is accepted.
///
/// # Errors
///
/// `Validation` for a message that is too long; `Programming` for a value that is neither text nor
/// empty.
pub async fn check_length(
    tx: &mut Transaction,
    addon: &AddonDescriptor,
    field_label: &str,
    values: Vec<Value>,
) -> Result<()> {
    let max = max_chars(tx, addon).await?;
    for value in values {
        match value {
            Value::False => {}
            Value::Str(text) => {
                if text.chars().count() > max {
                    return Err(Error::validation(format!(
                        "{field_label} cannot exceed {max} characters ({MAX_CHARS_KEY})."
                    )));
                }
            }
            other => {
                return Err(Error::programming(format!(
                    "{field_label} expected text or empty value, received {other:?}."
                )));
            }
        }
    }
    Ok(())
}

/// The text of a read value; an empty or whitespace-only message is none, so no blank banner shows.
///
/// # Errors
///
/// `Programming` for a value that is neither text nor empty.
pub fn non_empty_text(context: &str, value: Value) -> Result<Option<Arc<str>>> {
    match value {
        Value::False => Ok(None),
        Value::Str(text) => Ok((!text.trim().is_empty()).then_some(text)),
        other => Err(Error::programming(format!(
            "{context} expected text or empty value, received {other:?}."
        ))),
    }
}

/// The record id of a read many-to-one value.
///
/// # Errors
///
/// `Programming` for a value that is neither an id nor empty.
pub fn optional_rec_id(context: &str, value: Value) -> Result<Option<RecId>> {
    match value {
        Value::False => Ok(None),
        Value::Int(id) => RecId::from_real(id)
            .ok_or_else(|| Error::programming(format!("{context} had invalid record id {id}.")))
            .map(Some),
        other => Err(Error::programming(format!(
            "{context} expected a relation id or empty value, received {other:?}."
        ))),
    }
}

/// The ids among `ids` of `model` that the caller of `env` may read: `env` is the elevated
/// environment of a `compute_sudo` field, and the caller is the same environment without the
/// elevation (the model check and the record rules). A record the caller cannot read is left out.
///
/// # Errors
///
/// Failures that are not access refusals.
pub async fn readable_ids(
    tx: &mut Transaction,
    env: &Env,
    model: &str,
    ids: Vec<RecId>,
) -> Result<Vec<RecId>> {
    if ids.is_empty() {
        return Ok(ids);
    }
    let visible = env
        .sudo(false)
        .records(model)?
        .browse_ids(ids)
        .filtered_access(tx, Operation::Read)
        .await?;
    Ok(visible.rec_ids().to_vec())
}
