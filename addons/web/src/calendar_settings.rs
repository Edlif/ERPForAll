// SPDX-License-Identifier: MIT OR Apache-2.0
//! The calendar view's bound, set in Settings (database-wide): the most events one visible range may
//! load. The web client reads it when it opens a calendar, asks for one event more than the bound and
//! refuses the range by name when that one comes back, instead of drawing a silently cut grid.

/// Setting key of the most events one calendar range may load.
pub(crate) const MAX_EVENTS: &str = "web.calendar_max_events";

fn declarations() -> erp_sdk::Result<Vec<erp_sdk::settings::SettingDecl>> {
    use erp_sdk::settings::{ScopeKind, SettingDecl};
    Ok(vec![
        SettingDecl::integer(
            MAX_EVENTS,
            "Calendar view: most events one range may show",
            ScopeKind::Global,
            1000,
            (50, 20_000),
        )?
        .access(&["base.group_user"], &["base.group_system"])?,
    ])
}
erp_sdk::setting_declarations!(declarations);

#[cfg(test)]
mod tests {
    #[test]
    fn calendar_event_bound_is_declared_with_default_floor_and_hard_bound() {
        use erp_sdk::settings::SettingKind;
        let declarations = super::declarations().unwrap();
        assert_eq!(declarations.len(), 1);
        let setting = &declarations[0];
        assert_eq!(setting.key(), super::MAX_EVENTS);
        assert_eq!(setting.read_groups(), &["base.group_user"]);
        assert_eq!(setting.write_groups(), &["base.group_system"]);
        assert_eq!(setting.default_value(), &erp_sdk::Value::Int(1000));
        let SettingKind::Integer { min, max } = setting.kind() else {
            panic!("integer required")
        };
        assert_eq!((*min, *max), (50, 20_000));
    }
}
