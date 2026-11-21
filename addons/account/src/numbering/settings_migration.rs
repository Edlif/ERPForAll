// SPDX-License-Identifier: MIT OR Apache-2.0
//! Move the database-wide wait into System Parameters.

use erp_sdk::migrations::SemverVersion;
use erp_sdk::prelude::*;

use super::lock_wait_parameter;
use crate::settings_migration::move_global_integer_parameters;

#[erp_sdk::migration(addon = "account", version = "1.9", stage = post)]
async fn move_sequence_wait(tx: &mut Transaction, _from: &SemverVersion) -> Result<()> {
    move_global_integer_parameters(tx, [lock_wait_parameter()?], "migration:account@1.9").await
}
