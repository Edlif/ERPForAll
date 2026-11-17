// SPDX-License-Identifier: MIT OR Apache-2.0
//! The declared settings of the base sales addon (B4).

use erp_addon_base::ir_config_setting::read_values;
use erp_sdk::prelude::*;
use erp_sdk::serde_json;
use erp_sdk::settings::{ScopeKind, SettingDecl, SettingsScope};

/// "Lock confirmed sales": a confirmed order is locked at once.
pub const LOCK_CONFIRMED: &str = "sale.lock_confirmed_sales";

fn settings() -> Result<Vec<SettingDecl>> {
    Ok(vec![
        SettingDecl::boolean(
            LOCK_CONFIRMED,
            "Lock confirmed sales",
            ScopeKind::Global,
            false,
        )?
        .access(&["base.group_user"], &["base.group_system"])?,
    ])
}
erp_sdk::setting_declarations!(settings);

/// The value of [`LOCK_CONFIRMED`], read with the framework's rights (confirming needs no
/// settings group).
///
/// # Errors
/// The read's errors; `Internal` for a stored value that is not a switch.
pub(crate) async fn lock_confirmed(tx: &mut Transaction) -> Result<bool> {
    let env = tx.env().sudo(true);
    let read = read_values(
        &env,
        tx,
        SettingsScope::Global {},
        vec![LOCK_CONFIRMED.to_owned()],
    )
    .await?;
    let read = serde_json::to_value(&read)
        .map_err(|error| Error::internal(format!("sales settings: {error}")))?;
    read["values"]
        .as_array()
        .and_then(|values| values.iter().find(|v| v["key"] == LOCK_CONFIRMED))
        .and_then(|entry| entry["value"].as_bool())
        .ok_or_else(|| Error::internal(format!("{LOCK_CONFIRMED} is not a switch")))
}
