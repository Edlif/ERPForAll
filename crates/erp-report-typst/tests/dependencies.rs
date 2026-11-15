// SPDX-License-Identifier: MIT OR Apache-2.0
use std::process::Command;

/// crate without its fonts feature; forbidden-edge fixtures are in the layer checker's script test)
#[test]
fn renderer_dependency_graph_is_isolated() {
    let output = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "tree",
            "--locked",
            "--offline",
            "-p",
            "erp-report-typst",
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).unwrap();
    for line in tree.lines() {
        let name = line.split_whitespace().next().unwrap();
        assert!(
            ![
                "erp-orm",
                "erp-sql",
                "erp-server",
                "tokio-postgres",
                "postgres",
                "sqlx",
                "rusqlite",
                "typst-kit"
            ]
            .contains(&name),
            "forbidden renderer dependency: {line}"
        );
        if name.starts_with("typst") {
            let version = line.split_whitespace().nth(1).unwrap();
            assert_eq!(version, "v0.15.1");
            // The renderer identity reported with every result names the resolved engine version.
            assert!(
                erp_report_typst::RENDERER.ends_with(&format!("typst-{}", &version[1..])),
                "{}",
                erp_report_typst::RENDERER
            );
        }
    }
    // The assets crate ships fonts behind a feature: the engine loads the shipped font package only.
    let output = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "tree",
            "--locked",
            "--offline",
            "-p",
            "erp-report-typst",
            "--edges",
            "features,normal,build",
            "--prefix",
            "none",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let features = String::from_utf8(output.stdout).unwrap();
    assert!(features.contains("typst-assets feature \"default\""));
    assert!(!features.contains("typst-assets feature \"fonts\""));
}
