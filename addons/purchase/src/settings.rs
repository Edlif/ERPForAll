// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Purchase settings that order confirmation reads: approval levels, the amount above which a
//! manager approves, and whether confirmed orders are locked. All three are per company, as the
//! reference keeps them on the company, and are
//! written by administrators on the Settings page (spec `access.md` §4.7 S1-S3).

use erp_addon_base::ir_config_setting::read_values;
use erp_sdk::prelude::*;
use erp_sdk::serde_json;
use erp_sdk::settings::{PanelItemDecl, PanelItemValue, ScopeKind, SettingDecl};
use erp_sdk::views::wire::{SettingsScope, WireId};

use crate::USER_GROUP;

/// Approval levels: `one_step` or `two_step` (S1).
pub const APPROVAL_SETTING: &str = "purchase.po_double_validation";
/// The order total from which a manager approves, in the company currency (S2).
pub const AMOUNT_SETTING: &str = "purchase.po_double_validation_amount";
/// `edit` keeps confirmed orders editable, `lock` locks them at approval (S3).
pub const LOCK_SETTING: &str = "purchase.po_lock";

/// Default of [`AMOUNT_SETTING`].
pub const AMOUNT_DEFAULT: f64 = 5_000.0;
/// Floor of [`AMOUNT_SETTING`]: 0 sends every order to a manager.
pub const AMOUNT_FLOOR: f64 = 0.0;
/// Hard bound of [`AMOUNT_SETTING`], in major units of the company currency.
pub const AMOUNT_HARD: f64 = 1_000_000_000_000.0;

const APPROVAL_CHOICES: [(&str, &str); 2] = [
    ("one_step", "Every buyer confirms orders directly"),
    (
        "two_step",
        "A manager approves orders from the minimum amount",
    ),
];
const LOCK_CHOICES: [(&str, &str); 2] = [
    ("edit", "Confirmed orders stay editable"),
    ("lock", "Confirmed orders are locked"),
];

// Open the installed master-data menus with their ordinary access checks.
fn setup_links() -> Result<Vec<PanelItemDecl>> {
    [
        (
            "Products",
            "purchase.attributes",
            "Product attributes",
            "product.menu_product_attribute",
            USER_GROUP,
            10,
        ),
        (
            "Products",
            "purchase.variants",
            "Product variants",
            "product.menu_product_product",
            USER_GROUP,
            20,
        ),
        (
            "Products",
            "purchase.units",
            "Units of measure",
            "uom.menu_uom_form_action",
            "base.group_system",
            30,
        ),
        (
            "Vendor bills",
            "purchase.bill_payment_terms",
            "Payment terms",
            "account.menu_action_payment_term",
            "account.group_account_readonly",
            10,
        ),
    ]
    .into_iter()
    .map(|(block, key, label, menu, group, order)| {
        PanelItemDecl::menu_link(key, label, menu)?
            .block(block)?
            .sequence(order)
            .scope(ScopeKind::Company)
            .access(&[group])
    })
    .collect()
}

const GROUP_TOGGLES: &[(&str, &str, &str, &str)] = &[(
    "purchase.group_warning_purchase",
    "Warnings",
    "Orders",
    crate::WARNING_GROUP,
)];

fn group_toggles() -> Result<Vec<PanelItemDecl>> {
    GROUP_TOGGLES
        .iter()
        .map(|(key, label, block, group)| {
            PanelItemDecl::group_toggle(key, label, group)?
                .block(block)?
                .sequence(100)
                .access(&["base.group_system"])
        })
        .collect()
}

const MODULES: &[(&str, &str, &str, &str)] = &[
    (
        "purchase.module_three_way_match",
        "Three-way bill matching",
        "Invoicing",
        "purchase_three_way_match",
    ),
    (
        "purchase.module_stock_dropshipping",
        "Direct supplier delivery",
        "Logistics",
        "stock_dropshipping",
    ),
];

fn module_toggles() -> Result<Vec<PanelItemDecl>> {
    MODULES
        .iter()
        .map(|(key, label, block, addon)| {
            PanelItemDecl::module_toggle(key, label, addon)?
                .block(block)?
                .sequence(200)
                .access(&["base.group_system"])
        })
        .collect()
}

fn panel_items() -> Result<Vec<PanelItemDecl>> {
    let mut items = setup_links()?;
    items.extend(group_toggles()?);
    items.extend(module_toggles()?);
    Ok(items)
}

// Static menu targets and module toggles are resolved by the settings service.
fn resolve_setup<'a>(
    _: &'a Env,
    _: &'a mut Transaction,
    _: &'a erp_sdk::settings::SettingsScope,
    key: &'a str,
) -> erp_sdk::BoxFuture<'a, Result<PanelItemValue>> {
    Box::pin(async move {
        Err(Error::programming(format!(
            "Purchase has no dynamic setup value for {key}."
        )))
    })
}
erp_sdk::settings_panel_items!(panel_items, resolve_setup);

fn settings() -> Result<Vec<SettingDecl>> {
    let place = |decl: SettingDecl, sequence: u32| -> Result<SettingDecl> {
        decl.block("Orders")?
            .sequence(sequence)
            .access(&[USER_GROUP], &["base.group_system"])
    };
    Ok(vec![
        place(
            SettingDecl::selection(
                APPROVAL_SETTING,
                "Purchase order approval",
                ScopeKind::Company,
                "one_step",
                &APPROVAL_CHOICES,
            )?,
            10,
        )?,
        place(
            SettingDecl::float(
                AMOUNT_SETTING,
                "Minimum amount for manager approval (company currency)",
                ScopeKind::Company,
                AMOUNT_DEFAULT,
                (AMOUNT_FLOOR, AMOUNT_HARD),
            )?,
            20,
        )?,
        place(
            SettingDecl::selection(
                LOCK_SETTING,
                "Lock confirmed orders",
                ScopeKind::Company,
                "edit",
                &LOCK_CHOICES,
            )?,
            30,
        )?,
    ])
}
erp_sdk::setting_declarations!(settings);

/// The order policy of one company.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderPolicy {
    /// A manager approves orders whose total is not under [`Self::amount`].
    pub two_step: bool,
    /// The minimum amount in the company currency (exact: compared with exact totals).
    pub amount: Decimal,
    /// Approval locks the order.
    pub lock: bool,
}

impl OrderPolicy {
    /// The policy of `company` (the defaults when never set). Read with the framework's rights: the
    /// confirming user needs no settings group.
    ///
    /// # Errors
    ///
    /// Settings read failures; a stored value that no longer fits its declaration.
    pub async fn read(tx: &mut Transaction, company: i64) -> Result<Self> {
        let env = tx.env().sudo(true).with_company(company)?;
        let id = u64::try_from(company)
            .ok()
            .and_then(|id| WireId::try_from(id).ok())
            .ok_or_else(|| Error::validation(format!("company id {company} is invalid")))?;
        let keys = [APPROVAL_SETTING, AMOUNT_SETTING, LOCK_SETTING];
        let read = read_values(
            &env,
            tx,
            SettingsScope::Company { company_id: id },
            keys.iter().map(|k| (*k).to_owned()).collect(),
        )
        .await?;
        let read = serde_json::to_value(&read)
            .map_err(|error| Error::internal(format!("purchase settings: {error}")))?;
        let value = |key: &str| {
            read["values"]
                .as_array()
                .and_then(|values| values.iter().find(|v| v["key"] == key))
                .map(|entry| entry["value"].clone())
                .ok_or_else(|| Error::internal(format!("{key} was not read")))
        };
        let approval = value(APPROVAL_SETTING)?;
        let lock = value(LOCK_SETTING)?;
        let bounds = || {
            Error::validation(format!(
                "{AMOUNT_SETTING} must be a number from {AMOUNT_FLOOR} to {AMOUNT_HARD}."
            ))
        };
        let amount = value(AMOUNT_SETTING)?
            .as_f64()
            .filter(|a| (AMOUNT_FLOOR..=AMOUNT_HARD).contains(a))
            // the shortest text of the stored number is the number the administrator typed
            .and_then(|a| a.to_string().parse::<Decimal>().ok())
            .ok_or_else(bounds)?;
        Ok(Self {
            two_step: match approval.as_str() {
                Some("one_step") => false,
                Some("two_step") => true,
                _ => return Err(Error::validation(format!("{APPROVAL_SETTING} is invalid."))),
            },
            amount,
            lock: match lock.as_str() {
                Some("edit") => false,
                Some("lock") => true,
                _ => return Err(Error::validation(format!("{LOCK_SETTING} is invalid."))),
            },
        })
    }

    /// Whether an order of this company with `total` (already in the company currency) is
    /// approved by the confirming user: one step, a total under the amount, or a manager.
    #[must_use]
    pub fn approves(&self, total: Decimal, is_manager: bool) -> bool {
        !self.two_step || total < self.amount || is_manager
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use erp_sdk::settings::SettingKind;

    #[test]
    fn order_settings_are_bounded_company_settings() {
        let declarations = settings().unwrap();
        let keys: Vec<&str> = declarations.iter().map(|d| d.key()).collect();
        assert_eq!(keys, [APPROVAL_SETTING, AMOUNT_SETTING, LOCK_SETTING]);
        for decl in &declarations {
            assert_eq!(decl.write_groups(), &["base.group_system"]);
        }
        assert_eq!(declarations[0].default_value(), &Value::str("one_step"));
        let SettingKind::Float { min, max } = declarations[1].kind() else {
            panic!("the amount is a number");
        };
        assert_eq!((*min, *max), (AMOUNT_FLOOR, AMOUNT_HARD));
        assert_eq!(declarations[2].default_value(), &Value::str("edit"));
    }

    #[test]
    fn setup_links_use_existing_menus_and_groups() {
        use erp_sdk::settings::PanelItemKind;
        let items = setup_links().unwrap();
        let targets: Vec<_> = items
            .iter()
            .map(|item| {
                assert_eq!(item.scope_restriction(), Some(ScopeKind::Company));
                let PanelItemKind::Link(target) = item.kind() else {
                    panic!("expected a setup link");
                };
                (
                    item.block_title(),
                    target.xmlid(),
                    item.read_groups()[0].as_str(),
                )
            })
            .collect();
        assert_eq!(
            targets,
            [
                ("Products", "product.menu_product_attribute", USER_GROUP),
                ("Products", "product.menu_product_product", USER_GROUP),
                ("Products", "uom.menu_uom_form_action", "base.group_system"),
                (
                    "Vendor bills",
                    "account.menu_action_payment_term",
                    "account.group_account_readonly"
                ),
            ]
        );
    }

    #[test]
    fn module_toggles_declare_existing_addons_for_administrators() {
        use erp_sdk::settings::PanelItemKind;
        let items = panel_items().unwrap();
        assert_eq!(items.len(), 7);
        let mut keys = std::collections::BTreeSet::new();
        for item in &items {
            assert!(keys.insert(item.key()), "duplicate key {}", item.key());
        }
        let group_items: Vec<_> = items
            .iter()
            .filter_map(|item| match item.kind() {
                PanelItemKind::Group { group, .. } => Some((
                    item.key(),
                    item.block_title(),
                    item.label(),
                    group.as_str(),
                    item.read_groups().to_vec(),
                    item.scope_restriction(),
                    item.display_sequence(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            group_items,
            [(
                "purchase.group_warning_purchase",
                "Orders",
                "Warnings",
                crate::WARNING_GROUP,
                vec!["base.group_system".to_string()],
                None,
                100,
            )]
        );
        let toggles: Vec<_> = items
            .iter()
            .filter_map(|item| match item.kind() {
                PanelItemKind::Module { addon, .. } => Some((
                    item.key(),
                    item.block_title(),
                    item.label(),
                    addon.as_str(),
                    item.read_groups().to_vec(),
                    item.scope_restriction(),
                    item.display_sequence(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            toggles,
            [
                (
                    "purchase.module_three_way_match",
                    "Invoicing",
                    "Three-way bill matching",
                    "purchase_three_way_match",
                    vec!["base.group_system".to_string()],
                    None,
                    200,
                ),
                (
                    "purchase.module_stock_dropshipping",
                    "Logistics",
                    "Direct supplier delivery",
                    "stock_dropshipping",
                    vec!["base.group_system".to_string()],
                    None,
                    200,
                ),
            ]
        );
        let addons_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        for (_, _, _, addon, _, _, _) in toggles {
            assert!(
                addons_dir.join(addon).join("manifest.toml").is_file(),
                "manifest for {addon} must exist"
            );
        }
    }

    #[test]
    fn approval_rule() {
        let d = |text: &str| text.parse::<Decimal>().unwrap();
        let one = OrderPolicy {
            two_step: false,
            amount: d("0"),
            lock: false,
        };
        assert!(one.approves(d("1000000000"), false));
        let two = OrderPolicy {
            two_step: true,
            amount: d("0"),
            lock: false,
        };
        assert!(!two.approves(d("0"), false));
        assert!(two.approves(d("0"), true));
        let two = OrderPolicy {
            amount: d("5000"),
            ..two
        };
        assert!(two.approves(d("4999.99"), false));
        // a total one cent under the limit is under it, which no float sum can promise
        assert!(two.approves(d("4999.999999999999999"), false));
        assert!(!two.approves(d("5000"), false));
    }
}
