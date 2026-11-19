// SPDX-License-Identifier: MIT OR Apache-2.0
//! The bounds of the report screen as declared settings (design §10): global scope, block "Dynamic
//! reports" on the Settings page. A value outside its range is refused by the settings write.
use erp_addon_base::ir_config_setting::read_values;
use erp_sdk::prelude::*;
use erp_sdk::serde_json;
use erp_sdk::settings::{ScopeKind, SettingDecl};
use erp_sdk::views::wire::SettingsScope;

/// Ceiling of each report's `load_more_limit` (children per unfold page).
pub const MAX_LOAD_MORE_LIMIT: &str = "reporting.max_load_more_limit";
/// Group-by levels below a definition line, and so the depth of a line id.
pub const MAX_UNFOLD_DEPTH: &str = "reporting.max_unfold_depth";
/// Lines in one answer (unfold all, loaded pages of one group, `unfolded` ids).
pub const MAX_SCREEN_LINES: &str = "reporting.max_screen_lines";
/// Comparison periods besides the main one.
pub const MAX_COMPARISON_PERIODS: &str = "reporting.max_comparison_periods";

/// Rows of one synchronous PDF or XLSX export (durable export jobs are).
pub const EXPORT_MAX_ROWS: &str = "reporting.export_max_rows";
/// Bytes of one export request (options only; the browser never posts rows).
pub const EXPORT_MAX_REQUEST_BYTES: &str = "reporting.export_max_request_bytes";

// (key, label, default, floor, hard bound): design §10. The hard bounds keep one answer inside
// 's 8 MiB page and its 16 comparison columns.
const BOUNDS: [(&str, &str, i64, i64, i64); 6] = [
    (
        MAX_LOAD_MORE_LIMIT,
        "Dynamic reports: maximum load-more page",
        2_000,
        10,
        2_000,
    ),
    (
        MAX_UNFOLD_DEPTH,
        "Dynamic reports: maximum unfold depth",
        8,
        2,
        16,
    ),
    (
        MAX_SCREEN_LINES,
        "Dynamic reports: maximum lines on screen",
        10_000,
        500,
        50_000,
    ),
    (
        MAX_COMPARISON_PERIODS,
        "Dynamic reports: maximum comparison periods",
        12,
        1,
        16,
    ),
    (
        EXPORT_MAX_ROWS,
        "Dynamic reports: maximum rows of a PDF or XLSX export",
        50_000,
        100,
        500_000,
    ),
    (
        EXPORT_MAX_REQUEST_BYTES,
        "Dynamic reports: maximum bytes of an export request",
        1_048_576,
        16_384,
        16_777_216,
    ),
];

fn settings() -> Result<Vec<SettingDecl>> {
    BOUNDS
        .iter()
        .map(|(key, label, default, floor, hard)| {
            SettingDecl::integer(key, label, ScopeKind::Global, *default, (*floor, *hard))?
                .block("Dynamic reports")?
                .access(&["base.group_user"], &["base.group_system"])
        })
        .collect()
}
erp_sdk::setting_declarations!(settings);

/// The bounds as read from the settings (their defaults when never set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Bounds {
    /// [`MAX_LOAD_MORE_LIMIT`].
    pub max_load_more_limit: u64,
    /// [`MAX_UNFOLD_DEPTH`].
    pub max_unfold_depth: usize,
    /// [`MAX_SCREEN_LINES`].
    pub max_screen_lines: usize,
    /// [`MAX_COMPARISON_PERIODS`].
    pub max_comparison_periods: u32,
    /// [`EXPORT_MAX_ROWS`].
    pub export_max_rows: usize,
    /// [`EXPORT_MAX_REQUEST_BYTES`].
    pub export_max_request_bytes: usize,
}

/// Read the bounds. The framework reads its own configuration with elevated rights, as the pivot
/// download does: a report reader needs no settings group, and no business data is read there.
///
/// # Errors
/// The settings read's errors; `Internal` when a stored value is not a number in its range.
pub async fn read(env: &Env, tx: &mut Transaction) -> Result<Bounds> {
    let elevated = env.sudo(true);
    let keys = BOUNDS.iter().map(|bound| bound.0.to_owned()).collect();
    let read = read_values(&elevated, tx, SettingsScope::Global {}, keys).await?;
    let read = serde_json::to_value(&read)
        .map_err(|error| Error::internal(format!("reporting settings: {error}")))?;
    let entries = read["values"]
        .as_array()
        .ok_or_else(|| Error::internal("reporting settings: no values"))?;
    let value = |key: &str| -> Result<u64> {
        entries
            .iter()
            .find(|entry| entry["key"] == key)
            .and_then(|entry| entry["value"].as_u64())
            .ok_or_else(|| Error::internal(format!("reporting settings: {key} is not a number")))
    };
    let narrow = |key: &str| -> Result<usize> {
        usize::try_from(value(key)?)
            .map_err(|_| Error::internal(format!("reporting settings: {key} is out of range")))
    };
    Ok(Bounds {
        max_load_more_limit: value(MAX_LOAD_MORE_LIMIT)?,
        max_unfold_depth: narrow(MAX_UNFOLD_DEPTH)?,
        max_screen_lines: narrow(MAX_SCREEN_LINES)?,
        max_comparison_periods: u32::try_from(value(MAX_COMPARISON_PERIODS)?).map_err(|_| {
            Error::internal(format!(
                "reporting settings: {MAX_COMPARISON_PERIODS} is out of range"
            ))
        })?,
        export_max_rows: narrow(EXPORT_MAX_ROWS)?,
        export_max_request_bytes: narrow(EXPORT_MAX_REQUEST_BYTES)?,
    })
}
