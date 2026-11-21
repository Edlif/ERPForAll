// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure, bounded PDF post-processing.
//!
//! Callers own worker isolation and publication. Foreign PDFs must run in an isolated worker;
//! byte/graph limits and caught panics do not replace operating-system memory/deadline enforcement.
//! No operation opens files or network resources. Discard ALL emitted bytes on any failure.
//! Merge/extract/split produce plain reading copies, not archival documents. Encryption,
//! signatures are outside the current profile. Page annotations follow an explicit
//! [`AnnotationPolicy`] in reading copies ([`ReadingCopy`]) and stay as they are in rotate and
//! stamp. Optional content (layers) stays as it is in rotate and stamp; a reading copy gets the
//! default configuration of its inputs rebuilt for the groups its pages use, so a layer hidden by
//! default stays hidden. Alternate configurations are not carried, and an overlay with layers is
//! refused.
//! An unsigned form stays a form in rotate and stamp; a reading copy refuses it or, under
//! [`FormPolicy::Flatten`], paints its fields into their pages.
//! Embedded-file, metadata and e-invoice edits are incremental updates: the input bytes are the
//! unchanged prefix of the output.
//! Collect a plain reading copy inside an already-isolated worker.
//!
//! ```
//! fn reading_copy(inputs: Vec<Vec<u8>>) -> Result<Vec<u8>, erp_pdf::PdfError> {
//!     let mut output = Vec::new();
//!     erp_pdf::merge(inputs, &erp_pdf::PdfLimits::default(), &mut |bytes| {
//!         output.extend_from_slice(bytes);
//!         Ok(())
//!     })?;
//!     Ok(output) // Publish only after the entire operation succeeds.
//! }
//! ```
//! The function above is an integration example, not a worker implementation.
#![deny(missing_docs)]
mod annot;
mod attach;
mod date;
mod edit;
mod einvoice;
mod error;
mod forms;
mod graph;
mod limits;
mod meta;
mod ocg;
mod options;
mod pages;
mod stamp;
mod writer;
mod xmp;
pub use attach::{
    Duplicate, EmbeddedFile, EmbeddedFileOptions, Relationship, attach, embedded_files,
};
pub use date::PdfDate;
pub use einvoice::{ConformanceLevel, EInvoiceName, EInvoiceOptions, einvoice, einvoice_level};
pub use error::{PdfError, PdfErrorKind};
pub use limits::{Limit, PdfLimits};
pub use meta::{Metadata, MetadataField, MetadataUpdate, PdfaId, metadata, update_metadata};
pub use options::{AnnotationPolicy, FormPolicy, PageSelection, Rotation, StampPlacement};
pub use pages::{ReadingCopy, ReadingCopySummary, extract, merge, rotate, split};
pub use stamp::stamp;
pub use writer::Emitter;
