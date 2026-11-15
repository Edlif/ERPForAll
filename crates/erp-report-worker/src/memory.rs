// SPDX-License-Identifier: MIT OR Apache-2.0
//! Memory of a worker: its resident size as the pool reads it, and the address-space limit the worker
//! puts on itself. `RLIMIT_RSS` is not used: Linux ignores it.
use crate::wire::CapState;

/// Reads the resident size of a child by process id. The pool keeps the child unreaped while it
/// reads, so the id cannot name another process.
pub(crate) struct ResidentReader {
    #[cfg(not(target_os = "linux"))]
    system: sysinfo::System,
}
impl ResidentReader {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(not(target_os = "linux"))]
            system: sysinfo::System::new(),
        }
    }

    /// Resident bytes, or `None` when the process is gone.
    #[cfg(target_os = "linux")]
    pub(crate) fn read(&mut self, process_id: u32) -> std::io::Result<Option<u64>> {
        match statm(&format!("/proc/{process_id}/statm"), 1) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            // A process that ended between two reads has an empty or vanished entry.
            Err(error) if error.raw_os_error() == Some(3) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Resident bytes, or `None` when the process is gone.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn read(&mut self, process_id: u32) -> std::io::Result<Option<u64>> {
        let process_id = sysinfo::Pid::from_u32(process_id);
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[process_id]),
            true,
            sysinfo::ProcessRefreshKind::nothing().with_memory(),
        );
        Ok(self
            .system
            .process(process_id)
            .map(sysinfo::Process::memory))
    }
}

/// Field `field` of a `statm` file, in bytes (0 is the address-space size, 1 the resident size).
#[cfg(target_os = "linux")]
fn statm(path: &str, field: usize) -> std::io::Result<u64> {
    let text = std::fs::read_to_string(path)?;
    let pages: u64 = text
        .split_ascii_whitespace()
        .nth(field)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{path} has no numeric field {field}"),
            )
        })?;
    pages
        .checked_mul(rustix::param::page_size() as u64)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{path} field {field} overflows"),
            )
        })
}

/// The worker's own resident size, for its `diag` reply.
pub(crate) fn own_resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        statm("/proc/self/statm", 1).ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        ResidentReader::new()
            .read(std::process::id())
            .ok()
            .flatten()
    }
}

/// Limit the calling process's address space to its size now plus `cap` plus `headroom`. The size at
/// start is counted because the binary, its libraries and its thread stacks are mapped before any
/// document arrives. An allocation beyond the limit fails, and a failed allocation ends the worker.
#[cfg(target_os = "linux")]
pub(crate) fn limit_address_space(cap: u64, headroom: u64) -> CapState {
    use rustix::process::{Resource, Rlimit, setrlimit};
    let unavailable = |reason: String| CapState::Unavailable { reason };
    let size = match statm("/proc/self/statm", 0) {
        Ok(size) => size,
        Err(error) => return unavailable(format!("cannot read the address-space size: {error}")),
    };
    let Some(limit) = size
        .checked_add(cap)
        .and_then(|sum| sum.checked_add(headroom))
    else {
        return unavailable("address-space limit overflows".into());
    };
    match setrlimit(
        Resource::As,
        Rlimit {
            current: Some(limit),
            maximum: Some(limit),
        },
    ) {
        Ok(()) => CapState::Applied {
            detail: format!("RLIMIT_AS {limit} bytes"),
        },
        Err(error) => unavailable(format!("setrlimit(RLIMIT_AS) failed: {error}")),
    }
}

/// Only Linux gets the limit: the size at start is read from `/proc`, and the pool's reading of the
/// resident size is the cap everywhere.
#[cfg(not(target_os = "linux"))]
pub(crate) fn limit_address_space(_cap: u64, _headroom: u64) -> CapState {
    CapState::Unavailable {
        reason:
            "RLIMIT_AS is applied on Linux only; the pool's resident-memory reading is the cap here"
                .into(),
    }
}
