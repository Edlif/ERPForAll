// SPDX-License-Identifier: MIT OR Apache-2.0
// Calendar arch lowering; included into the `h18_lower` test module.

fn calendar_refusal(xml: &str) -> String {
    match fixture(xml, "calendar").unwrap_err() {
        ViewError::InvalidDescription(message) => message,
        other => panic!("Unexpected error kind: {other:?}"),
    }
}

#[test]
fn calendar_arch_defaults_and_fields() {
    let value = fixture(
        r#"<calendar string="Fixtures" date_start="deadline"><field name="name"/><field name="partner_id" filters="1" string="Who"/><field name="state" invisible="1"/></calendar>"#,
        "calendar",
    )
    .unwrap();
    let root = &value["nodes"][0];
    assert_eq!(root["kind"], "calendar");
    assert_eq!(root["date_start"]["name"], "deadline");
    assert_eq!(root["date_start"]["model"], "h18.fixture");
    for absent in ["date_stop", "all_day", "color"] {
        assert!(root.get(absent).is_none(), "{absent}");
    }
    assert_eq!(root["mode"], "week");
    assert_eq!(
        root["scales"],
        serde_json::json!(["day", "week", "month", "year"])
    );
    for (flag, expected) in [
        ("create", true),
        ("edit", true),
        ("delete", true),
        ("quick_create", true),
        ("month_overflow", true),
        ("hide_date", false),
        ("hide_time", false),
    ] {
        assert_eq!(root[flag], expected, "{flag}");
    }
    assert_eq!(root["event_limit"], 5);
    assert_eq!(root["children"], serde_json::json!([1, 2, 3]));
    let fields: Vec<_> = value["nodes"].as_array().unwrap()[1..]
        .iter()
        .map(|node| {
            (
                node["kind"].as_str().unwrap(),
                node["field"]["name"].as_str().unwrap(),
                node["label"].as_str().unwrap(),
                node["invisible"].as_bool().unwrap(),
                node["filters"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("calendar_field", "name", "Name", false, false),
            ("calendar_field", "partner_id", "Who", false, true),
            ("calendar_field", "state", "State", true, false),
        ]
    );
    let missing = calendar_refusal(r#"<calendar><field name="name"/></calendar>"#);
    assert!(missing.contains("needs date_start"), "{missing}");
}

#[test]
fn calendar_arch_declared_attributes() {
    let value = fixture(
        r#"<calendar date_start="seen_at" date_stop="seen_at" all_day="active" color="partner_id" scales="month,day" mode="day" create="0" edit="False" delete="false" quick_create="0" hide_date="True" hide_time="true" month_overflow="0" event_limit="3"/>"#,
        "calendar",
    )
    .unwrap();
    let root = &value["nodes"][0];
    assert_eq!(root["date_stop"]["name"], "seen_at");
    assert_eq!(root["all_day"]["name"], "active");
    assert_eq!(root["color"]["name"], "partner_id");
    assert_eq!(root["scales"], serde_json::json!(["month", "day"]));
    assert_eq!(root["mode"], "day");
    for (flag, expected) in [
        ("create", false),
        ("edit", false),
        ("delete", false),
        ("quick_create", false),
        ("month_overflow", false),
        ("hide_date", true),
        ("hide_time", true),
    ] {
        assert_eq!(root[flag], expected, "{flag}");
    }
    assert_eq!(root["event_limit"], 3);
    assert_eq!(root["children"], serde_json::json!([]));
    // Without week among the scales and no mode, the first scale is shown first.
    let first = fixture(
        r#"<calendar date_start="deadline" scales="year,month"/>"#,
        "calendar",
    )
    .unwrap();
    assert_eq!(first["nodes"][0]["mode"], "year");
    let integer_colour = fixture(
        r#"<calendar date_start="deadline" color="count"/>"#,
        "calendar",
    )
    .unwrap();
    assert_eq!(integer_colour["nodes"][0]["color"]["name"], "count");
}

#[test]
fn calendar_refuses_each_unsupported_shape_by_name() {
    for (xml, expected) in [
        (
            r#"<calendar date_start="name"/>"#,
            "date_start 'h18.fixture.name' must be a stored date or datetime field",
        ),
        (
            r#"<calendar date_start="nope"/>"#,
            "date_start names unknown field 'h18.fixture.nope'",
        ),
        (
            r#"<calendar date_start="deadline" date_stop="seen_at"/>"#,
            "must have the type of date_start (date)",
        ),
        (
            r#"<calendar date_start="deadline" all_day="count"/>"#,
            "all_day 'h18.fixture.count' must be a boolean field",
        ),
        (
            r#"<calendar date_start="deadline" color="state"/>"#,
            "color 'h18.fixture.state' must be a many2one or integer field",
        ),
        (
            r#"<calendar date_start="deadline" scales="week,fortnight"/>"#,
            "scale \"fortnight\" must be one of day, week, month or year",
        ),
        (
            r#"<calendar date_start="deadline" scales="week,week"/>"#,
            "scale \"week\" is listed twice",
        ),
        (
            r#"<calendar date_start="deadline" scales="day,month" mode="week"/>"#,
            "mode \"week\" is not one of its scales",
        ),
        (
            r#"<calendar date_start="deadline" event_limit="0"/>"#,
            "event_limit must be an integer from 1 through 50",
        ),
        (
            r#"<calendar date_start="deadline" event_limit="51"/>"#,
            "event_limit must be an integer from 1 through 50",
        ),
        (
            r#"<calendar date_start="deadline" create="[('state', '=', 'draft')]"/>"#,
            "'create'",
        ),
        (
            r#"<calendar date_start="deadline" date_delay="count"/>"#,
            "'date_delay'",
        ),
        (
            r#"<calendar date_start="deadline"><group/></calendar>"#,
            "<group> is not allowed under <calendar>",
        ),
        (
            r#"<calendar date_start="deadline"><field name="name"/><field name="name"/></calendar>"#,
            "'h18.fixture.name' is declared twice",
        ),
        (
            r#"<calendar date_start="deadline"><field name="name" invisible="state == 'draft'"/></calendar>"#,
            "'invisible'",
        ),
        (
            r#"<calendar date_start="deadline"><field name="name" bogus="1"/></calendar>"#,
            "'bogus'",
        ),
    ] {
        let message = calendar_refusal(xml);
        assert!(message.contains(expected), "{xml}: {message}");
    }
}

#[test]
fn calendar_refuses_what_comes_later_with_its_task() {
    let mut cases: Vec<String> = [
        "aggregate=\"count\"",
        "event_open_popup=\"1\"",
        "form_view_id=\"3\"",
        "create_name_field=\"name\"",
        "quick_create_view_id=\"3\"",
        "multi_create_view=\"x\"",
        "show_unusual_days=\"1\"",
        "show_date_picker=\"0\"",
        "schedule=\"1\"",
        "js_class=\"x\"",
    ]
    .iter()
    .map(|attribute| format!(r#"<calendar date_start="deadline" {attribute}/>"#))
    .collect();
    for attribute in [
        "widget=\"x\"",
        "options=\"{}\"",
        "color=\"count\"",
        "avatar_field=\"name\"",
        "write_model=\"h18.fixture\"",
        "write_field=\"name\"",
        "filter_field=\"active\"",
    ] {
        cases.push(format!(
            r#"<calendar date_start="deadline"><field name="partner_id" filters="1" {attribute}/></calendar>"#
        ));
    }
    cases.push(r#"<calendar date_start="deadline"><popover/></calendar>"#.to_owned());
    cases.push(
        r#"<calendar date_start="deadline"><field name="count" filters="1"/></calendar>"#
            .to_owned(),
    );
    for xml in cases {
        let message = calendar_refusal(&xml);
        assert!(
            message.contains("comes with a later release"),
            "{xml}: {message}"
        );
    }
    // A filter on a selection field is carried.
    let selection = fixture(
        r#"<calendar date_start="deadline"><field name="state" filters="1"/></calendar>"#,
        "calendar",
    )
    .unwrap();
    assert_eq!(selection["nodes"][1]["filters"], true);
}
