// SPDX-License-Identifier: MIT OR Apache-2.0
//! A render worker around a scripted engine, for the pool tests only. The document id picks what the
//! engine does: `ok…` renders, `loop` never ends, `grow` takes memory until it is stopped, `exit…`
//! leaves the process, `exit-once:<path>` leaves it unless `<path>` exists (and creates it),
//! `flood` writes a megabyte to stderr and exits. The argument `exit-at-start` exits before `ready`.
use erp_report::{DocTree, LayoutEngine, RenderIdentity, RenderResult, ReportError};
use std::{hint::black_box, process::ExitCode, time::Duration};

struct Scripted;
impl LayoutEngine for Scripted {
    fn render(&self, document: &DocTree) -> Result<RenderResult, ReportError> {
        let id = document.options().id();
        if id == "loop" {
            loop {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if id == "grow" {
            let mut held: Vec<Vec<u8>> = Vec::new();
            loop {
                // Pages that are written count as resident memory.
                held.push(vec![1u8; 8 * 1024 * 1024]);
                black_box(&held);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if id == "flood" {
            let line = "x".repeat(1023);
            for _ in 0..1024 {
                eprintln!("{line}");
            }
            std::process::exit(1);
        }
        if let Some(marker) = id.strip_prefix("exit-once:") {
            if std::fs::metadata(marker).is_err() {
                std::fs::write(marker, b"").map_err(|e| ReportError::WorkerUnavailable {
                    cause: format!("the fixture cannot write its marker {marker}: {e}"),
                })?;
                std::process::exit(1);
            }
        } else if id.starts_with("exit") {
            std::process::exit(1);
        }
        let result = RenderResult::new(
            RenderIdentity::new(id, format!("fixture-{}", std::process::id())),
            b"%PDF-fixture".to_vec(),
            1,
            Vec::new(),
        );
        if id == "image-failure" {
            return result.with_image_failures(
                vec![erp_report::ImageFailure::new(
                    document
                        .nodes()
                        .iter()
                        .position(|node| matches!(node, erp_report::Node::Image { .. }))
                        .unwrap_or(0),
                    "fixture decoder failure".into(),
                    erp_report::Limits::default(),
                )?],
                erp_report::Limits::default(),
            );
        }
        Ok(result)
    }
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("exit-at-start") {
        eprintln!("fixture: told to exit at start");
        return ExitCode::from(1);
    }
    let status = erp_report_worker::run(std::io::stdin().lock(), std::io::stdout().lock(), |_| {
        Ok(Scripted)
    });
    ExitCode::from(status)
}
