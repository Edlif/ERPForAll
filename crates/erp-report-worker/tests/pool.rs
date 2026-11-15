// SPDX-License-Identifier: MIT OR Apache-2.0
//! The pool against the scripted worker of `fixture/main.rs` (design `reports-pdf.md` §6.1.1).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
use erp_report::{
    DocTree, DocumentBuilder, DocumentOptions, LayoutEngine, Limits, Node, PageSetup, ReportError,
    WorkerCrash,
};
use erp_report_worker::{PoolSettings, PoolStatus, Setting, WorkerCommand, WorkerPool};
use std::time::{Duration, Instant};

const FIXTURE: &str = env!("CARGO_BIN_EXE_erp-report-worker-fixture");

fn document(id: &str, text: &str) -> DocTree {
    let mut builder = DocumentBuilder::new(Limits::default());
    let body = builder.add(Node::Text(text.into())).unwrap();
    builder
        .finish(
            body,
            DocumentOptions::new(id, "fixture", PageSetup::new(595.0, 842.0, 36.0).unwrap()),
        )
        .unwrap()
}

fn settings(fonts: &tempfile::TempDir, arguments: &[&str]) -> PoolSettings {
    PoolSettings::new(
        WorkerCommand::new(FIXTURE, arguments.iter().copied()),
        fonts.path(),
    )
    .with(Setting::Workers, 1)
    .unwrap()
    .with(Setting::PollIntervalMs, 10)
    .unwrap()
}

fn pool(settings: PoolSettings) -> WorkerPool {
    WorkerPool::new(settings).unwrap()
}

/// The descriptor numbers open in process `pid` (`self` for this one), in order.
#[cfg(target_os = "linux")]
fn descriptors(pid: &str) -> Vec<u64> {
    let mut fds: Vec<u64> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .parse()
                .unwrap()
        })
        .collect();
    fds.sort_unstable();
    fds
}

/// Whether descriptor `fd` of this process is open and stays open across `exec`.
#[cfg(target_os = "linux")]
fn inherited_by_children(fd: u64) -> bool {
    const O_CLOEXEC: u32 = 0o2_000_000;
    // the listing's own descriptor is closed again by now
    let Ok(info) = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")) else {
        return false;
    };
    let flags = info
        .lines()
        .find_map(|line| line.strip_prefix("flags:"))
        .map(|flags| u32::from_str_radix(flags.trim(), 8).unwrap())
        .unwrap();
    flags & O_CLOEXEC == 0
}

/// descriptors, as the worker reports them; the closed-database-port render needs the server wiring)
#[test]
fn a_worker_sees_only_its_allowlist_three_descriptors_and_an_empty_directory() {
    let fonts = tempfile::tempdir().unwrap();
    let pool = pool(settings(&fonts, &[]));
    let diag = pool.diag().unwrap();
    let allowlist = [
        "ERP_REPORT_MEMORY_CAP",
        "ERP_REPORT_PROTOCOL",
        "ERP_REPORT_WORKER",
    ];
    for name in allowlist {
        assert!(diag.report.environment.iter().any(|seen| seen == name));
    }
    // Nothing of the test process's own environment reaches the worker. macOS CoreFoundation sets
    // `__CF_USER_TEXT_ENCODING` inside every process that links it, so that name is not a leak.
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if cfg!(target_os = "macos") && name == "__CF_USER_TEXT_ENCODING" {
            continue;
        }
        assert!(
            !diag.report.environment.iter().any(|seen| *seen == name),
            "{name} was inherited"
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(diag.report.environment, allowlist);
        // A descriptor this test process holds without close-on-exec reaches every child it
        // starts: `scripts/ci.sh` keeps the terminal on 3 and 4 (`exec 3>&1 4>&2`). Safe Rust
        // cannot close those in the child (`pre_exec` needs `unsafe`, forbidden here), so the
        // worker holds its three pipes, those, and nothing of the pool's own.
        let mut expected = vec![0, 1, 2];
        expected.extend(
            descriptors("self")
                .into_iter()
                .filter(|fd| *fd > 2 && inherited_by_children(*fd)),
        );
        let held = descriptors(&diag.report.process_id.to_string());
        assert_eq!(held, expected);
        assert_eq!(diag.report.open_descriptors, u64::try_from(held.len()).ok());
    }
    assert_eq!(
        std::path::Path::new(&diag.report.working_directory),
        pool.working_directory().canonicalize().unwrap()
    );
    assert_eq!(diag.report.working_directory_entries, Some(0));
    assert_eq!(diag.report.memory_cap, Setting::MemoryCapBytes.default());
}

#[test]
fn a_document_over_its_deadline_is_stopped_not_retried_and_the_pool_goes_on() {
    let fonts = tempfile::tempdir().unwrap();
    let pool = pool(
        settings(&fonts, &[])
            .with(Setting::DeadlineMs, 300)
            .unwrap(),
    );
    let started = Instant::now();
    let error = pool.render(&document("loop", "body")).unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(300 + 500));
    assert!(matches!(
        &error,
        ReportError::Deadline { document, deadline_ms: 300 } if document == "loop"
    ));
    assert_eq!(
        error.to_string(),
        "report loop did not finish within 300 ms; its render worker was stopped and the document is not retried"
    );
    let next = pool.render(&document("ok", "body")).unwrap();
    assert_eq!(next.retried(), 0);
    assert_eq!(next.bytes(), b"%PDF-fixture");
}

/// is)
#[test]
fn a_worker_over_its_memory_cap_is_stopped_and_the_document_is_not_retried() {
    let fonts = tempfile::tempdir().unwrap();
    let pool = pool(
        settings(&fonts, &[])
            .with(Setting::MemoryCapBytes, 64 * 1024 * 1024)
            .unwrap()
            .with(Setting::DeadlineMs, 20_000)
            .unwrap(),
    );
    let started = Instant::now();
    let error = pool.render(&document("grow", "body")).unwrap_err();
    assert!(
        matches!(
            &error,
            ReportError::WorkerCrashed { document, reason: WorkerCrash::MemoryCap, retried: 0, .. }
                if document == "grow"
        ),
        "{error:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(pool.render(&document("ok", "body")).is_ok());
}

#[test]
fn a_document_whose_worker_is_lost_is_sent_once_more_and_a_second_loss_is_final() {
    let fonts = tempfile::tempdir().unwrap();
    let pool = pool(settings(&fonts, &[]));
    let marker = fonts.path().join("exited");
    let id = format!("exit-once:{}", marker.display());
    let result = pool.render(&document(&id, "body")).unwrap();
    assert_eq!(result.retried(), 1);
    assert!(marker.exists());
    let error = pool.render(&document("exit", "body")).unwrap_err();
    assert!(
        matches!(
            &error,
            ReportError::WorkerCrashed { document, reason: WorkerCrash::Exit, retried: 1, .. }
                if document == "exit"
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "report render worker stopped under exit: process exited (retried 1)"
    );
}

/// reset after a good start are not asserted)
#[test]
fn workers_that_do_not_start_trip_the_breaker_and_documents_are_refused_at_once() {
    let fonts = tempfile::tempdir().unwrap();
    let pool = pool(settings(&fonts, &["exit-at-start"]));
    let started = Instant::now();
    let error = pool.render(&document("ok", "body")).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    let ReportError::WorkerUnavailable { cause } = &error else {
        panic!("{error:?}");
    };
    assert!(
        cause.starts_with(
            "3 worker starts failed, last: the worker stopped before it was ready: process exited"
        ),
        "{cause}"
    );
    assert!(cause.ends_with("fixture: told to exit at start"), "{cause}");
    assert!(matches!(
        pool.status(),
        PoolStatus::Degraded { attempts: 3, .. }
    ));
    let again = Instant::now();
    assert!(matches!(
        pool.render(&document("ok", "body")).unwrap_err(),
        ReportError::WorkerUnavailable { .. }
    ));
    assert!(again.elapsed() < Duration::from_millis(100));
}

#[test]
fn a_worker_is_replaced_after_its_documents_and_stays_warm_until_then() {
    let fonts = tempfile::tempdir().unwrap();
    let pool = pool(
        settings(&fonts, &[])
            .with(Setting::RecycleDocuments, 2)
            .unwrap(),
    );
    let renderer = |id: &str| {
        pool.render(&document(id, "body"))
            .unwrap()
            .identity()
            .renderer()
            .to_owned()
    };
    let first = renderer("ok-1");
    assert_eq!(renderer("ok-2"), first);
    assert_ne!(renderer("ok-3"), first);
}

#[test]
fn a_worker_log_is_capped_and_never_holds_the_document() {
    let fonts = tempfile::tempdir().unwrap();
    let pool = pool(settings(&fonts, &[]).with(Setting::LogBytes, 4096).unwrap());
    let error = pool
        .render(&document("flood", "SENTINEL-7f3a document text"))
        .unwrap_err();
    let ReportError::WorkerCrashed { log, .. } = &error else {
        panic!("{error:?}");
    };
    assert!(log.starts_with('['), "{}", &log[..log.len().min(80)]);
    assert!(log.contains("earlier bytes dropped]"));
    assert!(log.len() <= 4096 + 64);
    assert!(!log.contains("SENTINEL-7f3a"));
    assert!(!error.to_string().contains("SENTINEL-7f3a"));
}
