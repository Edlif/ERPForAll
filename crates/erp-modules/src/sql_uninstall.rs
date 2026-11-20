// SPDX-License-Identifier: MIT OR Apache-2.0
//! Compatibility namespace for the native count-only guard runtime.
//! ModuleSystem retains lifecycle orchestration; addons declare guards through the native SDK.
pub(crate) use erp_orm::sql_uninstall::validate;
pub use erp_orm::sql_uninstall::{
    SQL_UNINSTALL_GUARDS, SqlGuardFn, SqlGuardLimits, SqlGuardOutcome, SqlGuardRead,
    SqlUninstallGuard, SqlUninstallGuardManifest,
};
