// SPDX-License-Identifier: MIT OR Apache-2.0
use erp_report::{
    AttachmentPolicy, FieldInfo, ReportError, ReportLimits, TemplateSource, check_attachment,
};

fn fields(model: &str, field: &str) -> Option<FieldInfo> {
    match (model, field) {
        ("sale.order", "name") => Some(FieldInfo::scalar(false)),
        ("sale.order", "secret") => Some(FieldInfo::scalar(true)),
        ("sale.order", "lines") => Some(FieldInfo::relation("sale.line", false)),
        ("sale.line", "cost") => Some(FieldInfo::scalar(true)),
        _ => None,
    }
}
fn policy(attachment: bool, acknowledged: bool) -> AttachmentPolicy<'static> {
    AttachmentPolicy::new("sample.action", "sale.order")
        .with_attachment(attachment)
        .with_acknowledgement(acknowledged)
}
/// `docs` is the only name the library binds; the fixtures loop over it as report templates do.
fn each_doc(xml: &str) -> String {
    format!("<t t-foreach='docs' t-as='doc'>{xml}</t>")
}
fn check(
    xml: &str,
    attachment: bool,
    acknowledged: bool,
) -> Result<Vec<erp_report::RestrictedField>, ReportError> {
    check_attachment(
        &policy(attachment, acknowledged),
        &[TemplateSource::new("sample.report", &each_doc(xml))],
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
}

#[test]
fn attachment_check_is_pure_and_explicit_acknowledgement_keeps_findings() {
    let xml = "<p t-field='doc.secret'/>";
    assert!(check(xml, false, false).unwrap().is_empty());
    let error = check(xml, true, false).unwrap_err();
    assert!(matches!(&error, ReportError::Restricted { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 prints restricted field sale.order.secret; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
    );
    let findings = check(xml, true, true).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].field(), "sale.order.secret");
    assert!(
        check("<p t-field='doc.name'/>", true, false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn aliases_relations_and_called_templates_cannot_hide_restricted_fields() {
    let xml = "<t><t t-set='order' t-value='doc'/><t t-foreach='order.lines' t-as='line'><b t-out='line.cost'/></t></t>";
    assert_eq!(check(xml, true, true).unwrap()[0].field(), "sale.line.cost");
    let root = each_doc("<t t-call='sample.body'/>");
    let sources = [
        TemplateSource::new("sample.report", &root),
        TemplateSource::new("sample.body", "<b t-out='doc.secret'/>"),
    ];
    let error = check_attachment(
        &policy(true, false),
        &sources,
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Restricted { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.body:1 prints restricted field sale.order.secret; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
    );
}

#[test]
fn missing_metadata_and_helper_calls_are_not_security_passes() {
    for expression in ["doc.missing", "doc.compute_secret()", "unknown.secret"] {
        let error = check(&format!("<p t-out='{expression}'/>"), true, true).unwrap_err();
        assert!(matches!(&error, ReportError::Unresolved { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "report sample.action: sample.report:1 cannot resolve `{expression}` for attachment field validation"
            )
        );
    }
}

#[test]
fn call_cycles_fail_instead_of_skipping_repeated_templates() {
    let error = check("<t t-call='sample.report'/>", true, true).unwrap_err();
    assert!(matches!(&error, ReportError::Unresolved { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 cannot resolve `cyclic t-call sample.report` for attachment field validation"
    );
}

#[test]
fn declared_scalar_helpers_analyze_dependencies_and_bound_recursive_declarations() {
    use erp_report::HelperDependencies;
    // Dependencies are written in the report context's names, not in the arch's aliases.
    let helpers = [HelperDependencies::new("total", &["docs.secret"])];
    let policy = policy(true, false).with_helpers(&helpers);
    let root = each_doc("<p t-out='total(doc.name)'/>");
    let sources = [TemplateSource::new("sample.report", &root)];
    let error = check_attachment(
        &policy,
        &sources,
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Restricted { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 prints restricted field sale.order.secret; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
    );
    let recursive = [HelperDependencies::new("total", &["total()"])];
    let policy = super_policy().with_helpers(&recursive);
    let error = check_attachment(
        &policy,
        &sources,
        "sample.report",
        &fields,
        ReportLimits::default().with_call_depth(2).unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Limit {
            name: "helper_depth",
            maximum: 2
        }
    ));
    assert_eq!(
        error.to_string(),
        "report limit helper_depth exceeded or invalid (maximum 2)"
    );
}
fn super_policy() -> AttachmentPolicy<'static> {
    policy(true, true)
}

#[test]
fn implicit_record_formatting_is_not_a_security_pass() {
    for expression in ["doc", "[doc]", "doc.lines"] {
        let error = check(&format!("<p t-out='{expression}'/>"), true, true).unwrap_err();
        assert!(matches!(&error, ReportError::Unresolved { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "report sample.action: sample.report:1 cannot resolve `{expression}` for attachment field validation"
            )
        );
    }
}

#[test]
fn dynamic_attributes_cannot_stringify_records_without_declared_fields() {
    for acknowledged in [false, true] {
        for name in [
            "t-att-title",
            "t-att-alt",
            "t-att",
            "t-tag",
            "t-lang",
            "t-options",
        ] {
            let error = check(&format!("<img {name}='doc'/>"), true, acknowledged).unwrap_err();
            assert!(matches!(&error, ReportError::Unresolved { .. }));
            assert_eq!(
                error.to_string(),
                "report sample.action: sample.report:1 cannot resolve `doc` for attachment field validation"
            );
        }
        assert!(
            check("<p t-att-title='doc.name'/>", true, acknowledged)
                .unwrap()
                .is_empty()
        );
        let result = check("<p t-att-title='doc.secret'/>", true, acknowledged);
        if acknowledged {
            assert_eq!(result.unwrap()[0].field(), "sale.order.secret");
        } else {
            let error = result.unwrap_err();
            assert!(matches!(&error, ReportError::Restricted { .. }));
            assert_eq!(
                error.to_string(),
                "report sample.action: sample.report:1 prints restricted field sale.order.secret; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
            );
        }
    }
}

#[test]
fn template_identity_and_call_bounds_fail_explicitly() {
    let root = each_doc("<t t-call='sample.body'/>");
    let sources = [
        TemplateSource::new("sample.report", &root),
        TemplateSource::new("sample.body", "<p t-out='doc.name'/>"),
    ];
    assert!(
        check_attachment(
            &policy(true, false),
            &sources,
            "sample.report",
            &fields,
            ReportLimits::default().with_call_depth(2).unwrap()
        )
        .unwrap()
        .is_empty()
    );
    let error = check_attachment(
        &policy(true, false),
        &sources,
        "sample.report",
        &fields,
        ReportLimits::default().with_call_depth(1).unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Limit {
            name: "call_depth",
            maximum: 1
        }
    ));
    assert_eq!(
        error.to_string(),
        "report limit call_depth exceeded or invalid (maximum 1)"
    );
    let repeated = [sources[0], sources[0]];
    let error = check_attachment(
        &policy(true, false),
        &repeated,
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Unresolved { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 cannot resolve `duplicate template key` for attachment field validation"
    );
}

#[test]
fn context_names_resolve_only_when_the_caller_declares_them() {
    let sources = [TemplateSource::new(
        "sample.report",
        "<p t-field='o.secret'/>",
    )];
    for name in ["doc", "o", "object"] {
        let xml = format!("<p t-field='{name}.secret'/>");
        let error = check_attachment(
            &policy(true, true),
            &[TemplateSource::new("sample.report", &xml)],
            "sample.report",
            &fields,
            ReportLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(&error, ReportError::Unresolved { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "report sample.action: sample.report:1 cannot resolve `{name}.secret` for attachment field validation"
            )
        );
    }
    let bindings = [("o", Some("sale.order")), ("time", None)];
    let error = check_attachment(
        &policy(true, false).with_bindings(&bindings),
        &sources,
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Restricted { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 prints restricted field sale.order.secret; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
    );
    // The declared model decides the metadata: `o` as a line has no field `secret`.
    let bindings = [("o", Some("sale.line"))];
    let error = check_attachment(
        &policy(true, true).with_bindings(&bindings),
        &sources,
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Unresolved { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 cannot resolve `o.secret` for attachment field validation"
    );
    for bindings in [
        [("docs", Some("sale.line"))],
        [("", Some("sale.order"))],
        [("o", Some(""))],
    ] {
        let error = check_attachment(
            &policy(true, true).with_bindings(&bindings),
            &sources,
            "sample.report",
            &fields,
            ReportLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(&error, ReportError::Unresolved { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "report sample.action: sample.report:1 cannot resolve `invalid binding {}` for attachment field validation",
                bindings[0].0
            )
        );
    }
}

#[test]
fn a_call_body_is_read_before_the_callee_and_is_not_a_cycle() {
    // The body's assignment reaches the called template.
    let root = "<t t-call='sample.body'><t t-set='line' t-value='docs.lines'/></t>";
    let sources = [
        TemplateSource::new("sample.report", root),
        TemplateSource::new("sample.body", "<b t-out='line.cost'/>"),
    ];
    let findings = check_attachment(
        &policy(true, true),
        &sources,
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap();
    assert_eq!(
        findings
            .iter()
            .map(|f| (f.template(), f.line(), f.field()))
            .collect::<Vec<_>>(),
        [("sample.body", 1, "sale.line.cost")]
    );
    let nested = "<t t-call='sample.leaf'><t t-call='sample.leaf'/></t>";
    let sources = [
        TemplateSource::new("sample.report", nested),
        TemplateSource::new("sample.leaf", "<p>static</p>"),
    ];
    assert!(
        check_attachment(
            &policy(true, false),
            &sources,
            "sample.report",
            &fields,
            ReportLimits::default().with_call_depth(2).unwrap(),
        )
        .unwrap()
        .is_empty()
    );
    let sources = [
        TemplateSource::new("sample.report", "<t t-call='sample.other'/>"),
        TemplateSource::new("sample.other", "<t t-call='sample.report'/>"),
    ];
    let error = check_attachment(
        &policy(true, true),
        &sources,
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Unresolved { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.other:1 cannot resolve `cyclic t-call sample.report` for attachment field validation"
    );
}

#[test]
fn loop_helpers_are_scalars_and_end_with_their_loop() {
    assert!(
        check(
            "<p t-if='doc_first' t-out='doc_index + doc_size'/>",
            true,
            false
        )
        .unwrap()
        .is_empty()
    );
    let xml =
        "<t><t t-foreach='docs' t-as='doc'><p t-out='doc.name'/></t><p t-out='doc_last'/></t>";
    let error = check_attachment(
        &policy(true, false),
        &[TemplateSource::new("sample.report", xml)],
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Unresolved { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 cannot resolve `doc_last` for attachment field validation"
    );
}

#[test]
fn the_loop_value_alias_carries_the_loop_model_and_the_parity_helpers_are_scalars() {
    // `line_value` is the iterated line inside the loop, whatever an outer `line_value` was.
    let xml = "<t><t t-set='line_value' t-value='doc'/><t t-foreach='doc.lines' t-as='line'>\
               <b t-out='line_value.cost'/></t></t>";
    let findings = check(xml, true, true).unwrap();
    assert_eq!(
        findings.iter().map(|f| f.field()).collect::<Vec<_>>(),
        ["sale.line.cost"]
    );
    let error = check(xml, true, false).unwrap_err();
    assert!(matches!(&error, ReportError::Restricted { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 prints restricted field sale.line.cost; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
    );
    // The outer binding is back after the loop.
    let after = "<t><t t-set='line_value' t-value='doc'/><t t-foreach='doc.lines' t-as='line'/>\
                 <b t-out='line_value.secret'/></t>";
    assert_eq!(
        check(after, true, true).unwrap()[0].field(),
        "sale.order.secret"
    );
    // An outer record named like a parity helper is a scalar inside the loop: no field is read.
    let shadowed = "<t><t t-set='line_odd' t-value='doc'/><t t-foreach='doc.lines' t-as='line'>\
                    <p t-if='line_odd or line_even' t-out='line_parity'/></t></t>";
    assert!(check(shadowed, true, false).unwrap().is_empty());
}

#[test]
fn unknown_and_orphan_directives_are_never_a_pass() {
    for (xml, named) in [
        ("<b t-raw='doc.secret'/>", "t-raw"),
        ("<b t-out-x='doc.secret'/>", "t-out-x"),
        ("<b t-value='doc.secret'/>", "t-value without t-set"),
        ("<t t-foreach='doc.lines' t-as='1x'/>", "1x"),
    ] {
        let error = check(xml, true, true).unwrap_err();
        assert!(matches!(&error, ReportError::Unresolved { .. }), "{xml}");
        assert_eq!(
            error.to_string(),
            format!(
                "report sample.action: sample.report:1 cannot resolve `{named}` for attachment field validation"
            )
        );
    }
}

#[test]
fn conditional_assignments_end_with_their_branch() {
    let inside = "<t t-if='docs'><t t-set='rows' t-value='docs.lines'/><b t-foreach='rows' t-as='row' t-out='row.cost'/></t>";
    let findings = check_attachment(
        &policy(true, true),
        &[TemplateSource::new("sample.report", inside)],
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap();
    assert_eq!(findings[0].field(), "sale.line.cost");
    for xml in [
        "<t><t t-if='docs'><t t-set='user' t-value='docs.lines'/></t><b t-foreach='user' t-as='row' t-out='row.cost'/></t>",
        "<t><t t-if='docs' t-set='user' t-value='docs.lines'/><b t-foreach='user' t-as='row' t-out='row.cost'/></t>",
        "<t><t t-if='docs'/><t t-else=''><t t-set='user' t-value='docs.lines'/></t><b t-foreach='user' t-as='row' t-out='row.cost'/></t>",
    ] {
        let error = check_attachment(
            &policy(true, true),
            &[TemplateSource::new("sample.report", xml)],
            "sample.report",
            &fields,
            ReportLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(&error, ReportError::Unresolved { .. }), "{xml}");
        assert_eq!(
            error.to_string(),
            "report sample.action: sample.report:1 cannot resolve `user` for attachment field validation"
        );
    }
    // A declared plain value resolves and holds no record.
    let bindings = [("time", None)];
    assert!(
        check_attachment(
            &policy(true, false).with_bindings(&bindings),
            &[TemplateSource::new("sample.report", "<p t-out='time'/>")],
            "sample.report",
            &fields,
            ReportLimits::default(),
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn helper_dependencies_ignore_names_the_arch_shadows() {
    use erp_report::HelperDependencies;
    // On the line model `secret` is unrestricted; the helper reads the order's field whatever
    // the arch calls `docs` at the call site.
    let fields = |model: &str, field: &str| match (model, field) {
        ("sale.order", "lines") => Some(FieldInfo::relation("sale.line", false)),
        ("sale.order", "secret") => Some(FieldInfo::scalar(true)),
        ("sale.line", "secret") => Some(FieldInfo::scalar(false)),
        _ => None,
    };
    let helpers = [HelperDependencies::new("total", &["docs.secret"])];
    let xml = "<t t-foreach='docs.lines' t-as='docs'><b t-out='total()'/></t>";
    let error = check_attachment(
        &policy(true, false).with_helpers(&helpers),
        &[TemplateSource::new("sample.report", xml)],
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Restricted { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 prints restricted field sale.order.secret; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
    );
}

#[test]
fn image_sources_other_than_embedded_data_are_unresolved() {
    use erp_report::HelperDependencies;
    for (xml, named) in [
        (
            "<img src='/web/image/sale.order/7/secret_image'/>",
            "/web/image/sale.order/7/secret_image",
        ),
        (
            "<img t-attf-src='/web/image/sale.order/7/secret_image'/>",
            "/web/image/sale.order/7/secret_image",
        ),
        (
            "<img t-att-src=\"'/web/image/sale.order/7/' + doc.name\"/>",
            "'/web/image/sale.order/7/' + doc.name",
        ),
    ] {
        let error = check(xml, true, true).unwrap_err();
        assert!(matches!(&error, ReportError::Unresolved { .. }), "{xml}");
        assert_eq!(
            error.to_string(),
            format!(
                "report sample.action: sample.report:1 cannot resolve `{named}` for attachment field validation"
            )
        );
    }
    assert!(
        check("<img src='data:image/png;base64,AAAA'/>", true, false)
            .unwrap()
            .is_empty()
    );
    let helpers = [HelperDependencies::new("image_data_uri", &[])];
    let root = each_doc("<img t-att-src='image_data_uri(doc.name)'/>");
    assert!(
        check_attachment(
            &policy(true, false).with_helpers(&helpers),
            &[TemplateSource::new("sample.report", &root)],
            "sample.report",
            &fields,
            ReportLimits::default(),
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn binding_names_and_scope_copies_are_bounded() {
    use erp_views::{LimitKind, ViewLimits};
    let name = "n".repeat(64);
    assert!(
        check(&format!("<t t-set='{name}' t-value='1'/>"), true, false)
            .unwrap()
            .is_empty()
    );
    let error = check(&format!("<t t-set='{name}n' t-value='1'/>"), true, false).unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Limit {
            name: "binding_name_bytes",
            maximum: 64
        }
    ));
    assert_eq!(
        error.to_string(),
        "report limit binding_name_bytes exceeded or invalid (maximum 64)"
    );
    // 200 names of 60 bytes are copied at each of 3,000 loop scopes: 36 MB of copies against a
    // work budget of 20 million. The arch itself is far below every size limit.
    let mut xml = String::from("<t>");
    for index in 0..200 {
        xml.push_str(&format!("<t t-set='v{index:0>59}' t-value='1'/>"));
    }
    for _ in 0..3000 {
        xml.push_str("<t t-foreach='docs' t-as='d'/>");
    }
    xml.push_str("</t>");
    let maximum = ViewLimits::default().get(LimitKind::EvaluationSteps);
    let error = check_attachment(
        &policy(true, false),
        &[TemplateSource::new("sample.report", &xml)],
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, ReportError::Limit { name: "validation_work", maximum: m } if *m == maximum)
    );
    assert_eq!(
        error.to_string(),
        format!("report limit validation_work exceeded or invalid (maximum {maximum})")
    );
}

/// The example kept compiling and true.
#[test]
fn addon_guide_example() {
    let fields = |model: &str, field: &str| match (model, field) {
        ("sale.order", "name") => Some(FieldInfo::scalar(false)),
        ("sale.order", "margin") => Some(FieldInfo::scalar(true)), // the field carries groups
        _ => None,
    };
    let arch =
        "<t t-foreach='docs' t-as='doc'><p t-field='doc.name'/><p t-field='doc.margin'/></t>";
    let policy = AttachmentPolicy::new("sale.report_order", "sale.order").with_attachment(true);
    let sources = [TemplateSource::new("sale.report_order_document", arch)];
    let error = check_attachment(
        &policy,
        &sources,
        "sale.report_order_document",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "report sale.report_order: sale.report_order_document:1 prints restricted field sale.order.margin; set allow_attachment_group_fields explicitly to acknowledge the first printer's field view"
    );
}

#[test]
fn differing_reassignment_is_unresolved() {
    // One name, two models: neither branch of the analysis may hide the other's fields.
    let xml = "<t><t t-set='x' t-value='docs'/><t t-set='x' t-value='docs.lines'/></t>";
    let error = check_attachment(
        &policy(true, true),
        &[TemplateSource::new("sample.report", xml)],
        "sample.report",
        &fields,
        ReportLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(&error, ReportError::Unresolved { .. }));
    assert_eq!(
        error.to_string(),
        "report sample.action: sample.report:1 cannot resolve `x` for attachment field validation"
    );
}
