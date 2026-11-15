// SPDX-License-Identifier: MIT OR Apache-2.0
use erp_report::{ProfileKind, ReportError, ReportLimits, inspect, validate};
use erp_views::{LimitKind, ViewLimits};

#[test]
fn unknown_directive_names_template_line_and_token() {
    let error = validate(
        "sample.report",
        "<t>\n  <div t-missing=\"x\"/>\n</t>",
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::Directive));
    assert_eq!(
        error.to_string(),
        "sample.report:2: directive `t-missing` is not in print profile 1; see the addon guide page on reports"
    );
}

#[test]
fn vocabulary_groups_fail_with_precise_diagnostics() {
    for (xml, kind, token, label) in [
        ("<video/>", ProfileKind::Element, "video", "element"),
        (
            "<p class='invented'/>",
            ProfileKind::Class,
            "invented",
            "class",
        ),
        (
            "<p invented='x'/>",
            ProfileKind::Attribute,
            "invented",
            "attribute",
        ),
        (
            "<p style='transform:rotate(2deg)'/>",
            ProfileKind::Style,
            "transform",
            "style",
        ),
        (
            "<p style='width:NaNpx'/>",
            ProfileKind::StyleValue,
            "width:NaNpx",
            "style value",
        ),
        (
            "<i data-icon='invented'/>",
            ProfileKind::Icon,
            "invented",
            "icon",
        ),
    ] {
        let error = validate("sample.report", xml, ReportLimits::default()).unwrap_err();
        assert!(matches!(&error, ReportError::Profile(d) if d.kind() == kind));
        assert_eq!(
            error.to_string(),
            format!(
                "sample.report:1: {label} `{token}` is not in print profile 1; see the addon guide page on reports"
            )
        );
    }
}

#[test]
fn seed_profile_accepts_print_vocabulary_and_explicit_noops() {
    let xml = r#"<t t-name="sample.report"><section class="page"><h1 class="text-center mb-2">Title</h1><div class="row"><span class="col-6 fw-bold" style="margin:0 2mm; color:#123456; font-size:12pt; border:1px solid black" t-esc="doc.name"/></div><table class="table table-borderless"><thead><tr><th>Item</th></tr></thead><tbody><tr><td colspan="2" class="d-md-table-cell">Text</td></tr></tbody></table><a href="mailto:hello@example.test">Mail</a><i class="oi oi-fw" data-icon="mail" role="img" aria-label="Mail"/></section></t>"#;
    let result = validate("sample.report", xml, ReportLimits::default()).unwrap();
    assert!(result.obligations().is_empty());
}

#[test]
fn dynamic_values_retain_obligations_but_unsafe_targets_fail() {
    let result = validate(
        "sample.report",
        "<p t-att-class='classes' t-attf-style='width: {{ width }}px' t-att='attrs'/>",
        ReportLimits::default(),
    )
    .unwrap();
    assert_eq!(
        result
            .obligations()
            .iter()
            .map(|o| o.attribute())
            .collect::<Vec<_>>(),
        ["class", "style", "*"]
    );
    for xml in ["<p t-att-onclick='x'/>", "<p onclick='x'/>"] {
        let error = validate("sample.report", xml, ReportLimits::default()).unwrap_err();
        assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::Attribute));
        assert_eq!(
            error.to_string(),
            "sample.report:1: attribute `onclick` is not in print profile 1; see the addon guide page on reports"
        );
    }
}

#[test]
fn links_reject_decoded_and_mixed_case_active_schemes() {
    for href in [
        "JavaScript:alert(1)",
        "java&#x9;script:alert(1)",
        "data:text/html,bad",
    ] {
        let error = validate(
            "sample.report",
            &format!("<a href='{href}'/>"),
            ReportLimits::default(),
        )
        .unwrap_err();
        assert!(
            matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::AttributeValue)
        );
        let token = href.replace("&#x9;", "\t");
        assert_eq!(
            error.to_string(),
            format!(
                "sample.report:1: attribute value `href:{token}` is not in print profile 1; see the addon guide page on reports"
            )
        );
    }
}

#[test]
fn inspection_collects_missing_tokens_and_preserves_crlf_lines() {
    let result = inspect(
        "sample.report",
        "<t>\r\n<p class='one two' bogus='3'/>\r\n</t>",
        ReportLimits::default(),
    )
    .unwrap();
    assert_eq!(
        result
            .diagnostics()
            .iter()
            .map(|d| (d.line(), d.token()))
            .collect::<Vec<_>>(),
        [(2, "one"), (2, "two"), (2, "bogus")]
    );
}

#[test]
fn limits_and_invalid_xml_return_typed_errors() {
    let limits = ReportLimits::default()
        .with_views(ViewLimits::default().lowered(LimitKind::Depth, 2).unwrap());
    let error = validate("sample.report", "<t><p><b/></p></t>", limits).unwrap_err();
    assert!(matches!(&error, ReportError::View { .. }));
    assert!(std::error::Error::source(&error).is_some());
    // Parser wording belongs to erp-views; wrapper must retain that exact source message.
    assert_eq!(
        error.to_string(),
        format!(
            "sample.report: {}",
            std::error::Error::source(&error).unwrap()
        )
    );
    for xml in ["", "<!DOCTYPE t><t/>", "<t xmlns='urn:test'/>", "<t>"] {
        let error = validate("sample.report", xml, ReportLimits::default()).unwrap_err();
        assert!(matches!(&error, ReportError::View { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "sample.report: {}",
                std::error::Error::source(&error).unwrap()
            )
        );
    }
}

#[test]
fn malformed_dynamic_expression_is_not_admitted() {
    let error = validate(
        "sample.report",
        "<p t-att-class='('/>",
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Expression { .. }));
    assert_eq!(
        error.to_string(),
        format!(
            "sample.report:1: expression `(`: {}",
            std::error::Error::source(&error).unwrap()
        )
    );
}

#[test]
fn evaluated_attributes_use_the_same_closed_value_grammar() {
    erp_report::validate_attribute("p", "class", "fw-bold", ReportLimits::default()).unwrap();
    let error =
        erp_report::validate_attribute("a", "href", "javascript:bad", ReportLimits::default())
            .unwrap_err();
    assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::AttributeValue));
    assert_eq!(
        error.to_string(),
        "evaluated attribute:1: attribute value `href:javascript:bad` is not in print profile 1; see the addon guide page on reports"
    );
}

#[test]
fn combined_arch_honors_lowered_limits_from_the_validator_caller() {
    let arch = erp_views::ViewCatalogue::with_limits(
        vec![erp_views::ViewDescriptor::new(
            1,
            "test",
            "",
            "<t><p><b/></p></t>",
        )],
        ViewLimits::default(),
    )
    .unwrap()
    .combine(1)
    .unwrap();
    let limits = ReportLimits::default()
        .with_views(ViewLimits::default().lowered(LimitKind::Depth, 2).unwrap());
    let error = erp_report::validate_arch("sample.report", arch.arch(), limits).unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Limit {
            name: "arch_depth",
            maximum: 2
        }
    ));
    assert_eq!(
        error.to_string(),
        "report limit arch_depth exceeded or invalid (maximum 2)"
    );
}

#[test]
fn style_longhands_refuse_multiple_lengths() {
    for style in [
        "margin-top:1px 2px",
        "padding-left:1px 2px",
        "font-family:'Arial",
        "font-family:'Arial\"",
    ] {
        let encoded = style.replace('\'', "&apos;").replace('"', "&quot;");
        let error = validate(
            "sample.report",
            &format!("<p style='{encoded}'/>"),
            ReportLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::StyleValue));
        assert_eq!(
            error.to_string(),
            format!(
                "sample.report:1: style value `{style}` is not in print profile 1; see the addon guide page on reports"
            )
        );
        let error = erp_report::validate_attribute("p", "style", style, ReportLimits::default())
            .unwrap_err();
        assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::StyleValue));
        assert_eq!(
            error.to_string(),
            format!(
                "evaluated attribute:1: style value `{style}` is not in print profile 1; see the addon guide page on reports"
            )
        );
    }
    for style in [
        "margin:1px 2px 3px 4px",
        "padding:1px 2px 3px 4px",
        "font-family:'Arial', serif",
        "font-family:\"Noto Sans\"",
    ] {
        erp_report::validate_attribute("p", "style", style, ReportLimits::default()).unwrap();
    }
}

#[test]
fn admission_limits_refuse_zero_overflow_and_boundary_plus_one() {
    for value in [0, 257, usize::MAX] {
        let error = ReportLimits::default().with_diagnostics(value).unwrap_err();
        assert!(matches!(
            &error,
            ReportError::Limit {
                name: "diagnostics",
                maximum: 256
            }
        ));
        assert_eq!(
            error.to_string(),
            "report limit diagnostics exceeded or invalid (maximum 256)"
        );
    }
    for value in [0, 33, usize::MAX] {
        let error = ReportLimits::default().with_call_depth(value).unwrap_err();
        assert!(matches!(
            &error,
            ReportError::Limit {
                name: "call_depth",
                maximum: 32
            }
        ));
        assert_eq!(
            error.to_string(),
            "report limit call_depth exceeded or invalid (maximum 32)"
        );
    }
    let limits = ReportLimits::default().with_diagnostics(1).unwrap();
    assert_eq!(
        inspect("demo", "<p class='unknown'/>", limits)
            .unwrap()
            .diagnostics()
            .len(),
        1
    );
    let error = inspect("demo", "<p class='unknown other'/>", limits).unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Limit {
            name: "diagnostics",
            maximum: 1
        }
    ));
    assert_eq!(
        error.to_string(),
        "report limit diagnostics exceeded or invalid (maximum 1)"
    );
    let arch = erp_views::ViewCatalogue::with_limits(
        vec![erp_views::ViewDescriptor::new(
            1,
            "test",
            "",
            "<t title='hello' class='page'><p>日本語</p></t>",
        )],
        ViewLimits::default(),
    )
    .unwrap()
    .combine(1)
    .unwrap();
    for (kind, name) in [
        (LimitKind::CombinedNodes, "arch_nodes"),
        (LimitKind::CombinedBytes, "arch_bytes"),
        (LimitKind::ElementAttributes, "element_attributes"),
        (LimitKind::EvaluationSteps, "validation_work"),
    ] {
        let limits =
            ReportLimits::default().with_views(ViewLimits::default().lowered(kind, 1).unwrap());
        let error = erp_report::validate_arch("sample.report", arch.arch(), limits).unwrap_err();
        assert!(
            matches!(&error, ReportError::Limit { name: actual, maximum: 1 } if *actual == name)
        );
        assert_eq!(
            error.to_string(),
            format!("report limit {name} exceeded or invalid (maximum 1)")
        );
    }
}

#[test]
fn profile_version_has_no_unchecked_construction() {
    assert_eq!(erp_report::PrintProfile::new(1).unwrap().version(), 1);
    for version in [0, 2, u32::MAX] {
        let error = erp_report::PrintProfile::new(version).unwrap_err();
        assert!(matches!(&error, ReportError::Version(v) if *v == version));
        assert_eq!(
            error.to_string(),
            format!("print profile {version} is unsupported; supported version is 1")
        );
    }
}

#[test]
fn class_families_are_bounded_not_prefix_wildcards() {
    validate(
        "sample.report",
        "<p class='col-1 col-12 m-0 mt-5 mx-auto gap-3 p-2'/>",
        ReportLimits::default(),
    )
    .unwrap();
    for token in [
        "col-0",
        "col-13",
        "col-01",
        "m-6",
        "p-auto",
        "gap-auto",
        "mt--1",
        "o_invented",
    ] {
        let error = validate(
            "sample.report",
            &format!("<p class='{token}'/>"),
            ReportLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::Class));
        assert_eq!(
            error.to_string(),
            format!(
                "sample.report:1: class `{token}` is not in print profile 1; see the addon guide page on reports"
            )
        );
    }
}

#[test]
fn literal_directive_and_asset_values_are_checked() {
    validate(
        "sample.report",
        "<t t-translation='off'><img src='/web/static/logo.png' alt='Logo'/><img src='data:image/png;base64,AAAA'/><a href='/my/orders'>Orders</a></t>",
        ReportLimits::default(),
    )
    .unwrap();
    for (xml, token) in [
        ("<t t-foreach='docs' t-as='1x'/>", "t-as:1x"),
        ("<t t-set='' t-value='1'/>", "t-set:"),
        ("<t t-translation='on'/>", "t-translation:on"),
        (
            "<img src='https://example.test/a.png'/>",
            "src:https://example.test/a.png",
        ),
        (
            "<img src='//example.test/a.png'/>",
            "src://example.test/a.png",
        ),
        (
            "<img src='javascript:alert(1)'/>",
            "src:javascript:alert(1)",
        ),
        ("<img src='data:text/html,x'/>", "src:data:text/html,x"),
        ("<a href='/\\example.test'/>", "href:/\\example.test"),
        ("<a href='//example.test'/>", "href://example.test"),
        (
            "<p t-attf-style='width: {{ width'/>",
            "t-attf-style:width: {{ width",
        ),
        ("<t t-call='sample.#{variant'/>", "t-call:sample.#{variant"),
    ] {
        let error = validate("sample.report", xml, ReportLimits::default()).unwrap_err();
        assert!(
            matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::AttributeValue),
            "{xml}: {error}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "sample.report:1: attribute value `{token}` is not in print profile 1; see the addon guide page on reports"
            )
        );
    }
    let error = erp_report::validate_attribute(
        "img",
        "src",
        "http://example.test/a.png",
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::AttributeValue));
    assert_eq!(
        error.to_string(),
        "evaluated attribute:1: attribute value `src:http://example.test/a.png` is not in print profile 1; see the addon guide page on reports"
    );
}

#[test]
fn dynamic_tags_and_template_names_are_obligations() {
    let result = validate(
        "sample.report",
        "<t><div t-tag='kind'/><t t-call='sample.{{ variant }}'/><t t-call='sample.static'/></t>",
        ReportLimits::default(),
    )
    .unwrap();
    assert_eq!(
        result
            .obligations()
            .iter()
            .map(|o| (o.attribute(), o.expression()))
            .collect::<Vec<_>>(),
        [("tag", "kind"), ("template", "sample.{{ variant }}")]
    );
}

#[test]
fn evaluated_names_and_format_parts_are_scanned_strictly() {
    for name in [
        "aria-x onclick=alert(1)",
        "data-oe-Model",
        "aria-",
        "data-oe-a=b",
    ] {
        let error =
            erp_report::validate_attribute("p", name, "v", ReportLimits::default()).unwrap_err();
        assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::Attribute));
        assert_eq!(
            error.to_string(),
            format!(
                "evaluated attribute:1: attribute `{name}` is not in print profile 1; see the addon guide page on reports"
            )
        );
    }
    erp_report::validate_attribute("p", "aria-label", "v", ReportLimits::default()).unwrap();
    erp_report::validate_attribute("p", "data-oe-source_id", "v", ReportLimits::default()).unwrap();
    // Both part forms, in any order and number, are parsed; a bad part names its expression.
    let parts = "x #{a} y {{ b }} ".repeat(2000);
    let result = validate(
        "sample.report",
        &format!("<p t-attf-title='{parts}'/>"),
        ReportLimits::default(),
    )
    .unwrap();
    assert_eq!(result.obligations().len(), 1);
    let error = validate(
        "sample.report",
        "<p t-attf-title='#{a} {{ ( }}'/>",
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Expression { .. }));
    assert_eq!(
        error.to_string(),
        format!(
            "sample.report:1: expression `(`: {}",
            std::error::Error::source(&error).unwrap()
        )
    );
}

#[test]
fn headings_are_levels_one_to_six() {
    validate(
        "sample.report",
        "<t><h1>a</h1><h2>b</h2><h3>c</h3><h4>d</h4><h5>e</h5><h6>f</h6></t>",
        ReportLimits::default(),
    )
    .unwrap();
    for tag in ["h0", "h7", "h10", "h", "hr1"] {
        let error = validate(
            "sample.report",
            &format!("<{tag}/>"),
            ReportLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(&error, ReportError::Profile(d) if d.kind() == ProfileKind::Element));
        assert_eq!(
            error.to_string(),
            format!(
                "sample.report:1: element `{tag}` is not in print profile 1; see the addon guide page on reports"
            )
        );
    }
}
