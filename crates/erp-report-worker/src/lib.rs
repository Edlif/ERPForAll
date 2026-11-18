// SPDX-License-Identifier: MIT OR Apache-2.0
//! Report render workers (design and §6.1.1).
//!
//! Layout runs in processes apart from the server, so a document that loops or eats memory is
//! stopped from outside and takes nothing with it. This crate holds both ends:
//!
//! - [`WorkerPool`], a [`erp_report::LayoutEngine`] that sends each document to a worker process
//!   over a pipe. It starts workers with an environment of three variables, an empty read-only
//!   working directory and three descriptors; stops a worker at the document's deadline or when its
//!   resident memory passes the cap; sends a document again once when its worker was lost for
//!   another reason; stops starting workers for a while when starts keep failing; and replaces a
//!   worker after a number of documents, bytes or seconds.
//! - [`run`], the loop of a worker, generic over the engine. `erp report-worker` calls it with the
//!   Typst engine; this crate links no layout engine and no database.
//!
//! The frames between them are length-prefixed CBOR ([`Request`], [`Response`]). A document crosses
//! as [`WireDocument`]: every node kind, spanning table cells (protocol 6) and barcode and QR
//! graphics (`Node::Vector`, protocol 9) included. A graphic crosses as its rectangles, never as
//! markup, and the worker makes it again under the same checks.
//!
//! Not in this crate yet, each named where it is refused: the cgroup v2 memory cap,
//! Landlock and seccomp, and the `pdf-process` and
//! `validate-font` frames.
mod frame;
mod log;
mod memory;
mod pool;
mod settings;
mod wire;
mod worker;

pub use frame::{
    FrameError, HELLO_FRAME_BYTES, PROTOCOL_VERSION, REPLY_FRAME_BYTES, decode, encode, read_frame,
    write_frame,
};
pub use pool::{PoolStatus, WorkerDiag, WorkerPool};
pub use settings::{PoolSettings, Setting, WorkerCommand};
pub use wire::{CapState, DiagReport, Hello, Request, Response, WireDocument, WireFailure};
pub use worker::{
    ENV_MEMORY_CAP, ENV_PROTOCOL, ENV_WORKER, EXIT_IO, EXIT_OK, EXIT_PROTOCOL, EXIT_SETUP,
    WorkerSetup, run,
};
