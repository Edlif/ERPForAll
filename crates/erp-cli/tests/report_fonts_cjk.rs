// SPDX-License-Identifier: MIT OR Apache-2.0
//! The optional Chinese, Japanese and Korean font package on the server's print path: `erp server`
//! prints through `erp report-worker` processes, so the package reaches the engine in the workers'
//! start message, and every document crosses the worker frame.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]

use erp_report::{
    DocTree, DocumentBuilder, DocumentOptions, LayoutEngine, Limits, Node, PageSetup,
};
use erp_report_typst::OptionalFonts;
use erp_report_worker::{PoolSettings, Setting, WorkerCommand, WorkerPool};
use std::path::{Path, PathBuf};

fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn settings() -> PoolSettings {
    PoolSettings::new(
        WorkerCommand::new(env!("CARGO_BIN_EXE_erp"), ["report-worker"]),
        repo("addons/report_fonts"),
    )
    .with(Setting::Workers, 1)
    .unwrap()
}

/// One line of `text`; `family` is the only font it asks for, or the default when `None`.
fn document(family: Option<&str>, text: &str) -> DocTree {
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(Node::Text(text.into())).unwrap();
    let mut options =
        DocumentOptions::new("cjk", "CJK", PageSetup::new(400.0, 500.0, 35.0).unwrap());
    if let Some(family) = family {
        options = options.with_fonts(vec![family.into()]).unwrap();
    }
    b.finish(body, options).unwrap()
}

const EMPTY_BOXES: &str = "print as empty boxes";
const TEXT: &str = "日本語の商品";

/// document with Japanese text in the default family prints with the empty-box warning and a
/// document that names Noto Sans JP is refused naming the package)
#[test]
fn without_the_cjk_package_documents_print_and_its_families_are_refused_by_name() {
    let text = TEXT;
    let without = WorkerPool::new(settings()).unwrap();
    let fallback = without.render(&document(None, text)).unwrap();
    assert!(fallback.bytes().starts_with(b"%PDF-"));
    assert!(
        fallback.warnings().iter().any(|w| w.contains(EMPTY_BOXES)),
        "{:?}",
        fallback.warnings()
    );
    let refused = without
        .render(&document(Some("Noto Sans JP"), text))
        .unwrap_err();
    assert!(
        refused.to_string().contains(
            "is in the optional font package report_fonts_cjk, which is not installed on this node"
        ),
        "{refused}"
    );
}

/// the package, before any worker starts)
#[test]
fn absent_cjk_package_directory_is_refused_by_the_pool() {
    let absent = std::env::temp_dir().join(format!("erp-no-cjk-{}", std::process::id()));
    let error =
        WorkerPool::new(settings().with_optional_fonts(OptionalFonts::Cjk.package(), absent))
            .unwrap_err();
    assert!(
        error.to_string().contains(
            "cannot open the optional font package report_fonts_cjk for the render workers"
        ),
        "{error}"
    );
}

/// start, naming the package; an existing directory does not make it known)
#[test]
fn unknown_optional_package_is_refused_by_name() {
    let pool = WorkerPool::new(settings().with_optional_fonts(
        "report_fonts_nope",
        repo("optional-assets/report_fonts_cjk"),
    ))
    .unwrap();
    let refused = pool.render(&document(None, "ERP")).unwrap_err();
    assert!(
        refused.to_string().contains("report_fonts_nope"),
        "{refused}"
    );
}
