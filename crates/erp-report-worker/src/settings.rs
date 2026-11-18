// SPDX-License-Identifier: MIT OR Apache-2.0
//! What an operator can set on a worker pool. These are node configuration: the pool starts before
//! any database is reached, and a worker has none. Every value has a default, a floor and a bound.
use erp_report::{Limits, ReportError};
use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// One numeric setting of the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Setting {
    /// Worker processes; one document is in flight per worker.
    Workers,
    /// Time one document may take before its worker is stopped.
    DeadlineMs,
    /// Resident memory of one worker.
    MemoryCapBytes,
    /// Address space a worker may map beyond the memory cap and its size at start (Linux).
    AddressSpaceHeadroomBytes,
    /// How often the pool reads a worker's resident memory.
    PollIntervalMs,
    /// Time a worker has to answer its start message, and to exit after a shutdown message.
    StartTimeoutMs,
    /// Time a document waits for a free worker.
    AcquireTimeoutMs,
    /// Documents after which a worker is replaced.
    RecycleDocuments,
    /// PDF bytes after which a worker is replaced.
    RecycleBytes,
    /// Age after which a worker is replaced.
    RecycleAgeSeconds,
    /// Failed starts inside the window that stop further attempts for a while.
    StartFailures,
    /// The window in which failed starts are counted.
    StartFailureWindowMs,
    /// First wait before a new start attempt once starts keep failing; it doubles per failure.
    BackoffInitialMs,
    /// Longest wait between start attempts.
    BackoffMaxMs,
    /// Bytes of a worker's stderr kept per document; older bytes are dropped and counted.
    LogBytes,
}
impl Setting {
    /// Every setting, in declaration order.
    pub const ALL: [Self; 15] = [
        Self::Workers,
        Self::DeadlineMs,
        Self::MemoryCapBytes,
        Self::AddressSpaceHeadroomBytes,
        Self::PollIntervalMs,
        Self::StartTimeoutMs,
        Self::AcquireTimeoutMs,
        Self::RecycleDocuments,
        Self::RecycleBytes,
        Self::RecycleAgeSeconds,
        Self::StartFailures,
        Self::StartFailureWindowMs,
        Self::BackoffInitialMs,
        Self::BackoffMaxMs,
        Self::LogBytes,
    ];
    /// The name used in node configuration and in errors.
    pub fn name(self) -> &'static str {
        self.row().0
    }
    /// The setting of that name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|setting| setting.name() == name)
    }
    /// Smallest accepted value; never zero.
    pub fn floor(self) -> u64 {
        self.row().2
    }
    /// Largest accepted value.
    pub fn bound(self) -> u64 {
        self.row().3
    }
    /// The value in use when the operator sets none. `Workers` follows the machine: half its
    /// available parallelism, at least one, at most the bound.
    pub fn default(self) -> u64 {
        if self == Self::Workers {
            let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get() as u64);
            return (cores / 2).clamp(self.floor(), self.bound());
        }
        self.row().1
    }
    /// name, default, floor, bound
    fn row(self) -> (&'static str, u64, u64, u64) {
        match self {
            Self::Workers => ("workers", 1, 1, 64),
            Self::DeadlineMs => ("deadline_ms", 30_000, 50, 3_600_000),
            Self::MemoryCapBytes => ("memory_cap_bytes", GIB, 16 * MIB, 64 * GIB),
            Self::AddressSpaceHeadroomBytes => {
                ("address_space_headroom_bytes", 256 * MIB, MIB, 64 * GIB)
            }
            Self::PollIntervalMs => ("poll_interval_ms", 200, 10, 5_000),
            Self::StartTimeoutMs => ("start_timeout_ms", 30_000, 100, 300_000),
            Self::AcquireTimeoutMs => ("acquire_timeout_ms", 60_000, 10, 3_600_000),
            Self::RecycleDocuments => ("recycle_documents", 1_000, 1, 1_000_000),
            Self::RecycleBytes => ("recycle_bytes", 4 * GIB, MIB, 1024 * GIB),
            Self::RecycleAgeSeconds => ("recycle_age_seconds", 3_600, 1, 604_800),
            Self::StartFailures => ("start_failures", 3, 1, 100),
            Self::StartFailureWindowMs => ("start_failure_window_ms", 60_000, 100, 3_600_000),
            Self::BackoffInitialMs => ("backoff_initial_ms", 1_000, 10, 60_000),
            Self::BackoffMaxMs => ("backoff_max_ms", 30_000, 10, 3_600_000),
            Self::LogBytes => ("log_bytes", 64 * KIB, KIB, 16 * MIB),
        }
    }
    fn index(self) -> usize {
        self as usize
    }
}
impl fmt::Display for Setting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The program a worker is, and its fixed arguments. No argument carries a configured value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WorkerCommand {
    program: PathBuf,
    arguments: Vec<OsString>,
}
impl WorkerCommand {
    /// The running binary with the `report-worker` subcommand: the worker of `erp server`.
    pub fn current_exe() -> Result<Self, ReportError> {
        Ok(Self {
            program: std::env::current_exe().map_err(|e| ReportError::WorkerUnavailable {
                cause: format!("cannot find the running program to start render workers: {e}"),
            })?,
            arguments: vec!["report-worker".into()],
        })
    }
    /// Another program that speaks the frame protocol, for an install that ships the worker apart.
    pub fn new(
        program: impl Into<PathBuf>,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            arguments: arguments.into_iter().map(Into::into).collect(),
        }
    }
    pub fn program(&self) -> &Path {
        &self.program
    }
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

/// Settings of one pool. Start from the defaults and change what the node configuration names.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PoolSettings {
    command: WorkerCommand,
    font_directory: PathBuf,
    optional_fonts: Vec<(String, PathBuf)>,
    limits: Limits,
    values: [u64; 15],
    backtrace: bool,
}
impl PoolSettings {
    /// `font_directory` is the shipped font store the workers read.
    pub fn new(command: WorkerCommand, font_directory: impl Into<PathBuf>) -> Self {
        Self {
            command,
            font_directory: font_directory.into(),
            optional_fonts: Vec::new(),
            limits: Limits::default(),
            values: Setting::ALL.map(Setting::default),
            backtrace: false,
        }
    }
    /// Set one value. A value below the floor or above the bound is refused by name; zero is below
    /// every floor.
    pub fn with(mut self, setting: Setting, value: u64) -> Result<Self, ReportError> {
        if !(setting.floor()..=setting.bound()).contains(&value) {
            return Err(ReportError::InvalidSetting {
                name: setting.name(),
                value,
                min: setting.floor(),
                max: setting.bound(),
            });
        }
        self.values[setting.index()] = value;
        Ok(self)
    }
    /// The directory of an optional font package installed on this node, by the package's name
    /// (`report_fonts_cjk`). The engine of each worker checks the name and the files.
    pub fn with_optional_fonts(
        mut self,
        package: impl Into<String>,
        directory: impl Into<PathBuf>,
    ) -> Self {
        self.optional_fonts.push((package.into(), directory.into()));
        self
    }
    /// The document limits every worker of the pool enforces.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
    /// Give the workers `RUST_BACKTRACE=1`, the one optional entry of their environment.
    pub fn with_backtrace(mut self, backtrace: bool) -> Self {
        self.backtrace = backtrace;
        self
    }
    pub fn get(&self, setting: Setting) -> u64 {
        self.values[setting.index()]
    }
    pub fn command(&self) -> &WorkerCommand {
        &self.command
    }
    pub fn font_directory(&self) -> &Path {
        &self.font_directory
    }
    pub fn optional_fonts(&self) -> &[(String, PathBuf)] {
        &self.optional_fonts
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
    pub fn backtrace(&self) -> bool {
        self.backtrace
    }
    pub(crate) fn millis(&self, setting: Setting) -> Duration {
        Duration::from_millis(self.get(setting))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]
    use super::*;

    fn settings() -> PoolSettings {
        PoolSettings::new(WorkerCommand::new("worker", ["report-worker"]), "fonts")
    }

    /// refuses a value outside them by name; the node configuration that feeds them is not built)
    #[test]
    fn every_setting_refuses_values_outside_its_floor_and_bound() {
        for setting in Setting::ALL {
            assert_eq!(Setting::from_name(setting.name()), Some(setting));
            assert!(setting.floor() >= 1, "{setting}: zero is never accepted");
            assert!((setting.floor()..=setting.bound()).contains(&setting.default()));
            assert_eq!(settings().get(setting), setting.default());
            for accepted in [setting.floor(), setting.bound()] {
                assert_eq!(
                    settings().with(setting, accepted).unwrap().get(setting),
                    accepted
                );
            }
            for refused in [0, setting.floor() - 1, setting.bound() + 1] {
                let error = settings().with(setting, refused).unwrap_err();
                assert!(matches!(
                    error,
                    ReportError::InvalidSetting { name, value, min, max }
                        if name == setting.name()
                            && value == refused
                            && min == setting.floor()
                            && max == setting.bound()
                ));
                assert_eq!(
                    error.to_string(),
                    format!(
                        "report worker setting {} must be in {}..={}; got {refused}",
                        setting.name(),
                        setting.floor(),
                        setting.bound()
                    )
                );
            }
        }
        assert_eq!(Setting::from_name("unlimited"), None);
    }

    #[test]
    fn the_design_defaults_are_the_defaults() {
        assert_eq!(Setting::DeadlineMs.default(), 30_000);
        assert_eq!(Setting::MemoryCapBytes.default(), 1024 * 1024 * 1024);
        assert_eq!(Setting::PollIntervalMs.default(), 200);
        assert_eq!(Setting::StartFailures.default(), 3);
        assert_eq!(Setting::StartFailureWindowMs.default(), 60_000);
        assert_eq!(Setting::BackoffInitialMs.default(), 1_000);
        assert_eq!(Setting::BackoffMaxMs.default(), 30_000);
        assert_eq!(Setting::LogBytes.default(), 64 * 1024);
        let cores = std::thread::available_parallelism().unwrap().get() as u64;
        assert_eq!(Setting::Workers.default(), (cores / 2).clamp(1, 64));
    }
}
