// SPDX-License-Identifier: MIT OR Apache-2.0
//! The worker side: read frames from the pool, lay documents out, answer. A worker reads three
//! environment variables and its start message; it opens no configuration and no database.
use crate::{
    PROTOCOL_VERSION,
    frame::{
        FrameError, HELLO_FRAME_BYTES, REPLY_FRAME_BYTES, decode, encode, read_frame, write_frame,
    },
    memory,
    wire::{
        CapState, DiagReport, Request, Response, WireFailure, clip_warnings, request_frame_bytes,
    },
};
use erp_report::{LayoutEngine, LimitKind, Limits, ReportError};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// Set to `1` for a worker.
pub const ENV_WORKER: &str = "ERP_REPORT_WORKER";
/// The protocol version the pool speaks.
pub const ENV_PROTOCOL: &str = "ERP_REPORT_PROTOCOL";
/// The worker's memory cap in bytes.
pub const ENV_MEMORY_CAP: &str = "ERP_REPORT_MEMORY_CAP";

/// Exit status after a shutdown message or the end of the input.
pub const EXIT_OK: u8 = 0;
/// Exit status after a frame that is unknown, malformed or over its cap.
pub const EXIT_PROTOCOL: u8 = 3;
/// Exit status when the worker could not start: environment, start message or engine.
pub const EXIT_SETUP: u8 = 4;
/// Exit status when a frame could not be read or written.
pub const EXIT_IO: u8 = 5;

/// What the start message gives an engine.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WorkerSetup {
    font_directory: PathBuf,
    limits: Limits,
}
impl WorkerSetup {
    pub fn font_directory(&self) -> &Path {
        &self.font_directory
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
}

/// A line on stderr for the operator. Never a frame's content.
fn note(text: &str) {
    eprintln!("report worker: {text}");
}

fn environment() -> Result<u64, String> {
    let variable = |name: &str| std::env::var(name).map_err(|_| format!("{name} is not set"));
    if variable(ENV_WORKER)? != "1" {
        return Err(format!("{ENV_WORKER} must be 1"));
    }
    let protocol = variable(ENV_PROTOCOL)?;
    if protocol.parse::<u32>().ok() != Some(PROTOCOL_VERSION) {
        return Err(format!(
            "{ENV_PROTOCOL} is not protocol version {PROTOCOL_VERSION}"
        ));
    }
    match variable(ENV_MEMORY_CAP)?.parse::<u64>() {
        Ok(cap) if cap > 0 => Ok(cap),
        _ => Err(format!("{ENV_MEMORY_CAP} must be a positive byte count")),
    }
}

/// Send one message; `false` when the pool no longer reads.
fn reply(output: &mut impl Write, message: &Response) -> bool {
    let sent = encode(message, REPLY_FRAME_BYTES).and_then(|body| write_frame(output, &body));
    if let Err(error) = &sent {
        note(&format!("cannot send a reply: {error}"));
    }
    sent.is_ok()
}

/// Serve the pool on `input` and `output` until a shutdown message or the end of the input, and
/// return the exit status. `build` makes the engine from the start message; it runs once.
///
/// Nothing but frames may be written to `output`.
pub fn run<E: LayoutEngine>(
    mut input: impl Read,
    mut output: impl Write,
    build: impl FnOnce(&WorkerSetup) -> Result<E, ReportError>,
) -> u8 {
    let memory_cap = match environment() {
        Ok(cap) => cap,
        Err(reason) => {
            note(&format!(
                "not started by a pool: {reason}; this command is run by the server"
            ));
            return EXIT_SETUP;
        }
    };
    let hello = match read_frame(&mut input, HELLO_FRAME_BYTES) {
        Ok(Some(body)) => match decode::<Request<'static>>(&body) {
            Ok(Request::Hello(hello)) if hello.protocol == PROTOCOL_VERSION => hello,
            Ok(_) | Err(_) => {
                note("the first frame is not a start message of this protocol version");
                return EXIT_PROTOCOL;
            }
        },
        Ok(None) => return EXIT_OK,
        Err(error) => return closing(&error),
    };
    let setup = match hello.limits() {
        Ok(limits) => WorkerSetup {
            font_directory: PathBuf::from(&hello.font_directory),
            limits,
        },
        Err(error) => {
            reply(
                &mut output,
                &Response::Failed(WireFailure::from_error(&error)),
            );
            return EXIT_SETUP;
        }
    };
    let engine = match build(&setup) {
        Ok(engine) => engine,
        Err(error) => {
            note("the layout engine could not be set up");
            reply(
                &mut output,
                &Response::Failed(WireFailure::from_error(&error)),
            );
            return EXIT_SETUP;
        }
    };
    let address_space = memory::limit_address_space(memory_cap, hello.address_space_headroom);
    let ready = Response::Ready {
        protocol: PROTOCOL_VERSION,
        address_space: address_space.clone(),
    };
    if !reply(&mut output, &ready) {
        return EXIT_IO;
    }
    let request_bytes = request_frame_bytes(setup.limits);
    loop {
        let request = match read_frame(&mut input, request_bytes) {
            Ok(Some(body)) => match decode::<Request<'static>>(&body) {
                Ok(request) => request,
                Err(error) => return closing(&error),
            },
            Ok(None) => return EXIT_OK,
            Err(error) => return closing(&error),
        };
        let sent = match request {
            Request::Render(document) => {
                let rendered = document
                    .into_tree(setup.limits)
                    .and_then(|tree| engine.render(&tree));
                match rendered {
                    Ok(result) if result.bytes().len() > setup.limits.get(LimitKind::PdfBytes) => {
                        let over = ReportError::DocumentLimit {
                            kind: LimitKind::PdfBytes,
                            actual: result.bytes().len(),
                            max: setup.limits.get(LimitKind::PdfBytes),
                        };
                        reply(
                            &mut output,
                            &Response::Failed(WireFailure::from_error(&over)),
                        )
                    }
                    Ok(result) => {
                        let head = Response::Rendered {
                            pages: result.page_count() as u64,
                            warnings: clip_warnings(result.warnings()),
                            renderer: result.identity().renderer().to_owned(),
                        };
                        reply(&mut output, &head)
                            && match write_frame(&mut output, result.bytes()) {
                                Ok(()) => true,
                                Err(error) => {
                                    note(&format!("cannot send a PDF: {error}"));
                                    false
                                }
                            }
                    }
                    Err(error) => reply(
                        &mut output,
                        &Response::Failed(WireFailure::from_error(&error)),
                    ),
                }
            }
            Request::Diag => reply(
                &mut output,
                &Response::Diag(diag(memory_cap, address_space.clone())),
            ),
            Request::Shutdown => return EXIT_OK,
            Request::Hello(_) => {
                note("a second start message");
                return EXIT_PROTOCOL;
            }
        };
        if !sent {
            return EXIT_IO;
        }
    }
}

/// The exit status for a frame that could not be read. The log line names the kind of failure and
/// sizes only: a decoder's text can quote the frame.
fn closing(error: &FrameError) -> u8 {
    match error {
        FrameError::Io(error) => {
            note(&format!("cannot read a frame: {error}"));
            EXIT_IO
        }
        FrameError::Malformed { .. } => {
            note("closing: a frame is not a known message");
            EXIT_PROTOCOL
        }
        other => {
            note(&format!("closing: {other}"));
            EXIT_PROTOCOL
        }
    }
}

fn diag(memory_cap: u64, address_space: CapState) -> DiagReport {
    let mut environment: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    environment.sort();
    let working_directory = std::env::current_dir();
    let later = |what: &str| CapState::Unavailable {
        reason: format!("{what} is not built yet (a later release)"),
    };
    DiagReport {
        protocol: PROTOCOL_VERSION,
        process_id: std::process::id(),
        environment,
        open_descriptors: open_descriptors(),
        working_directory_entries: working_directory
            .as_ref()
            .ok()
            .and_then(|path| std::fs::read_dir(path).ok())
            .map(|entries| entries.count() as u64),
        working_directory: match working_directory {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(error) => format!("unreadable: {error}"),
        },
        resident_bytes: memory::own_resident_bytes(),
        memory_cap,
        address_space,
        landlock: later("Landlock"),
        seccomp: later("seccomp"),
    }
}

/// Descriptors open in this process. Listing the directory opens one more, which is not counted.
#[cfg(target_os = "linux")]
fn open_descriptors() -> Option<u64> {
    let entries = std::fs::read_dir("/proc/self/fd").ok()?;
    Some((entries.count() as u64).saturating_sub(1))
}
#[cfg(not(target_os = "linux"))]
fn open_descriptors() -> Option<u64> {
    None
}
