// SPDX-License-Identifier: MIT OR Apache-2.0
//! Administrator links to provider metadata and company routing in General Settings.
use erp_sdk::prelude::*;
use erp_sdk::settings::{PanelItemDecl, PanelItemValue, ScopeKind, SettingsScope};

fn items() -> Result<Vec<PanelItemDecl>> {
    [
        (
            "ai_base.providers",
            "Providers",
            "ai_base.provider_menu",
            10,
        ),
        ("ai_base.routes", "Company routes", "ai_base.route_menu", 20),
    ]
    .into_iter()
    .map(|(key, label, menu, order)| {
        PanelItemDecl::menu_link(key, label, menu)?
            .app("base")?
            .block("AI")?
            .sequence(order)
            .scope(ScopeKind::Global)
            .access(&["base.group_system"])
    })
    .collect()
}

// The Settings service resolves static menu links with the caller's access rules.
fn resolve<'a>(
    _: &'a Env,
    _: &'a mut Transaction,
    _: &'a SettingsScope,
    key: &'a str,
) -> erp_sdk::BoxFuture<'a, Result<PanelItemValue>> {
    Box::pin(async move {
        Err(Error::programming(format!(
            "AI settings have no dynamic value for {key}."
        )))
    })
}
erp_sdk::settings_panel_items!(items, resolve);
