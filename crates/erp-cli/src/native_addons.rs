// SPDX-License-Identifier: MIT OR Apache-2.0
//! The native addons the `erp` binary links: one list for the serving node, the lifecycle
//! commands and the identity receipts (`names.rs`, read by `build.rs`).
use erp_registry::AddonDescriptor;

mod names;
pub use names::{DECLARED_ONLY, LINKED, NOT_LINKED};

/// The descriptors of [`LINKED`], in the same order. Every native composition site passes
/// these and [`SQL_MANIFESTS`] to the module-system builder.
pub static DESCRIPTORS: &[&AddonDescriptor] = &[
    &erp_addon_base::ADDON,
    &erp_addon_barcodes::ADDON,
    &erp_addon_uom::ADDON,
    &erp_addon_mail::ADDON,
    &erp_addon_product::ADDON,
    &erp_addon_product_mail::ADDON,
    &erp_addon_web::ADDON,
    &erp_addon_acceptance_notes::ADDON,
    &erp_addon_theme_office_blue::ADDON,
    &erp_addon_analytic::ADDON,
    &erp_addon_account::ADDON,
    &erp_addon_account_mail::ADDON,
    &erp_addon_purchase::ADDON,
    &erp_addon_purchase_mail::ADDON,
    &erp_addon_barcodes_gs1_nomenclature::ADDON,
    &erp_addon_reporting::ADDON,
    &erp_addon_account_reporting::ADDON,
    &erp_addon_stock::ADDON,
    &erp_addon_stock_account::ADDON,
    &erp_addon_stock_mail::ADDON,
    &erp_addon_purchase_stock::ADDON,
    &erp_addon_sales_team::ADDON,
    &erp_addon_sale::ADDON,
    &erp_addon_ai_base::ADDON,
];

/// Expected SQL registrations of native addons; independent of the linked declarations.
pub static SQL_MANIFESTS: &[&erp_registry::SqlObjectManifest] = &[
    &erp_addon_stock::schema_guards::MANIFEST,
    &erp_addon_stock_account::history_guards::MANIFEST,
    &erp_addon_account_reporting::schema::MANIFEST,
];

/// Expected lifecycle guards for native addons linked by this host.
pub static UNINSTALL_MANIFESTS: &[&erp_modules::sql_uninstall::SqlUninstallGuardManifest] =
    &[&erp_addon_stock_account::uninstall_guard::MANIFEST];
