// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
//! Extended linear symbols preserve payloads and fit their complete geometry inside the box.
use erp_report::{Code, LimitKind, Limits, ModuleGrid, QrLevel, ReportError, Symbology};

#[test]
fn extended39_accepts_full_ascii_without_rewriting_the_payload() {
    assert_eq!(
        Symbology::from_name("Extended39"),
        Some(Symbology::Extended39)
    );
    for byte in 0u8..=127 {
        let value = char::from(byte).to_string();
        let code = Code::encode(
            Symbology::Extended39,
            &value,
            QrLevel::Low,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(code.text(), value);
        assert_eq!(code.symbology(), Symbology::Extended39);
    }
}

#[test]
fn gs1_code128_preserves_raw_input_and_adds_a_function_symbol() {
    let value = "10LOT-7\u{1d}2110";
    let gs1 = Code::encode_gs1_code128(value, &Limits::default()).unwrap();
    let plain = Code::encode(Symbology::Code128, value, QrLevel::Low, &Limits::default()).unwrap();
    assert_eq!(gs1.text(), value);
    assert_eq!(gs1.symbology(), Symbology::Code128);
    assert_ne!(
        (0..gs1.modules_across())
            .map(|x| gs1.is_dark(x, 0))
            .collect::<Vec<_>>(),
        (0..plain.modules_across())
            .map(|x| plain.is_dark(x, 0))
            .collect::<Vec<_>>()
    );
}

#[test]
fn itf14_bearers_are_inside_the_box_and_preserve_horizontal_quiet_zones() {
    for dpi in [203, 300, 600] {
        for thickness in [2u8, 5, 10] {
            let code = Code::encode(
                Symbology::Itf14,
                "1540014128876",
                QrLevel::Low,
                &Limits::default(),
            )
            .unwrap()
            .with_bearer_modules(thickness)
            .unwrap();
            let fitted = code
                .fit(300.0, 80.0, ModuleGrid::new(dpi).unwrap())
                .unwrap();
            let vector = fitted.vector();
            let span = vector.width() - 20.0;
            let strips: Vec<_> = vector
                .rectangle_list()
                .iter()
                .filter(|r| r[2] == span)
                .collect();
            assert_eq!(strips.len(), 2);
            assert!(strips.contains(&&[10.0, 0.0, span, f64::from(thickness)]));
            assert!(strips.contains(&&[
                10.0,
                vector.height() - f64::from(thickness),
                span,
                f64::from(thickness)
            ]));
            for &[x, y, w, h] in vector.rectangle_list() {
                assert!(x >= 10.0 && x + w <= vector.width() - 10.0);
                assert!(y >= 0.0 && h > 0.0 && y + h <= vector.height() + 1e-9);
                if w != span {
                    assert_eq!(y, f64::from(thickness));
                    assert!((y + h - (vector.height() - f64::from(thickness))).abs() < 1e-9);
                }
            }
            assert!((fitted.height() - 80.0).abs() < 1e-9);
            let dots = fitted.module() * f64::from(dpi) / 72.0;
            assert!((dots - dots.round()).abs() < 1e-9);
        }
    }
}

fn assert_code_error(error: ReportError, symbology: &'static str, reason: &str) {
    assert!(matches!(&error, ReportError::Code { symbology: got, .. } if *got == symbology));
    assert_eq!(
        error.to_string(),
        format!("report {symbology} code: {reason}")
    );
}

#[test]
fn gs1_code128_rejects_unrepresentable_input_without_substitution() {
    for value in ["é", "\n", "\0", "\u{7f}", "ñ"] {
        assert_code_error(
            Code::encode_gs1_code128(value, &Limits::default()).unwrap_err(),
            "Code128",
            "GS1 data must contain printable ASCII or the ASCII GS separator (0x1D)",
        );
    }
    assert_code_error(
        Code::encode_gs1_code128("", &Limits::default()).unwrap_err(),
        "Code128",
        "there is no value to encode",
    );
}

#[test]
fn bearer_option_is_bounded_and_only_available_for_itf14() {
    for value in [0, 1, 11, 255] {
        let code = Code::encode(
            Symbology::Itf14,
            "1540014128876",
            QrLevel::Low,
            &Limits::default(),
        )
        .unwrap();
        assert_code_error(
            code.with_bearer_modules(value).unwrap_err(),
            "ITF14",
            &format!("bearer_modules must be in 2..=10; got {value}"),
        );
    }
    for kind in [
        Symbology::Code128,
        Symbology::Extended39,
        Symbology::Interleaved2of5,
        Symbology::DataMatrix,
    ] {
        let code = Code::encode(kind, "1234", QrLevel::Low, &Limits::default()).unwrap();
        assert_code_error(
            code.with_bearer_modules(2).unwrap_err(),
            kind.name(),
            "bearer_modules is available only for ITF14",
        );
    }
}

#[test]
fn new_encoders_apply_the_raw_input_limit_at_its_boundary() {
    let limits = Limits::default().lowered(LimitKind::CodeBytes, 8).unwrap();
    for gs1 in [false, true] {
        let encode = |value| {
            if gs1 {
                Code::encode_gs1_code128(value, &limits)
            } else {
                Code::encode(Symbology::Extended39, value, QrLevel::Low, &limits)
            }
        };
        assert_eq!(encode("abcd1234").unwrap().text(), "abcd1234");
        let error = encode("abcd12345").unwrap_err();
        assert!(matches!(
            error,
            ReportError::DocumentLimit {
                kind: LimitKind::CodeBytes,
                actual: 9,
                max: 8
            }
        ));
        assert_eq!(error.to_string(), "report limit code_bytes exceeded: 9 > 8");
    }
}

#[test]
fn default_bearers_match_explicit_two_modules_without_changing_i2of5() {
    let limits = Limits::default();
    let code = Code::encode(Symbology::Itf14, "1540014128876", QrLevel::Low, &limits).unwrap();
    let default = code.fit(300.0, 80.0, ModuleGrid::default()).unwrap();
    let explicit = code
        .with_bearer_modules(2)
        .unwrap()
        .fit(300.0, 80.0, ModuleGrid::default())
        .unwrap();
    assert_eq!(default.vector(), explicit.vector());
    let ordinary = Code::encode(
        Symbology::Interleaved2of5,
        "15400141288763",
        QrLevel::Low,
        &limits,
    )
    .unwrap();
    let fitted = ordinary.fit(300.0, 80.0, ModuleGrid::default()).unwrap();
    assert!(
        fitted
            .vector()
            .rectangle_list()
            .iter()
            .all(|r| r[1] == 0.0 && r[3] == fitted.vector().height())
    );
}

#[test]
fn extended39_refuses_non_ascii_empty_input_and_expanded_symbols_that_do_not_fit() {
    let limits = Limits::default();
    for value in ["é", "a東京", "🦀"] {
        let character = value.chars().find(|c| !c.is_ascii()).unwrap();
        assert_code_error(
            Code::encode(Symbology::Extended39, value, QrLevel::Low, &limits).unwrap_err(),
            "Extended39",
            &format!("it prints ASCII characters; `{value}` has `{character}`"),
        );
    }
    assert_code_error(
        Code::encode(Symbology::Extended39, "", QrLevel::Low, &limits).unwrap_err(),
        "Extended39",
        "there is no value to encode",
    );
    let ordinary = Code::encode(Symbology::Code39, "AAAA", QrLevel::Low, &limits).unwrap();
    assert!(
        ordinary
            .fit(60.0, 30.0, ModuleGrid::default())
            .unwrap()
            .width()
            <= 60.0
    );
    let expanded = Code::encode(Symbology::Extended39, "aaaa", QrLevel::Low, &limits).unwrap();
    assert_eq!(expanded.text(), "aaaa");
    assert_code_error(
        expanded.fit(60.0, 30.0, ModuleGrid::default()).unwrap_err(),
        "Extended39",
        "`aaaa` is 179 modules and needs 85.9 points at the smallest module (2 dots at 300 dpi); its box gives 60.0",
    );
}

#[test]
fn bearer_minimum_height_is_admitted_and_just_below_it_is_refused() {
    for (thickness, minimum, too_short, expected) in [
        (
            2,
            2.40,
            2.39,
            "bearer_modules 2 needs at least 2.40 points of height at 300 dpi; got 2.39",
        ),
        (
            10,
            10.08,
            10.07,
            "bearer_modules 10 needs at least 10.08 points of height at 300 dpi; got 10.07",
        ),
    ] {
        let code = Code::encode(
            Symbology::Itf14,
            "15400141288763",
            QrLevel::Low,
            &Limits::default(),
        )
        .unwrap()
        .with_bearer_modules(thickness)
        .unwrap();
        let fitted = code.fit(300.0, minimum, ModuleGrid::default()).unwrap();
        assert!((fitted.module() - 0.48).abs() < 1e-9);
        let span = fitted.vector().width() - 20.0;
        for rectangle in fitted
            .vector()
            .rectangle_list()
            .iter()
            .filter(|r| r[2] != span)
        {
            assert!((rectangle[3] - 1.0).abs() < 1e-9);
        }
        assert_code_error(
            code.fit(300.0, too_short, ModuleGrid::default())
                .unwrap_err(),
            "ITF14",
            expected,
        );
    }
}
