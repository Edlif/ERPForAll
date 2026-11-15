// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
#[test]
fn stateless_processes_and_abandoned_output() {
    fn child(mode: &str) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "process_helper", "--nocapture"])
            .env("ERP_PDF_TEST_MODE", mode);
        c
    }
    let a = child("complete").output().unwrap();
    assert!(a.status.success());
    let mut doomed = child("partial")
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = std::io::BufReader::new(doomed.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line.trim() == "PARTIAL" {
            break;
        }
    }
    doomed.kill().unwrap();
    assert!(!doomed.wait().unwrap().success());
    let b = child("complete").output().unwrap();
    assert!(b.status.success());
    fn result(output: &[u8]) -> String {
        String::from_utf8(output.to_vec())
            .unwrap()
            .lines()
            .find(|l| l.starts_with("PDFHEX:"))
            .unwrap()
            .to_owned()
    }
    assert_eq!(result(&a.stdout), result(&b.stdout));
}
/// Child process of `stateless_processes_and_abandoned_output`; a no-op when run directly.
#[test]
fn process_helper() {
    let Ok(mode) = std::env::var("ERP_PDF_TEST_MODE") else {
        return;
    };
    let partial = match mode.as_str() {
        "partial" => true,
        "complete" => false,
        other => panic!("unknown test mode {other}"),
    };
    let mut out = Vec::new();
    erp_pdf::merge(
        [support::fixture(&["STATELESS"])],
        &erp_pdf::PdfLimits::default(),
        &mut |b| {
            if partial {
                println!("PARTIAL");
                std::io::stdout().flush().unwrap();
                let mut line = String::new();
                std::io::stdin().read_line(&mut line).unwrap();
                panic!("partial child must be killed before publication")
            }
            out.extend_from_slice(b);
            Ok(())
        },
    )
    .unwrap();
    support::reopen(&out, 1);
    println!(
        "PDFHEX:{}",
        out.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
}
