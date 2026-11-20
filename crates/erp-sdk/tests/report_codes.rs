// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, reason = "test assertions")]
use erp_sdk::report::{Code, Limits, ModuleGrid, QrLevel, ReportError, Symbology};

#[test]
fn addons_encode_and_fit_codes_through_the_sdk() {
    let grid = ModuleGrid::new(203).unwrap();
    assert_eq!(grid.dpi(), 203);
    assert_eq!(ModuleGrid::default().dpi(), ModuleGrid::DEFAULT_DPI);
    for dpi in [0, 71, 2401, u32::MAX] {
        let error = ModuleGrid::new(dpi).unwrap_err();
        assert!(matches!(error, ReportError::Code { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "report barcode code: the printer resolution must be in 72..=2400 dots per inch; got {dpi}"
            )
        );
    }
    for symbology in [Symbology::Code128, Symbology::Qr] {
        let code =
            Code::encode(symbology, "WH/OUT/00012", QrLevel::High, &Limits::default()).unwrap();
        let graphic = code.fit(225.0, 225.0, grid).unwrap();
        let dots = graphic.module() * f64::from(grid.dpi()) / 72.0;
        assert!(dots > 0.0 && (dots - dots.round()).abs() < 1e-9);
        assert!(!graphic.vector().rectangle_list().is_empty());
    }
}
