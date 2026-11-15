// SPDX-License-Identifier: MIT OR Apache-2.0
//! `erp report-worker`: the Typst engine behind the frame protocol of `erp-report-worker`. The fonts
//! are loaded once per worker and reused for every document it lays out.
use erp_report_typst::TypstEngine;

/// Serve the pool on stdin and stdout; the exit status is the worker's.
pub(crate) fn serve() -> u8 {
    erp_report_worker::run(std::io::stdin().lock(), std::io::stdout().lock(), |setup| {
        TypstEngine::new(setup.font_directory(), setup.limits())
    })
}
