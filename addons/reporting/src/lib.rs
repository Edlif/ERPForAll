// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dynamic reports (design, spec `SPEC-DYNREP`): a
//! `reporting.report` record names a [`provider::ReportProvider`]; the server resolves the report's
//! options, builds its tree of lines, unfolds a line one page at a time and answers the drill-down
//! action behind a line or a cell. Every read runs as the caller, inside the RPC call's read
//! transaction; nothing is kept between calls.
//!
//! The generic provider `reporting.definition` reads its lines, columns and expressions from data
//! ([`definition`]); a native addon registers its own with [`provider_declarations!`]. The screen
//! and the PDF and XLSX exports build on the same calls.

erp_sdk::addon!(name = "reporting", depends = ["base", "web"]);

mod client_action;
pub mod definition;
pub mod export;
mod formula;
pub mod line;
mod models;
pub mod options;
pub mod provider;
pub mod runtime;
pub mod settings;

pub use models::{
    ReportingReport, ReportingReportColumn, ReportingReportExpression, ReportingReportLine,
};

#[doc(hidden)]
pub use erp_sdk::__private::linkme as __linkme;
