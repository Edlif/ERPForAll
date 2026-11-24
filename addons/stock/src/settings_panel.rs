// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit boundaries for unfinished inventory features and addon setup.
use erp_sdk::prelude::*;
use erp_sdk::settings::{PanelItemDecl, PanelItemValue, ScopeKind, SettingsScope};

const GROUPS: &str = "Unavailable until a later release supplies feature-group changes.";
const TRACKING: &str =
    "Unavailable until a later release and a later release supply tracking controls.";
const ROUTES: &str = "Unavailable until a later release supplies route activation.";
const ADDONS: &str = "Unavailable until a later release supplies this addon.";
const FEATURES: &[(&str, &str, &str, &str)] = &[
    (
        "stock.group_stock_lot_print_gs1",
        "GS1 tracking labels",
        "Traceability",
        TRACKING,
    ),
    (
        "stock.group_lot_on_delivery_slip",
        "Tracking details on delivery documents",
        "Traceability",
        TRACKING,
    ),
    (
        "stock.group_stock_sign_delivery",
        "Delivery signatures",
        "Shipping",
        GROUPS,
    ),
    (
        "stock.replenish_on_order",
        "Replenish on order",
        "Warehouse",
        ROUTES,
    ),
    (
        "stock.module_stock_fleet",
        "Transport",
        "Operations",
        ADDONS,
    ),
    ("stock.module_stock_barcode", "Scanning", "Barcode", ADDONS),
    (
        "stock.module_stock_barcode_barcodelookup",
        "Barcode lookup",
        "Barcode",
        ADDONS,
    ),
    (
        "stock.module_quality_control",
        "Quality checks",
        "Operations",
        ADDONS,
    ),
    (
        "stock.module_quality_control_worksheet",
        "Quality worksheets",
        "Operations",
        ADDONS,
    ),
];
// Feature groups that already show or hide menus, view elements or fields: the row turns the group on or
// off for every internal user. `(key, label, block, group)`.
const GROUP_TOGGLES: &[(&str, &str, &str, &str)] = &[
    (
        "stock.group_stock_picking_batch",
        "Batch transfers",
        "Operations",
        "stock.group_stock_picking_batch",
    ),
    (
        "stock.group_stock_multi_locations",
        "Multiple locations",
        "Warehouse",
        "stock.group_stock_multi_locations",
    ),
    (
        "stock.group_stock_adv_location",
        "Advanced routes",
        "Warehouse",
        "stock.group_adv_location",
    ),
    (
        "stock.group_stock_production_lot",
        "Company-wide lot tracking",
        "Traceability",
        "stock.group_production_lot",
    ),
    (
        "stock.group_stock_tracking_lot",
        "Packages",
        "Operations",
        "stock.group_tracking_lot",
    ),
    (
        "stock.group_stock_tracking_owner",
        "Consignment",
        "Operations",
        "stock.group_tracking_owner",
    ),
    (
        "stock.group_warning_stock",
        "Partner stock warnings",
        "Operations",
        "stock.group_warning_stock",
    ),
];
// Each shortcut opens the ordinary menu of the records that own the policy.
const CONFIGURATION: &[(&str, &str, &str, &str, &str)] = &[
    (
        "stock.backorder_policies",
        "Backorder policies by operation type",
        "Operations",
        "stock.menu_stock_picking_type_config",
        "stock.group_stock_manager",
    ),
    (
        "stock.reordering_rules",
        "Reordering rules (minimum and maximum)",
        "Planning",
        "stock.menu_stock_orderpoint",
        "stock.group_stock_manager",
    ),
    (
        "stock.lot_serial_register",
        "Lots and serial numbers",
        "Traceability",
        "stock.menu_stock_lot",
        "stock.group_production_lot",
    ),
    (
        "stock.lot_user_access",
        "Manage user access to lots and serial numbers",
        "Traceability",
        "base.settings_users_menu",
        "base.group_system",
    ),
];
const MODULES: &[(&str, &str, &str, &str)] = &[
    (
        "stock.module_product_expiry",
        "Expiry dates",
        "Traceability",
        "product_expiry",
    ),
    (
        "stock.module_stock_sms",
        "Text delivery",
        "Shipping",
        "stock_sms",
    ),
    ("stock.module_delivery", "Carriers", "Shipping", "delivery"),
    (
        "stock.module_stock_dropshipping",
        "Direct supplier delivery",
        "Warehouse",
        "stock_dropshipping",
    ),
];
fn items() -> Result<Vec<PanelItemDecl>> {
    let mut items = Vec::new();
    for (key, label, block, menu, group) in CONFIGURATION {
        items.push(
            PanelItemDecl::menu_link(key, label, menu)?
                .block(block)?
                .sequence(10)
                .scope(ScopeKind::Company)
                .access(&[group])?,
        );
    }
    for (key, label, block, _) in FEATURES {
        items.push(
            PanelItemDecl::info(key, label)?
                .block(block)?
                .sequence(100)
                .scope(ScopeKind::Company)
                .access(&["stock.group_stock_manager", "base.group_system"])?,
        );
    }
    for (key, label, block, group) in GROUP_TOGGLES {
        items.push(
            PanelItemDecl::group_toggle(key, label, group)?
                .block(block)?
                .sequence(100)
                .access(&["base.group_system"])?,
        );
    }
    for (key, label, block, addon) in MODULES {
        items.push(
            PanelItemDecl::module_toggle(key, label, addon)?
                .block(block)?
                .sequence(200)
                .access(&["base.group_system"])?,
        );
    }
    Ok(items)
}
fn resolve<'a>(
    _env: &'a Env,
    _tx: &'a mut Transaction,
    _scope: &'a SettingsScope,
    key: &'a str,
) -> erp_sdk::BoxFuture<'a, Result<PanelItemValue>> {
    Box::pin(async move {
        let (_, label, _, detail) = FEATURES
            .iter()
            .find(|(candidate, _, _, _)| *candidate == key)
            .ok_or_else(|| {
                Error::programming(format!("Inventory has no information row {key}."))
            })?;
        PanelItemValue::info(Value::str(format!("{label} (unavailable)")))?.detail(detail)
    })
}
erp_sdk::settings_panel_items!(items, resolve);

pub(super) fn require_linked() -> Result<()> {
    let expected: std::collections::BTreeSet<_> = FEATURES
        .iter()
        .map(|(key, _, _, _)| *key)
        .chain(CONFIGURATION.iter().map(|(key, _, _, _, _)| *key))
        .chain(MODULES.iter().map(|(key, _, _, _)| *key))
        .chain(GROUP_TOGGLES.iter().map(|(key, _, _, _)| *key))
        .map(str::to_owned)
        .collect();
    let mut keys = std::collections::BTreeSet::new();
    for entry in erp_sdk::settings::PANEL_CONTRIBUTIONS
        .iter()
        .filter(|entry| entry.addon.name == "stock")
    {
        for item in (entry.declarations)()? {
            keys.insert(item.key().to_owned());
        }
    }
    if keys != expected {
        return Err(Error::programming(
            "Inventory settings panel registration is missing from erp-addon-stock.",
        ));
    }
    Ok(())
}
