// SPDX-License-Identifier: MIT OR Apache-2.0
//! Declared technical parameters: constructors and publication checks.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
use erp_fields::{ErrorKind, Value};
use erp_registry::settings::{ParameterDecl, ParameterRegistry, SettingGroupDecl, SettingKind};
use std::collections::{BTreeMap, BTreeSet};

fn message(result: erp_fields::Result<ParameterDecl>) -> String {
    let error = result.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    error.message().to_owned()
}

#[test]
fn declarations_check_floor_default_and_bound_by_key() {
    let batch =
        ParameterDecl::integer("shop.import_batch", "Import batch", 500, (1, 10_000)).unwrap();
    assert!(matches!(
        batch.kind(),
        SettingKind::Integer {
            min: 1,
            max: 10_000
        }
    ));
    assert_eq!(batch.default_value(), &Value::Int(500));
    assert_eq!(batch.default_text(), "500");
    assert_eq!(batch.label(), "Import batch");
    assert!(!batch.is_secret());
    // default outside the bounds
    assert_eq!(
        message(ParameterDecl::integer(
            "shop.import_batch",
            "Import batch",
            0,
            (1, 10)
        )),
        "Parameter shop.import_batch has a default outside a whole number from 1 to 10."
    );
    // floor above bound
    assert_eq!(
        message(ParameterDecl::integer(
            "shop.import_batch",
            "Import batch",
            5,
            (10, 1)
        )),
        "Parameter shop.import_batch needs a finite floor not above its hard bound."
    );
    // every number has a floor of at least 1 whatever its key is called, so 0 can never mean
    // unlimited (review cycle 16 Rust S6: `_ms`, `_attempts`, `max_*` keys escaped a word list)
    let refusal = |key: &str| {
        format!(
            "Parameter {key} has a floor below 1; a number whose 0 or fraction has a meaning is \
             declared with integer_floor_below_one or float_floor_below_one and the reason."
        )
    };
    for key in [
        "shop.import_batch",
        "shop.retry_base_ms",
        "shop.quant_max_attempts",
        "shop.max_lines",
        "shop.horizon_months",
    ] {
        assert_eq!(
            message(ParameterDecl::integer(key, "Quantity", 0, (0, 10))),
            refusal(key)
        );
    }
    assert_eq!(
        message(ParameterDecl::float(
            "shop.ratio",
            "Ratio",
            0.5,
            (0.5, 10.0)
        )),
        refusal("shop.ratio")
    );
    // a lower floor that has a meaning is declared with its reason
    let off = ParameterDecl::integer_floor_below_one(
        "shop.after_login",
        "After login",
        0,
        (0, 10),
        "0 turns the rule off",
    )
    .unwrap();
    assert!(matches!(
        off.kind(),
        SettingKind::Integer { min: 0, max: 10 }
    ));
    assert!(
        ParameterDecl::float_floor_below_one("shop.ratio", "Ratio", 0.0, (0.0, 1.0), "a share")
            .is_ok()
    );
    for (floor, reason) in [(0, " "), (1, "not below one")] {
        assert_eq!(
            message(ParameterDecl::integer_floor_below_one(
                "shop.after_login",
                "After login",
                1,
                (floor, 10),
                reason
            )),
            "Parameter shop.after_login is declared with a floor below 1 but has no reason or no such floor."
        );
    }
    assert!(
        ParameterDecl::float_floor_below_one(
            "shop.ratio",
            "Ratio",
            0.0,
            (0.0, f64::INFINITY),
            "a share"
        )
        .is_err()
    );
    assert_eq!(
        message(ParameterDecl::selection(
            "shop.mode",
            "Mode",
            "fast",
            &[("slow", "Slow")]
        )),
        "Parameter shop.mode has a default outside one of slow."
    );
    assert!(
        ParameterDecl::selection(
            "shop.mode",
            "Mode",
            "slow",
            &[("slow", "Slow"), ("slow", "Again")]
        )
        .is_err()
    );
    assert!(ParameterDecl::string("shop.prefix", "Prefix", "INV", 0).is_err());
    assert!(ParameterDecl::string("shop.prefix", "Prefix", "INVOICE", 3).is_err());
    assert!(ParameterDecl::boolean("shop.flag", "", true).is_err());
    // a malformed key is not echoed
    assert_eq!(
        message(ParameterDecl::boolean("Shop Flag\n", "Flag", true)),
        "A parameter key must be lowercase dotted names of letters, digits and _, at most 128 bytes."
    );
    let secret = ParameterDecl::string("shop.api_token", "API token", "tok-xyz", 256)
        .unwrap()
        .secret();
    assert!(secret.is_secret());
    assert!(!format!("{secret:?}").contains("tok-xyz"));
}

#[test]
fn proposed_text_is_checked_by_kind_and_bounds_without_echoing_it() {
    let batch =
        ParameterDecl::integer("shop.import_batch", "Import batch", 500, (1, 10_000)).unwrap();
    assert_eq!(batch.check_text(Some(" 20 ")).unwrap(), Value::Int(20));
    assert_eq!(batch.check_text(None).unwrap(), Value::Int(500));
    assert_eq!(batch.check_text(Some("")).unwrap(), Value::Int(500));
    for bad in ["0", "10001", "abc", "1.5", "secret-ish"] {
        let error = batch.check_text(Some(bad)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(
            error.message(),
            "shop.import_batch takes a whole number from 1 to 10000."
        );
    }
    let flag = ParameterDecl::boolean("shop.flag", "Flag", false).unwrap();
    assert_eq!(flag.check_text(Some("True")).unwrap(), Value::Bool(true));
    assert_eq!(flag.check_text(Some("off")).unwrap(), Value::Bool(false));
    assert_eq!(
        flag.check_text(Some("maybe")).unwrap_err().message(),
        "shop.flag takes true or false."
    );
    let ratio =
        ParameterDecl::float_floor_below_one("shop.ratio", "Ratio", 1.0, (0.5, 2.0), "a factor")
            .unwrap();
    assert_eq!(ratio.default_text(), "1.0");
    assert!(ratio.check_text(Some("NaN")).is_err());
    assert!(ratio.check_text(Some("2.5")).is_err());
    let even = ParameterDecl::integer("shop.even", "Even", 2, (1, 10))
        .unwrap()
        .validator(|value| match value {
            Value::Int(n) if n % 2 == 0 => Ok(()),
            _ => Err(erp_fields::Error::validation("shop.even must be even.")),
        })
        .unwrap();
    assert_eq!(
        even.check_text(Some("3")).unwrap_err().message(),
        "shop.even must be even."
    );
    assert!(
        ParameterDecl::integer("shop.even", "Even", 1, (1, 10))
            .unwrap()
            .validator(|_| Err(erp_fields::Error::validation("never")))
            .is_err()
    );
}

#[test]
fn stored_text_outside_the_declaration_is_invalid_never_the_default() {
    let batch =
        ParameterDecl::integer("shop.import_batch", "Import batch", 500, (1, 10_000)).unwrap();
    assert_eq!(batch.read_text(None).unwrap(), Value::Int(500));
    assert_eq!(batch.read_text(Some("")).unwrap(), Value::Int(500));
    assert_eq!(batch.read_text(Some("7")).unwrap(), Value::Int(7));
    for bad in ["0", "abc"] {
        let error = batch.read_text(Some(bad)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(
            error.message(),
            "Stored parameter shop.import_batch is invalid."
        );
    }
    // the canonical text an audit row compares: the default for an empty row, trimmed numbers
    assert_eq!(batch.effective_text(None).unwrap(), "500");
    assert_eq!(batch.effective_text(Some(" +7 ")).unwrap(), "7");
    assert!(batch.effective_text(Some("0")).is_err());
    let flag = ParameterDecl::boolean("shop.flag", "Flag", false).unwrap();
    assert_eq!(flag.effective_text(Some("on")).unwrap(), "True");
    // a default row is shipped unless the declaration says the absent value follows something else
    assert!(batch.has_default_row());
    assert!(!batch.without_default_row().has_default_row());
}

fn installed(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}
fn low_high() -> Vec<ParameterDecl> {
    vec![
        ParameterDecl::integer("shop.low_count", "Low", 1, (1, 10)).unwrap(),
        ParameterDecl::integer("shop.high_count", "High", 10, (1, 10)).unwrap(),
    ]
}
fn ordered(values: &BTreeMap<String, Value>) -> erp_fields::Result<()> {
    match (&values["shop.low_count"], &values["shop.high_count"]) {
        (Value::Int(low), Value::Int(high)) if low <= high => Ok(()),
        _ => Err(erp_fields::Error::validation(
            "shop.low_count must not exceed shop.high_count.",
        )),
    }
}
fn group() -> SettingGroupDecl {
    SettingGroupDecl::new(
        "shop.counts",
        &["shop.low_count", "shop.high_count"],
        ordered,
    )
    .unwrap()
}

#[test]
fn publication_refuses_duplicates_foreign_prefixes_and_bad_groups_by_key() {
    let all = installed(&["base", "shop", "other"]);
    let build = |contributions, taken: &[&str]| {
        ParameterRegistry::build(contributions, &all, installed(taken))
            .map(drop)
            .unwrap_err()
            .message()
            .to_owned()
    };
    // twice among parameters, and once as a parameter and once as a setting or page item
    let one = || ParameterDecl::boolean("shop.flag", "Flag", false).unwrap();
    assert_eq!(
        build(vec![("shop".into(), vec![one(), one()], vec![])], &[]),
        "Parameter shop.flag has more than one declaration."
    );
    assert_eq!(
        build(
            vec![
                ("shop".into(), vec![one()], vec![]),
                ("shop".into(), vec![one()], vec![])
            ],
            &[]
        ),
        "Parameter shop.flag has more than one declaration."
    );
    assert_eq!(
        build(vec![("shop".into(), vec![one()], vec![])], &["shop.flag"]),
        "Parameter shop.flag has more than one declaration."
    );
    // another addon's prefix, or a base family claimed by another addon
    assert_eq!(
        build(vec![("other".into(), vec![one()], vec![])], &[]),
        "Parameter shop.flag is outside the key prefix of addon other."
    );
    let session = ParameterDecl::integer("sessions.idle_secs", "Idle", 60, (1, 600)).unwrap();
    assert_eq!(
        build(vec![("shop".into(), vec![session.clone()], vec![])], &[]),
        "Parameter sessions.idle_secs is outside the key prefix of addon shop."
    );
    assert!(
        ParameterRegistry::build(
            vec![("base".into(), vec![session], vec![])],
            &all,
            BTreeSet::new()
        )
        .is_ok()
    );
    // a group over keys the addon does not declare, or refusing the defaults
    assert_eq!(
        build(vec![("shop".into(), vec![one()], vec![group()])], &[]),
        "Parameter group shop.counts names shop.high_count, not a parameter of shop."
    );
    let mut inverted = low_high();
    inverted[0] = ParameterDecl::integer("shop.low_count", "Low", 10, (1, 10)).unwrap();
    inverted[1] = ParameterDecl::integer("shop.high_count", "High", 1, (1, 10)).unwrap();
    assert_eq!(
        build(vec![("shop".into(), inverted, vec![group()])], &[]),
        "shop.low_count must not exceed shop.high_count."
    );
    assert!(
        ParameterRegistry::build(
            vec![("shop".into(), vec![one()], vec![])],
            &installed(&["base"]),
            BTreeSet::new()
        )
        .is_err()
    );
}

#[test]
fn undeclared_keys_under_a_declaring_prefix_are_refused_others_are_free() {
    let all = installed(&["base", "shop", "quiet"]);
    let registry = ParameterRegistry::build(
        vec![
            ("shop".into(), low_high(), vec![]),
            (
                "base".into(),
                vec![ParameterDecl::integer("sessions.idle_secs", "Idle", 60, (1, 600)).unwrap()],
                vec![],
            ),
            ("quiet".into(), vec![], vec![]),
        ],
        &all,
        installed(&["shop.setting_key"]),
    )
    .unwrap();
    assert_eq!(registry.owner("shop.low_count"), Some("shop"));
    assert_eq!(registry.owner("sessions.idle_secs"), Some("base"));
    assert!(registry.get("shop.typo").is_none());
    for refused in [
        "shop.typo",
        "base.typo",
        "sessions.typo",
        "attachments.typo",
        "password.typo",
    ] {
        assert_eq!(
            registry.refuse_undeclared(refused).unwrap_err().message(),
            format!("Parameter {refused} is not declared by an installed addon.")
        );
    }
    for free in [
        "shop.low_count",
        "shop.setting_key",
        "quiet.anything",
        "web.base.url",
        "database.uuid",
        "integrator",
    ] {
        registry.refuse_undeclared(free).unwrap();
    }
}

#[test]
fn groups_validate_complete_snapshots_of_their_members() {
    let registry = ParameterRegistry::build(
        vec![("shop".into(), low_high(), vec![group()])],
        &installed(&["shop"]),
        BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(
        registry.group_members(["shop.low_count"]),
        vec!["shop.high_count".to_owned(), "shop.low_count".to_owned()]
    );
    assert!(registry.group_members(["shop.other"]).is_empty());
    let snapshot = |low, high| {
        BTreeMap::from([
            ("shop.low_count".to_owned(), Value::Int(low)),
            ("shop.high_count".to_owned(), Value::Int(high)),
        ])
    };
    registry.validate_groups(&snapshot(2, 3)).unwrap();
    assert_eq!(
        registry
            .validate_groups(&snapshot(4, 3))
            .unwrap_err()
            .message(),
        "shop.low_count must not exceed shop.high_count."
    );
    let partial = BTreeMap::from([("shop.low_count".to_owned(), Value::Int(1))]);
    assert_eq!(
        registry.validate_groups(&partial).unwrap_err().kind(),
        ErrorKind::Programming
    );
}
