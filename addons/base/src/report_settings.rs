// SPDX-License-Identifier: MIT OR Apache-2.0
//! Database-wide printer grid for report codes.
use erp_sdk::report::ModuleGrid;
use erp_sdk::settings::{ScopeKind, SettingDecl, SettingsRegistry};
use erp_sdk::views::wire::{SettingValue, SettingsScope};
use erp_sdk::{Error, Result, Transaction};

pub(crate) const BARCODE_DPI_KEY: &str = "base.report_barcode_dpi";

fn declarations() -> Result<Vec<SettingDecl>> {
    Ok(vec![
        SettingDecl::integer(
            BARCODE_DPI_KEY,
            "Barcode printer resolution (DPI)",
            ScopeKind::Global,
            i64::from(ModuleGrid::DEFAULT_DPI),
            (
                i64::from(ModuleGrid::MIN_DPI),
                i64::from(ModuleGrid::MAX_DPI),
            ),
        )?
        .block("Reports")?
        .help(&format!(
            "Resolution of the printer used for barcodes, shared by all companies. Default: {}; \
             allowed: {} to {} dots per inch. Choose the physical printer's resolution. \
             An explicit DPI in a report's barcode options overrides this default. \
             Saved changes apply when a new document is prepared.",
            ModuleGrid::DEFAULT_DPI,
            ModuleGrid::MIN_DPI,
            ModuleGrid::MAX_DPI,
        ))?
        .access(&["base.group_system"], &["base.group_system"])?,
    ])
}
erp_sdk::setting_declarations!(declarations);

pub(crate) fn require(registry: &SettingsRegistry) -> Result<()> {
    if registry.get(BARCODE_DPI_KEY).is_none() {
        return Err(Error::programming(format!(
            "The settings declaration {BARCODE_DPI_KEY} of erp-addon-base is not linked into this binary."
        )));
    }
    Ok(())
}

fn invalid() -> Error {
    Error::validation(format!("Stored setting {BARCODE_DPI_KEY} is invalid."))
}

/// Read this one nonsecret configuration value, without granting the printer Settings access.
/// The elevated environment stays local; the transaction's caller and all business reads stay
/// unchanged. The Settings service alone supplies an absent value's declared default and checks
/// stored values. Errors propagate; neither malformed storage nor a failed read selects a default.
pub(crate) async fn barcode_grid(tx: &mut Transaction) -> Result<ModuleGrid> {
    let configuration = tx.env().sudo(true);
    let values = crate::ir_config_setting::read_values(
        &configuration,
        tx,
        SettingsScope::Global {},
        vec![BARCODE_DPI_KEY.to_owned()],
    )
    .await?;
    let [entry] = values.values.as_slice() else {
        return Err(invalid());
    };
    if entry.key != BARCODE_DPI_KEY {
        return Err(invalid());
    }
    let SettingValue::Integer(dpi) = entry.value else {
        return Err(invalid());
    };
    let dpi = u32::try_from(dpi).map_err(|_| invalid())?;
    ModuleGrid::new(dpi).map_err(|_| invalid())
}
