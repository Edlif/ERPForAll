// SPDX-License-Identifier: MIT OR Apache-2.0
//! Corpus scan contracts on synthetic bundles. Every compared count comes from `scan_corpus` output.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
use erp_report::{CorpusError, ReportError, ReportLimits, inspect, scan_corpus};
use erp_views::{LimitKind, ViewLimits};
use serde_json::{Value, json};

fn template(key: &str, arch: &str) -> Value {
    let addon = key.split('.').next().unwrap();
    json!({"key": key, "addon": addon, "path": format!("addons/{addon}/report.xml"), "line": 3,
           "arch": arch, "parent": null, "primary": false, "priority": 16, "selected_by": ["path"]})
}
fn child(key: &str, arch: &str, parent: &str, primary: bool) -> Value {
    let mut value = template(key, arch);
    value["parent"] = parent.into();
    value["primary"] = primary.into();
    value
}
/// Templates load in the order given: `sequence` is the position.
fn bundle(mut templates: Vec<Value>) -> Value {
    for (at, value) in templates.iter_mut().enumerate() {
        value["sequence"] = (at + 1).into();
    }
    json!({"schema": 1, "reference_commit": "synthetic-pin",
           "provenance": {"reference_mode": "synthetic-fixture"},
           "field_metadata": "literal candidates only",
           "templates": templates, "reports": [], "field_declarations": [], "findings": []})
}
fn scan(bundle: &Value) -> Value {
    scan_with(bundle, ReportLimits::default())
}
fn scan_with(bundle: &Value, limits: ReportLimits) -> Value {
    serde_json::to_value(scan_corpus(&bundle.to_string(), limits).unwrap()).unwrap()
}
fn entry<'a>(output: &'a Value, key: &str) -> &'a Value {
    output["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == key)
        .unwrap_or_else(|| panic!("no entry {key}"))
}
fn lowered(kind: LimitKind, value: usize) -> ReportLimits {
    ReportLimits::default().with_views(ViewLimits::default().lowered(kind, value).unwrap())
}

#[test]
fn own_results_keep_four_separate_denominators() {
    let unresolved = "<p t-if='1 +'/>";
    let input = bundle(vec![
        template("demo.good", "<p>Good</p>"),
        template("demo.bad", "<p class='one two'/>"),
        template("demo.dynamic", "<p t-att-class='classes'/>"),
        template("demo.unresolved", unresolved),
        child(
            "demo.extension",
            "<xpath expr='//p' position='inside'><b>More</b></xpath>",
            "demo.good",
            false,
        ),
    ]);
    let output = scan(&input);
    let demo = &output["addons"]["demo"];
    assert_eq!(demo["roots"], 4);
    assert_eq!(demo["extensions"], 1);
    assert_eq!(
        demo["own"],
        json!({"validated": 1, "dynamic": 1, "invalid": 1, "unresolved": 1})
    );
    let own_sum: u64 = ["validated", "dynamic", "invalid", "unresolved"]
        .iter()
        .map(|k| demo["own"][k].as_u64().unwrap())
        .sum();
    assert_eq!(Value::from(own_sum), demo["roots"]);
    assert_eq!(output["totals"], *demo);
    assert_eq!(
        output["missing_tokens"],
        json!({"class:one": {"occurrences": 1, "templates": 1},
               "class:two": {"occurrences": 1, "templates": 1}})
    );
    assert_eq!(entry(&output, "demo.good")["own"]["status"], "validated");
    let bad = entry(&output, "demo.bad");
    assert_eq!(bad["own"]["status"], "invalid");
    assert_eq!(
        bad["own"]["diagnostics"],
        json!([{"kind": "class", "token": "one", "line": 1},
               {"kind": "class", "token": "two", "line": 1}])
    );
    let dynamic = entry(&output, "demo.dynamic");
    assert_eq!(dynamic["own"]["status"], "dynamic");
    assert_eq!(dynamic["own"]["obligations"], json!({"class": 1}));
    let expected = inspect("demo.unresolved", unresolved, ReportLimits::default())
        .unwrap_err()
        .to_string();
    let failed = entry(&output, "demo.unresolved");
    assert_eq!(failed["own"]["status"], "unresolved");
    assert_eq!(failed["own"]["reason"], "expression");
    assert_eq!(failed["own"]["error"], expected);
    assert_eq!(
        output["unresolved_reasons"]["own"],
        json!({"expression": 1})
    );
    assert_eq!(output["missing_tokens_complete"], false);
    assert_eq!(output["discovery_complete"], true);
    assert_eq!(output["field_metadata_complete"], false);
    assert_eq!(output["rendering_verified"], false);
    assert_eq!(output["attachment_security_verified"], false);
    assert_eq!(output["reference_commit"], "synthetic-pin");
    assert_eq!(output["provenance"]["reference_mode"], "synthetic-fixture");
}

#[test]
fn an_extension_changes_its_root_and_is_never_a_root() {
    let input = bundle(vec![
        template("base.page", "<p>Page</p>"),
        child(
            "other.patch",
            "<xpath expr='//p' position='inside'><b class='nope'>More</b></xpath>",
            "base.page",
            false,
        ),
    ]);
    let output = scan(&input);
    assert_eq!(output["addons"]["base"]["roots"], 1);
    assert_eq!(output["addons"]["base"]["own"]["invalid"], 1);
    assert_eq!(output["addons"]["other"]["roots"], 0);
    assert_eq!(output["addons"]["other"]["extensions"], 1);
    assert_eq!(output["entries"].as_array().unwrap().len(), 1);
    assert_eq!(
        output["missing_tokens"]["class:nope"],
        json!({"occurrences": 1, "templates": 1})
    );
    assert_eq!(output["missing_tokens_complete"], true);
}

#[test]
fn closure_follows_static_calls_without_merging_token_counts() {
    let input = bundle(vec![
        template("demo.report", "<t t-call='demo.layout'><p>Body</p></t>"),
        template("demo.second", "<t t-call='demo.layout'/>"),
        template("demo.layout", "<div class='unknown-layout'/>"),
        template("demo.chooser", "<t t-call='{{ chosen }}'/>"),
        template("demo.lost", "<t t-call='demo.absent'/>"),
    ]);
    let output = scan(&input);
    let report = entry(&output, "demo.report");
    assert_eq!(report["own"]["status"], "validated");
    assert_eq!(report["closure"]["status"], "invalid");
    assert_eq!(report["closure"]["calls"], json!(["demo.layout"]));
    assert_eq!(
        entry(&output, "demo.layout")["closure"]["status"],
        "invalid"
    );
    // Two roots call the layout; its token is still counted for one template.
    assert_eq!(
        output["missing_tokens"]["class:unknown-layout"],
        json!({"occurrences": 1, "templates": 1})
    );
    let chooser = entry(&output, "demo.chooser");
    assert_eq!(chooser["own"]["status"], "dynamic");
    assert_eq!(chooser["own"]["obligations"], json!({"template": 1}));
    assert_eq!(chooser["closure"]["status"], "dynamic");
    let lost = entry(&output, "demo.lost");
    assert_eq!(lost["own"]["status"], "validated");
    assert_eq!(lost["closure"]["status"], "unresolved");
    assert_eq!(lost["closure"]["reason"], "missing_template");
    assert_eq!(
        lost["closure"]["error"],
        "report corpus: template `demo.lost` calls `demo.absent`, which is not in the bundle"
    );
    let demo = &output["addons"]["demo"];
    assert_eq!(
        demo["own"],
        json!({"validated": 3, "dynamic": 1, "invalid": 1, "unresolved": 0})
    );
    assert_eq!(
        demo["closure"],
        json!({"validated": 0, "dynamic": 1, "invalid": 3, "unresolved": 1})
    );
    assert_eq!(
        output["unresolved_reasons"]["closure"],
        json!({"missing_template": 1})
    );
    // Own results are complete, so the token list is; only the closure is open.
    assert_eq!(output["missing_tokens_complete"], true);
}

#[test]
fn call_cycles_are_unresolved_and_shared_callees_are_not_cycles() {
    let input = bundle(vec![
        template("demo.a", "<t t-call='demo.b'/>"),
        template("demo.b", "<t t-call='demo.a'/>"),
        template("demo.self", "<t t-call='demo.self'/>"),
        template(
            "demo.top",
            "<t><t t-call='demo.left'/><t t-call='demo.right'/></t>",
        ),
        template("demo.left", "<t t-call='demo.leaf'/>"),
        template("demo.right", "<t t-call='demo.leaf'/>"),
        template("demo.leaf", "<p>Leaf</p>"),
    ]);
    let output = scan(&input);
    for (key, message) in [
        (
            "demo.a",
            "report corpus: cyclic t-call demo.a -> demo.b -> demo.a",
        ),
        (
            "demo.b",
            "report corpus: cyclic t-call demo.b -> demo.a -> demo.b",
        ),
        (
            "demo.self",
            "report corpus: cyclic t-call demo.self -> demo.self",
        ),
    ] {
        let found = entry(&output, key);
        assert_eq!(found["own"]["status"], "validated", "{key}");
        assert_eq!(found["closure"]["status"], "unresolved", "{key}");
        assert_eq!(found["closure"]["reason"], "cyclic_call", "{key}");
        assert_eq!(found["closure"]["error"], message, "{key}");
    }
    assert_eq!(entry(&output, "demo.top")["closure"]["status"], "validated");
    assert_eq!(
        output["unresolved_reasons"]["closure"],
        json!({"cyclic_call": 3})
    );
}

#[test]
fn call_depth_is_refused_one_above_the_bound() {
    let input = bundle(vec![
        template("demo.a", "<t t-call='demo.b'/>"),
        template("demo.b", "<t t-call='demo.c'/>"),
        template("demo.c", "<p>End</p>"),
    ]);
    let at_bound = scan_with(&input, ReportLimits::default().with_call_depth(3).unwrap());
    assert_eq!(
        at_bound["addons"]["demo"]["closure"],
        json!({"validated": 3, "dynamic": 0, "invalid": 0, "unresolved": 0})
    );
    let above = scan_with(&input, ReportLimits::default().with_call_depth(2).unwrap());
    let a = entry(&above, "demo.a");
    assert_eq!(a["closure"]["status"], "unresolved");
    assert_eq!(a["closure"]["reason"], "limit:call_depth");
    assert_eq!(
        a["closure"]["error"],
        "report limit call_depth exceeded or invalid (maximum 2)"
    );
    assert_eq!(entry(&above, "demo.b")["closure"]["status"], "validated");
}

#[test]
fn an_unresolved_callee_leaves_the_caller_unresolved() {
    let input = bundle(vec![
        template("demo.caller", "<t t-call='demo.broken'/>"),
        template("demo.broken", "<p t-esc='1 +'/>"),
    ]);
    let output = scan(&input);
    let caller = entry(&output, "demo.caller");
    assert_eq!(caller["own"]["status"], "validated");
    assert_eq!(caller["closure"]["status"], "unresolved");
    assert_eq!(caller["closure"]["reason"], "callee");
    assert_eq!(
        caller["closure"]["error"],
        "report corpus: called template `demo.broken` is unresolved"
    );
    let broken = entry(&output, "demo.broken");
    assert_eq!(broken["closure"]["status"], "unresolved");
    assert_eq!(broken["closure"]["reason"], "own");
    assert_eq!(
        output["unresolved_reasons"]["closure"],
        json!({"callee": 1, "own": 1})
    );
}

#[test]
fn primary_copies_are_roots_and_missing_parents_are_named() {
    let rename = "<data><xpath expr='.' position='attributes'><attribute name='t-name'>demo.copy</attribute></xpath></data>";
    let input = bundle(vec![
        template("demo.page", "<t t-name='demo.page'><p>Page</p></t>"),
        child("demo.copy", rename, "demo.page", true),
        child("demo.orphan_copy", rename, "demo.gone", true),
        child(
            "demo.orphan_patch",
            "<xpath expr='//p' position='inside'><b/></xpath>",
            "demo.gone",
            false,
        ),
    ]);
    let output = scan(&input);
    let demo = &output["addons"]["demo"];
    assert_eq!(demo["roots"], 3);
    assert_eq!(demo["extensions"], 1);
    assert_eq!(entry(&output, "demo.copy")["own"]["status"], "validated");
    let orphan = entry(&output, "demo.orphan_copy");
    assert_eq!(orphan["own"]["status"], "unresolved");
    assert_eq!(orphan["own"]["reason"], "missing_parent");
    assert_eq!(
        orphan["own"]["error"],
        "report corpus: template `demo.orphan_copy` names the parent `demo.gone`, which is not in the bundle"
    );
    assert_eq!(output["orphan_extensions"], json!(["demo.orphan_patch"]));
    assert_eq!(output["discovery_complete"], false);
}

#[test]
fn report_actions_mark_printable_roots_and_attachment_candidates() {
    let mut input = bundle(vec![
        template("sale.report_order", "<t t-call='sale.layout'/>"),
        template("sale.layout", "<p class='nope'/>"),
        template("stock.report_picking", "<p>Picking</p>"),
    ]);
    input["reports"] = json!([
        {"key": "sale.action_order", "template": "sale.report_order", "model": "sale.order",
         "attachment": true, "path": "addons/sale/report.xml", "line": 9},
        {"key": "stock.action_picking", "template": "stock.report_picking", "model": "stock.picking",
         "attachment": false, "path": "addons/stock/report.xml", "line": 4},
        {"key": "stock.action_lost", "template": "stock.report_lost", "model": "stock.picking",
         "attachment": false, "path": "addons/stock/report.xml", "line": 12}
    ]);
    input["field_declarations"] = json!([
        {"model": "sale.order", "field": "margin", "groups": ["base.group_user"],
         "path": "addons/sale/models/order.py", "line": 7},
        {"model": "sale.order", "field": "name", "groups": [],
         "path": "addons/sale/models/order.py", "line": 5},
        {"model": "stock.picking", "field": "secret", "groups": ["stock.group_manager"],
         "path": "addons/stock/models/picking.py", "line": 3}
    ]);
    let output = scan(&input);
    assert_eq!(
        entry(&output, "sale.report_order")["action_reports"],
        json!(["sale.action_order"])
    );
    assert_eq!(entry(&output, "sale.layout")["action_reports"], json!([]));
    let sale = &output["addons"]["sale"];
    assert_eq!(sale["roots"], 2);
    assert_eq!(sale["action_roots"], 1);
    assert_eq!(
        sale["action_closure"],
        json!({"validated": 0, "dynamic": 0, "invalid": 1, "unresolved": 0})
    );
    assert_eq!(
        output["addons"]["stock"]["action_closure"],
        json!({"validated": 1, "dynamic": 0, "invalid": 0, "unresolved": 0})
    );
    assert_eq!(
        output["attachment_reports"],
        json!([{"key": "sale.action_order", "template": "sale.report_order", "model": "sale.order",
                "restricted_field_candidates": [
                    {"field": "margin", "groups": ["base.group_user"],
                     "path": "addons/sale/models/order.py", "line": 7}]}])
    );
    assert_eq!(
        output["reports_without_template"],
        json!(["stock.action_lost"])
    );
    assert_eq!(output["discovery_complete"], false);
    assert_eq!(
        output["counts"],
        json!({"templates": 3, "reports": 3, "field_declarations": 3, "findings": {}})
    );
}

#[test]
fn the_gate_sums_named_addons_and_refuses_an_absent_one() {
    let input = bundle(vec![
        template("sale.good", "<p>Good</p>"),
        template("sale.dynamic", "<p t-att-class='c'/>"),
        template("sale.bad", "<p class='nope'/>"),
        template("stock.good", "<p>Good</p>"),
        template("web.bad", "<p class='nope'/>"),
    ]);
    let scanned = || scan_corpus(&input.to_string(), ReportLimits::default()).unwrap();
    let output = serde_json::to_value(scanned().with_gate(&["stock", "sale"]).unwrap()).unwrap();
    let gate = &output["gate"];
    assert_eq!(gate["addons"], json!(["sale", "stock"]));
    assert_eq!(gate["counts"]["roots"], 4);
    assert_eq!(
        gate["counts"]["own"],
        json!({"validated": 2, "dynamic": 1, "invalid": 1, "unresolved": 0})
    );
    assert_eq!(
        gate["permille"],
        json!({"own_validated": 500, "own_validated_or_dynamic": 750,
               "closure_validated": 500, "closure_validated_or_dynamic": 750,
               "action_closure_validated": null, "action_closure_validated_or_dynamic": null})
    );
    assert_eq!(
        serde_json::to_value(scanned()).unwrap()["gate"],
        Value::Null
    );
    let absent = scanned().with_gate(&["sale", "purchase"]).unwrap_err();
    assert!(matches!(
        &absent,
        ReportError::Corpus(CorpusError::GateAddon(name)) if name == "purchase"
    ));
    assert_eq!(
        absent.to_string(),
        "report corpus: gate addon `purchase` has no root template in the corpus"
    );
    let empty = scanned().with_gate(&[]).unwrap_err();
    assert!(matches!(
        &empty,
        ReportError::Corpus(CorpusError::EmptyGate)
    ));
    assert_eq!(
        empty.to_string(),
        "report corpus: the gate addon list is empty"
    );
}

#[test]
fn a_malformed_bundle_is_a_typed_refusal_never_an_empty_report() {
    let error = scan_corpus("{}", ReportLimits::default()).unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Corpus(CorpusError::Bundle(_))
    ));
    assert_eq!(
        error.to_string(),
        "report corpus: not a schema 1 extractor bundle: missing field `schema` at line 1 column 2"
    );
    let mut wrong_schema = bundle(vec![]);
    wrong_schema["schema"] = 2.into();
    let error = scan_corpus(&wrong_schema.to_string(), ReportLimits::default()).unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Corpus(CorpusError::Schema(2))
    ));
    assert_eq!(
        error.to_string(),
        "report corpus: bundle schema 2 is unsupported; supported schema is 1"
    );
    let twice = bundle(vec![
        template("demo.page", "<p/>"),
        template("demo.page", "<p/>"),
    ]);
    let error = scan_corpus(&twice.to_string(), ReportLimits::default()).unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Corpus(CorpusError::DuplicateTemplate(key)) if key == "demo.page"
    ));
    assert_eq!(
        error.to_string(),
        "report corpus: duplicate template key `demo.page`"
    );
    let mut extra = bundle(vec![]);
    extra["surprise"] = true.into();
    let error = scan_corpus(&extra.to_string(), ReportLimits::default()).unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Corpus(CorpusError::Bundle(_))
    ));
    assert!(
        error.to_string().starts_with(
            "report corpus: not a schema 1 extractor bundle: unknown field `surprise`"
        ),
        "{error}"
    );
    let mut scope = bundle(vec![]);
    scope["findings"] =
        json!([{"path": "a.xml", "line": 1, "kind": "x", "scope": "other", "detail": ""}]);
    let error = scan_corpus(&scope.to_string(), ReportLimits::default()).unwrap_err();
    assert!(matches!(
        &error,
        ReportError::Corpus(CorpusError::Bundle(_))
    ));
    assert!(
        error
            .to_string()
            .starts_with("report corpus: not a schema 1 extractor bundle: unknown variant `other`"),
        "{error}"
    );
}

#[test]
fn finding_scopes_decide_which_statement_is_incomplete() {
    let finding = |scope: &str| json!({"path": "addons/demo/a.xml", "line": 2, "kind": "example", "scope": scope, "detail": "why"});
    let mut input = bundle(vec![template("demo.page", "<p/>")]);
    input["findings"] = json!([finding("fields"), finding("security")]);
    let output = scan(&input);
    assert_eq!(output["discovery_complete"], true);
    assert_eq!(output["field_metadata_complete"], false);
    assert_eq!(
        output["counts"]["findings"],
        json!({"fields": 1, "security": 1})
    );
    assert_eq!(output["extraction_findings"], input["findings"]);
    input["findings"] = json!([finding("templates")]);
    assert_eq!(scan(&input)["discovery_complete"], false);
}

#[test]
fn scan_limits_refuse_the_run_instead_of_changing_counts() {
    let input = bundle(vec![
        template("demo.a", "<p>A</p>"),
        template("demo.b", "<p>B</p>"),
    ])
    .to_string();
    for (limits, name, maximum) in [
        (lowered(LimitKind::CatalogueBytes, 64), "corpus_bytes", 64),
        (lowered(LimitKind::CatalogueViews, 1), "corpus_templates", 1),
        (lowered(LimitKind::BatchSteps, 3), "corpus_work", 3),
    ] {
        let error = scan_corpus(&input, limits).unwrap_err();
        assert!(
            matches!(&error, ReportError::Limit { name: n, maximum: m } if *n == name && *m == maximum),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            format!("report limit {name} exceeded or invalid (maximum {maximum})")
        );
    }
    // A template over a per-template limit is that root's result; the run continues.
    let output =
        serde_json::to_value(scan_corpus(&input, lowered(LimitKind::ArchBytes, 4)).unwrap())
            .unwrap();
    assert_eq!(output["addons"]["demo"]["own"]["unresolved"], 2);
    assert_eq!(output["unresolved_reasons"]["own"], json!({"view": 2}));
}

#[test]
fn the_same_bundle_scans_to_the_same_sorted_report() {
    let input = bundle(vec![
        template("zeta.page", "<p class='b a'/>"),
        template("alpha.page", "<t t-call='zeta.page'/>"),
    ]);
    let first = scan(&input);
    assert_eq!(first, scan(&input));
    let keys: Vec<_> = first["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["alpha.page", "zeta.page"]);
    assert_eq!(first["schema"], 1);
    assert_eq!(first["profile"], 1);
    assert_eq!(
        first["combination"],
        "every extension in the bundle is applied"
    );
}

#[test]
fn extensions_of_equal_priority_apply_in_load_order_not_in_key_order() {
    // `zeta.first` loads first and adds the element `alpha.second` then extends.
    let input = bundle(vec![
        template("base.page", "<p>Page</p>"),
        child(
            "zeta.first",
            "<xpath expr='//p' position='inside'><b>One</b></xpath>",
            "base.page",
            false,
        ),
        child(
            "alpha.second",
            "<xpath expr='//b' position='inside'><i class='nope'>Two</i></xpath>",
            "base.page",
            false,
        ),
    ]);
    let output = scan(&input);
    let page = entry(&output, "base.page");
    assert_eq!(page["own"]["status"], "invalid");
    assert_eq!(
        page["own"]["diagnostics"],
        json!([{"kind": "class", "token": "nope", "line": 1}])
    );
    // The other order cannot find `//b`: the root is unresolved and keeps the cause.
    let mut reversed = input.clone();
    reversed["templates"][1]["sequence"] = 3.into();
    reversed["templates"][2]["sequence"] = 2.into();
    let failed = scan(&reversed);
    let page = entry(&failed, "base.page");
    assert_eq!(page["own"]["status"], "unresolved");
    assert_eq!(page["own"]["reason"], "view");
    assert_eq!(
        page["own"]["error"],
        "base.page: error in view 'alpha.second' (id 2): Element '<xpath expr=\"//b\">' cannot be located in parent view"
    );
}
