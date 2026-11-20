// SPDX-License-Identifier: MIT OR Apache-2.0
//! Named precision admission shared by planning, loading and evaluation.
use crate::{ReportError, program::Code};
use erp_expr::{Expr, Literal};
use std::collections::BTreeSet;

pub(crate) const OPTION: &str = "decimal_precision";
pub(crate) const LITERAL_REQUIRED: &str =
    "widget option `decimal_precision` requires a literal string in a static options dictionary";

// These are the base precision metadata contract, not configurable formatting choices.
fn valid_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
}

/// A named precision row supplied by the caller-rights data adapter.
/// Construction checks metadata bounds; the stage checks membership and completeness.
#[derive(Debug)]
pub struct LoadedPrecision {
    pub(crate) name: String,
    pub(crate) digits: u8,
}
impl LoadedPrecision {
    /// Admit one stored usage name and its digits (0–28, the base precision model's range).
    ///
    /// # Errors
    /// [`ReportError::Data`] for a name outside 1–128 bytes or digits outside 0–28.
    pub fn new(name: &str, digits: i64) -> Result<Self, ReportError> {
        if !valid_name(name) {
            return Err(ReportError::Data {
                reason: "decimal precision name must be 1 to 128 bytes long".into(),
            });
        }
        let digits = u8::try_from(digits)
            .ok()
            .filter(|digits| *digits <= 28)
            .ok_or_else(|| ReportError::Data {
                reason: format!(
                    "decimal precision `{name}` digits must be in 0..=28; got {digits}"
                ),
            })?;
        Ok(Self {
            name: name.into(),
            digits,
        })
    }
}

/// None means this expression declares no literal precision key. Runtime must still refuse a
/// dynamic mapping that introduces that key. A declared precision requires unambiguous keys.
pub(crate) fn literal_name(code: &Code) -> Result<Option<&str>, &'static str> {
    let Expr::Dict(entries) = &code.expr else {
        return Ok(None);
    };
    if !entries
        .iter()
        .any(|(key, _)| matches!(key, Expr::Literal(Literal::Str(key)) if key == OPTION))
    {
        return Ok(None);
    }
    let mut keys = BTreeSet::new();
    let mut name = None;
    for (key, value) in entries {
        let Expr::Literal(Literal::Str(key)) = key else {
            return Err("widget option `decimal_precision` requires unique literal option keys");
        };
        if !keys.insert(key.as_str()) {
            return Err("widget option `decimal_precision` requires unique literal option keys");
        }
        if key == OPTION {
            let Expr::Literal(Literal::Str(value)) = value else {
                return Err(LITERAL_REQUIRED);
            };
            if !valid_name(value) {
                return Err("widget option `decimal_precision` name must be 1 to 128 bytes long");
            }
            name = Some(value.as_str());
        }
    }
    Ok(name)
}

pub(crate) fn missing(name: &str) -> ReportError {
    ReportError::Data {
        reason: format!("decimal precision `{name}` was not loaded for this report"),
    }
}
