// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reads an extractor bundle (`scripts/report-corpus.py`) from stdin and writes the scan report as
//! JSON to stdout: analysis only, never reference template text.
//!
//! `cargo run -p erp-report --example scan_corpus -- [--gate-addons a,b,c] < bundle.json > scan.json`
//!
//! `--gate-addons` names the addons summed in the report's `gate` section. Its default is the four
//! addons's corpus gate; an addon without a root template in the corpus is refused.
use erp_report::{ReportLimits, scan_corpus};
use std::io::{Read, Write};

const GATE_ADDONS: &str = "account,sale,stock,purchase";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut gate = GATE_ADDONS.to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gate-addons" => gate = args.next().ok_or("--gate-addons needs a value")?,
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }
    let addons: Vec<&str> = gate.split(',').filter(|name| !name.is_empty()).collect();
    // One byte above the scan's input limit: an oversized input reaches that limit's error by name
    // instead of being cut short here.
    let cap = erp_views::ViewLimits::default().get(erp_views::LimitKind::CatalogueBytes);
    let mut input = String::new();
    std::io::stdin()
        .take(u64::try_from(cap)?.saturating_add(1))
        .read_to_string(&mut input)?;
    let report = scan_corpus(&input, ReportLimits::default())?.with_gate(&addons)?;
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, &report)?;
    writeln!(out)?;
    Ok(())
}
