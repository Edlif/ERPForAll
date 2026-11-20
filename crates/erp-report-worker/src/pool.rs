// SPDX-License-Identifier: MIT OR Apache-2.0
//! The pool side: worker processes, one document in flight per worker, and the rules of design
//! `reports-pdf.md` §6.1.1 for a worker that runs late, grows too large, dies or does not start.
use crate::{
    PROTOCOL_VERSION,
    frame::{
        FrameError, HELLO_FRAME_BYTES, REPLY_FRAME_BYTES, decode, encode, read_frame, write_frame,
    },
    log::CappedLog,
    memory::ResidentReader,
    settings::{PoolSettings, Setting},
    wire::{DiagReport, Hello, Request, Response, WireDocument, WireFailure, request_frame_bytes},
    worker::{ENV_MEMORY_CAP, ENV_PROTOCOL, ENV_WORKER, EXIT_PROTOCOL},
};
use erp_report::{
    DocTree, LayoutEngine, LimitKind, RenderIdentity, RenderResult, ReportError, WorkerCrash,
};
use std::{
    collections::VecDeque,
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// `SIGABRT`, the signal of a Rust process whose allocation failed; 6 on Linux and macOS.
const SIGABRT: i32 = 6;
/// What the Rust runtime writes to stderr before it aborts on a failed allocation.
const ALLOCATION_FAILED: &str = "memory allocation of";
/// Most characters of a worker's log quoted in the cause of a failed start.
const CAUSE_LOG_CHARS: usize = 300;

/// Every lock in this module guards counters and collections that are whole between any two
/// statements that touch them, so a lock poisoned by a panicking thread is taken over.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the pool can start workers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PoolStatus {
    /// Workers start, or none was needed yet.
    Ready,
    /// Starts kept failing. Documents are refused at once until a later start attempt succeeds;
    /// attempts follow each other with a doubling wait and are made when a document asks.
    Degraded { cause: String, attempts: u32 },
}

/// A worker's `diag` reply and what the pool captured of its stderr since its last document.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WorkerDiag {
    pub report: DiagReport,
    pub log: String,
    pub log_dropped: u64,
}

/// Why the watchdog stopped a worker.
#[derive(Debug, Clone)]
enum Stop {
    Deadline,
    Memory,
    /// The worker's memory could not be read, so its cap could not be enforced.
    Monitor(String),
}

/// What the document's thread and the worker's watchdog share.
struct Watch {
    state: Mutex<WatchState>,
    wake: Condvar,
}
struct WatchState {
    deadline: Option<Instant>,
    stop: Option<Stop>,
    done: bool,
}
impl Watch {
    fn arm(&self, deadline: Instant) {
        lock(&self.state).deadline = Some(deadline);
        self.wake.notify_all();
    }
    fn disarm(&self) {
        lock(&self.state).deadline = None;
    }
    fn stopped(&self) -> Option<Stop> {
        lock(&self.state).stop.clone()
    }
    /// End the watchdog and learn whether it stopped the worker.
    fn finish(&self) -> Option<Stop> {
        let mut state = lock(&self.state);
        state.done = true;
        self.wake.notify_all();
        state.stop.clone()
    }
}

fn kill(child: &Mutex<Child>) {
    // SIGKILL, never SIGTERM: a layout that loops does not look at signals.
    if let Err(error) = lock(child).kill() {
        tracing::warn!(%error, "a report worker could not be stopped");
    }
}

/// One thread per worker: stops it at the armed deadline and when its resident memory passes the cap.
fn watchdog(
    child: Arc<Mutex<Child>>,
    process_id: u32,
    watch: Arc<Watch>,
    cap: u64,
    poll: Duration,
) {
    let mut reader = ResidentReader::new();
    let mut next_read = Instant::now();
    loop {
        let now = Instant::now();
        if now >= next_read {
            next_read = now + poll;
            let stop = match reader.read(process_id) {
                Ok(Some(resident)) if resident > cap => Some(Stop::Memory),
                // A process that is gone has no memory to cap; its deadline is still served.
                Ok(Some(_) | None) => None,
                Err(error) => Some(Stop::Monitor(format!(
                    "the resident memory of worker {process_id} cannot be read: {error}"
                ))),
            };
            if let Some(stop) = stop {
                lock(&watch.state).stop.get_or_insert(stop);
                kill(&child);
                return;
            }
        }
        let mut state = lock(&watch.state);
        if state.done {
            return;
        }
        let now = Instant::now();
        let mut wait = next_read.saturating_duration_since(now);
        if let Some(deadline) = state.deadline {
            if now >= deadline {
                state.stop.get_or_insert(Stop::Deadline);
                drop(state);
                kill(&child);
                return;
            }
            wait = wait.min(deadline - now);
        }
        let (state, _) = watch
            .wake
            .wait_timeout(state, wait)
            .unwrap_or_else(PoisonError::into_inner);
        if state.done {
            return;
        }
    }
}

/// A frame exchange that did not complete.
enum Fault {
    /// The worker's output ended.
    Closed,
    Frame(FrameError),
}
fn other_reply() -> Fault {
    Fault::Frame(FrameError::Malformed {
        reason: "a reply of another kind".into(),
    })
}

/// How a worker ended, as far as the pool can tell.
enum Death {
    Deadline,
    Monitor(String),
    Crash {
        reason: WorkerCrash,
        detail: String,
        log: String,
    },
}
impl Death {
    /// The cause of a start that failed this way.
    fn start_cause(self, settings: &PoolSettings) -> String {
        match self {
            Self::Deadline => format!(
                "the worker did not answer its start message within {} ms",
                settings.get(Setting::StartTimeoutMs)
            ),
            Self::Monitor(cause) => cause,
            Self::Crash {
                reason,
                detail,
                log,
            } => {
                let said: String = log.trim().chars().take(CAUSE_LOG_CHARS).collect();
                if said.is_empty() {
                    format!("the worker stopped before it was ready: {reason} ({detail})")
                } else {
                    format!("the worker stopped before it was ready: {reason} ({detail}): {said}")
                }
            }
        }
    }
}

#[cfg(unix)]
fn signal(status: ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}
#[cfg(not(unix))]
fn signal(_status: ExitStatus) -> Option<i32> {
    None
}

/// What a worker answered to a document.
enum Reply {
    Rendered(RenderResult),
    Failed(WireFailure),
}

struct Worker {
    process_id: u32,
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    log: Arc<CappedLog>,
    watch: Arc<Watch>,
    watchdog: Option<JoinHandle<()>>,
    logger: Option<JoinHandle<()>>,
    started: Instant,
    documents: u64,
    bytes: u64,
    /// How long an end without a shutdown message waits for the process before it is stopped.
    patience: Duration,
    finished: bool,
}
/// The end of a worker process.
#[derive(Default)]
struct Ended {
    stop: Option<Stop>,
    status: Option<ExitStatus>,
    /// The process was still running after its patience and was stopped by the pool.
    forced: bool,
}

impl Worker {
    /// Start a process and take it through `hello` and `ready`. The error is the cause, for the
    /// operator.
    fn start(
        settings: &PoolSettings,
        directory: &Path,
        fonts: &str,
        optional_fonts: &[(String, String)],
    ) -> Result<Self, String> {
        let command = settings.command();
        let mut process = Command::new(command.program());
        // The whole environment of a worker: nothing is inherited, no PATH, no locale.
        process
            .args(command.arguments())
            .env_clear()
            .env(ENV_WORKER, "1")
            .env(ENV_PROTOCOL, PROTOCOL_VERSION.to_string())
            .env(
                ENV_MEMORY_CAP,
                settings.get(Setting::MemoryCapBytes).to_string(),
            )
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if settings.backtrace() {
            process.env("RUST_BACKTRACE", "1");
        }
        let mut child = process
            .spawn()
            .map_err(|error| format!("cannot start {}: {error}", command.program().display()))?;
        let process_id = child.id();
        let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let child = Arc::new(Mutex::new(child));
        let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
            kill(&child);
            if let Err(error) = lock(&child).wait() {
                tracing::warn!(%error, "a report worker without pipes could not be reaped");
            }
            return Err("the worker's pipes are missing".into());
        };
        let poll = settings.millis(Setting::PollIntervalMs);
        let mut worker = Self {
            process_id,
            child,
            stdin: Some(stdin),
            stdout,
            log: Arc::new(CappedLog::new(settings.get(Setting::LogBytes) as usize)),
            watch: Arc::new(Watch {
                state: Mutex::new(WatchState {
                    deadline: None,
                    stop: None,
                    done: false,
                }),
                wake: Condvar::new(),
            }),
            watchdog: None,
            logger: None,
            started: Instant::now(),
            documents: 0,
            bytes: 0,
            patience: poll,
            finished: false,
        };
        // From here a failed step drops `worker`, which stops and reaps the process.
        let thread = |name: &str| std::thread::Builder::new().name(name.into());
        let log = Arc::clone(&worker.log);
        worker.logger = Some(
            thread("erp-report-log")
                .spawn(move || log.drain(stderr))
                .map_err(|error| format!("cannot start the worker log thread: {error}"))?,
        );
        let (child, watch) = (Arc::clone(&worker.child), Arc::clone(&worker.watch));
        let cap = settings.get(Setting::MemoryCapBytes);
        worker.watchdog = Some(
            thread("erp-report-watch")
                .spawn(move || watchdog(child, process_id, watch, cap, poll))
                .map_err(|error| format!("cannot start the worker watchdog thread: {error}"))?,
        );
        let hello = Request::Hello(Hello::new(
            fonts.to_owned(),
            optional_fonts.to_vec(),
            settings.limits(),
            settings.get(Setting::AddressSpaceHeadroomBytes),
        ));
        let hello = encode(&hello, HELLO_FRAME_BYTES)
            .map_err(|error| format!("the start message cannot be sent: {error}"))?;
        let answer = worker.exchange(settings.millis(Setting::StartTimeoutMs), |worker| {
            worker.send(&hello)?;
            worker.message()
        });
        match answer {
            Ok(Response::Ready { protocol, .. }) if protocol == PROTOCOL_VERSION => Ok(worker),
            Ok(Response::Ready { protocol, .. }) => Err(format!(
                "the worker speaks protocol {protocol}, the pool {PROTOCOL_VERSION}"
            )),
            Ok(Response::Failed(failure)) => Err(format!(
                "the worker refused to start: {}",
                failure.describe()
            )),
            Ok(Response::Rendered { .. } | Response::Diag(_)) => {
                Err(worker.autopsy(other_reply()).start_cause(settings))
            }
            Err(fault) => Err(worker.autopsy(fault).start_cause(settings)),
        }
    }

    /// Run `work` under a deadline the watchdog enforces.
    fn exchange<T>(
        &mut self,
        within: Duration,
        work: impl FnOnce(&mut Self) -> Result<T, Fault>,
    ) -> Result<T, Fault> {
        self.watch.arm(Instant::now() + within);
        let result = work(self);
        self.watch.disarm();
        result
    }
    fn send(&mut self, body: &[u8]) -> Result<(), Fault> {
        let stdin = self.stdin.as_mut().ok_or(Fault::Closed)?;
        write_frame(stdin, body).map_err(Fault::Frame)
    }
    fn receive(&mut self, max: usize) -> Result<Vec<u8>, Fault> {
        match read_frame(&mut self.stdout, max) {
            Ok(Some(body)) => Ok(body),
            Ok(None) => Err(Fault::Closed),
            Err(error) => Err(Fault::Frame(error)),
        }
    }
    fn message(&mut self) -> Result<Response, Fault> {
        decode(&self.receive(REPLY_FRAME_BYTES)?).map_err(Fault::Frame)
    }

    /// Send one encoded document and read its answer.
    fn render(&mut self, id: &str, frame: &[u8], settings: &PoolSettings) -> Result<Reply, Fault> {
        self.log.reset();
        let pdf_bytes = settings.limits().get(LimitKind::PdfBytes);
        let reply = self.exchange(settings.millis(Setting::DeadlineMs), |worker| {
            worker.send(frame)?;
            match worker.message()? {
                Response::Rendered {
                    pages,
                    warnings,
                    image_failures,
                    renderer,
                } => {
                    let pages = usize::try_from(pages).map_err(|_| other_reply())?;
                    let image_failures =
                        crate::wire::image_failures_from_wire(image_failures, settings.limits())
                            .map_err(|_| other_reply())?;
                    let bytes = worker.receive(pdf_bytes)?;
                    Ok(Reply::Rendered(
                        RenderResult::new(
                            RenderIdentity::new(id, renderer),
                            bytes,
                            pages,
                            warnings,
                        )
                        .with_image_failures(image_failures, settings.limits())
                        .map_err(|_| other_reply())?,
                    ))
                }
                Response::Failed(failure) => Ok(Reply::Failed(failure)),
                Response::Ready { .. } | Response::Diag(_) => Err(other_reply()),
            }
        })?;
        if let Reply::Rendered(result) = &reply {
            self.documents += 1;
            self.bytes = self.bytes.saturating_add(result.bytes().len() as u64);
        }
        Ok(reply)
    }

    /// Whether the process runs and the watchdog has not stopped it.
    fn alive(&self) -> bool {
        self.watch.stopped().is_none() && matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// Ask a healthy worker to exit and give it `patience` to do so.
    fn shutdown(&mut self, patience: Duration) {
        if self.watch.stopped().is_none() {
            let sent = encode(&Request::Shutdown, HELLO_FRAME_BYTES)
                .map_err(Fault::Frame)
                .and_then(|body| self.send(&body));
            if sent.is_err() {
                // The worker is already gone; `finish` reaps it.
                tracing::debug!(
                    worker = self.process_id,
                    "a report worker was gone before its shutdown message"
                );
            }
        }
        self.patience = patience;
        self.finish();
    }

    /// End the threads and the process. Idempotent; also what dropping a worker does.
    fn finish(&mut self) -> Ended {
        if self.finished {
            return Ended::default();
        }
        self.finished = true;
        let stop = self.watch.finish();
        if let Some(watchdog) = self.watchdog.take()
            && watchdog.join().is_err()
        {
            tracing::warn!(
                worker = self.process_id,
                "a report worker's watchdog thread panicked"
            );
        }
        // The end of its input tells a worker that still runs to exit.
        drop(self.stdin.take());
        let mut status = None;
        let mut forced = false;
        {
            let mut child = lock(&self.child);
            let until = Instant::now() + self.patience;
            loop {
                match child.try_wait() {
                    Ok(Some(ended)) => {
                        status = Some(ended);
                        break;
                    }
                    Ok(None) if Instant::now() < until => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, worker = self.process_id, "a report worker's status cannot be read");
                        break;
                    }
                }
            }
            if status.is_none() {
                forced = true;
                if let Err(error) = child.kill() {
                    tracing::warn!(%error, worker = self.process_id, "a report worker could not be stopped");
                }
                match child.wait() {
                    Ok(ended) => status = Some(ended),
                    Err(error) => {
                        tracing::warn!(%error, worker = self.process_id, "a report worker could not be reaped");
                    }
                }
            }
        }
        // The log thread ends with the process: its pipe closes.
        if let Some(logger) = self.logger.take()
            && logger.join().is_err()
        {
            tracing::warn!(
                worker = self.process_id,
                "a report worker's log thread panicked"
            );
        }
        Ended {
            stop,
            status,
            forced,
        }
    }

    /// End a worker whose exchange failed and say how it died.
    fn autopsy(mut self, fault: Fault) -> Death {
        let ended = self.finish();
        let log = self.log.text();
        let detail = ended
            .status
            .map_or_else(|| "status unknown".to_owned(), |status| status.to_string());
        let reason = match ended.stop {
            Some(Stop::Deadline) => return Death::Deadline,
            Some(Stop::Monitor(cause)) => return Death::Monitor(cause),
            Some(Stop::Memory) => WorkerCrash::MemoryCap,
            None => match (&fault, ended.status) {
                (Fault::Frame(FrameError::TooLarge { .. } | FrameError::Malformed { .. }), _) => {
                    WorkerCrash::Protocol
                }
                // It closed its output and kept running.
                _ if ended.forced => WorkerCrash::Protocol,
                (_, Some(status)) => match signal(status) {
                    Some(SIGABRT) if log.contains(ALLOCATION_FAILED) => WorkerCrash::MemoryCap,
                    Some(_) => WorkerCrash::Signal,
                    None if status.code() == Some(i32::from(EXIT_PROTOCOL)) => {
                        WorkerCrash::Protocol
                    }
                    None => WorkerCrash::Exit,
                },
                (_, None) => WorkerCrash::Exit,
            },
        };
        Death::Crash {
            reason,
            detail,
            log,
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.finish();
    }
}

struct Breaker {
    cause: String,
    retry_at: Instant,
    backoff: Duration,
    /// One document is trying a start right now.
    probing: bool,
}
struct State {
    idle: Vec<Worker>,
    /// Workers that exist: idle, with a document, or starting.
    live: usize,
    /// Failed starts inside the window.
    failures: VecDeque<Instant>,
    /// Failed starts since the last one that succeeded.
    attempts: u32,
    breaker: Option<Breaker>,
}
impl State {
    fn unavailable(&self, cause: &str) -> ReportError {
        ReportError::WorkerUnavailable {
            cause: format!("{} worker starts failed, last: {cause}", self.attempts),
        }
    }
    /// Count a failed start. `Some` when documents are refused now; `None` asks for another start.
    fn start_failed(&mut self, cause: String, settings: &PoolSettings) -> Option<ReportError> {
        let now = Instant::now();
        self.attempts = self.attempts.saturating_add(1);
        let longest = settings.millis(Setting::BackoffMaxMs);
        if let Some(breaker) = &mut self.breaker {
            breaker.probing = false;
            breaker.backoff = breaker.backoff.saturating_mul(2).min(longest);
            breaker.retry_at = now + breaker.backoff;
            breaker.cause.clone_from(&cause);
            return Some(self.unavailable(&cause));
        }
        let window = settings.millis(Setting::StartFailureWindowMs);
        self.failures.push_back(now);
        while self
            .failures
            .front()
            .is_some_and(|at| now.duration_since(*at) > window)
        {
            self.failures.pop_front();
        }
        if self.failures.len() as u64 >= settings.get(Setting::StartFailures) {
            let backoff = settings.millis(Setting::BackoffInitialMs).min(longest);
            let error = self.unavailable(&cause);
            self.breaker = Some(Breaker {
                cause,
                retry_at: now + backoff,
                backoff,
                probing: false,
            });
            return Some(error);
        }
        None
    }
}

struct Shared {
    settings: PoolSettings,
    fonts: String,
    /// `(package, canonical directory)` of each optional font package.
    optional_fonts: Vec<(String, String)>,
    /// The workers' working directory: empty and read-only, removed with the pool.
    directory: tempfile::TempDir,
    state: Mutex<State>,
    freed: Condvar,
}
impl Shared {
    /// A worker for one document: an idle one, a new one, or a wait for one to come back.
    fn acquire(&self) -> Result<Worker, ReportError> {
        let settings = &self.settings;
        let give_up = Instant::now() + settings.millis(Setting::AcquireTimeoutMs);
        let mut state = lock(&self.state);
        loop {
            if let Some(worker) = state.idle.pop() {
                if worker.alive() {
                    return Ok(worker);
                }
                // Stopped or lost while idle: reap it outside the lock and look again.
                state.live -= 1;
                drop(state);
                drop(worker);
                state = lock(&self.state);
                continue;
            }
            if (state.live as u64) < settings.get(Setting::Workers) {
                let refused = state.breaker.as_mut().and_then(|breaker| {
                    if breaker.probing || Instant::now() < breaker.retry_at {
                        Some(breaker.cause.clone())
                    } else {
                        breaker.probing = true;
                        None
                    }
                });
                if let Some(cause) = refused {
                    return Err(state.unavailable(&cause));
                }
                state.live += 1;
                drop(state);
                let started = Worker::start(
                    settings,
                    self.directory.path(),
                    &self.fonts,
                    &self.optional_fonts,
                );
                state = lock(&self.state);
                match started {
                    Ok(worker) => {
                        state.failures.clear();
                        state.attempts = 0;
                        state.breaker = None;
                        return Ok(worker);
                    }
                    Err(cause) => {
                        state.live -= 1;
                        self.freed.notify_all();
                        tracing::warn!(cause = %cause, "a report worker did not start");
                        if let Some(error) = state.start_failed(cause, settings) {
                            return Err(error);
                        }
                        // Fewer failures than the breaker needs: start another at once.
                        continue;
                    }
                }
            }
            let now = Instant::now();
            if now >= give_up {
                return Err(ReportError::WorkerUnavailable {
                    cause: format!(
                        "all {} workers stayed busy for {} ms",
                        settings.get(Setting::Workers),
                        settings.get(Setting::AcquireTimeoutMs)
                    ),
                });
            }
            state = self
                .freed
                .wait_timeout(state, give_up - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Take a worker back, or replace it when it has served its documents, bytes or time.
    fn release(&self, mut worker: Worker) {
        let settings = &self.settings;
        let spent = worker.watch.stopped().is_some()
            || worker.documents >= settings.get(Setting::RecycleDocuments)
            || worker.bytes >= settings.get(Setting::RecycleBytes)
            || worker.started.elapsed()
                >= Duration::from_secs(settings.get(Setting::RecycleAgeSeconds));
        if spent {
            worker.shutdown(settings.millis(Setting::StartTimeoutMs));
            drop(worker);
            self.forget();
        } else {
            lock(&self.state).idle.push(worker);
            self.freed.notify_one();
        }
    }

    /// A worker is gone: its place is free for a new one.
    fn forget(&self) {
        lock(&self.state).live -= 1;
        self.freed.notify_all();
    }
}

/// A [`LayoutEngine`] that lays every document out in a worker process.
///
/// Workers start when documents need them, up to the `workers` setting. The pool is shared between
/// threads; each `render` call blocks its thread, so call it outside the async executor.
pub struct WorkerPool {
    shared: Arc<Shared>,
}
impl std::fmt::Debug for WorkerPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerPool")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}
impl WorkerPool {
    /// Check the font stores and make the workers' working directory. No worker is started yet.
    pub fn new(settings: PoolSettings) -> Result<Self, ReportError> {
        let canonical = |path: &Path, store: &str| {
            path.canonicalize()
                .map_err(|source| ReportError::Font {
                    reason: format!("cannot open the {store} for the render workers"),
                    source: Some(source),
                })?
                .into_os_string()
                .into_string()
                .map_err(|_| ReportError::Font {
                    reason: format!("the {store} path is not UTF-8"),
                    source: None,
                })
        };
        let fonts = canonical(settings.font_directory(), "font store")?;
        let optional_fonts = settings
            .optional_fonts()
            .iter()
            .map(|(package, path)| {
                let store = format!("optional font package {package}");
                Ok((package.clone(), canonical(path, &store)?))
            })
            .collect::<Result<Vec<_>, ReportError>>()?;
        // Each step is named: an I/O error without its step tells the operator nothing.
        let directory = tempfile::Builder::new()
            .prefix("erp-report-worker-")
            .tempdir()
            .map_err(|e| ReportError::WorkerUnavailable {
                cause: format!("cannot create the working directory of the render workers: {e}"),
            })?;
        read_only(directory.path()).map_err(|e| ReportError::WorkerUnavailable {
            cause: format!(
                "cannot make the working directory of the render workers read-only: {e}"
            ),
        })?;
        Ok(Self {
            shared: Arc::new(Shared {
                settings,
                fonts,
                optional_fonts,
                directory,
                state: Mutex::new(State {
                    idle: Vec::new(),
                    live: 0,
                    failures: VecDeque::new(),
                    attempts: 0,
                    breaker: None,
                }),
                freed: Condvar::new(),
            }),
        })
    }

    /// Whether workers start. A server reports this value at start-up and in its status.
    pub fn status(&self) -> PoolStatus {
        let state = lock(&self.shared.state);
        match &state.breaker {
            Some(breaker) => PoolStatus::Degraded {
                cause: breaker.cause.clone(),
                attempts: state.attempts,
            },
            None => PoolStatus::Ready,
        }
    }

    /// The empty read-only directory every worker runs in.
    pub fn working_directory(&self) -> &Path {
        self.shared.directory.path()
    }

    pub fn settings(&self) -> &PoolSettings {
        &self.shared.settings
    }

    /// Ask one worker what it sees of its own isolation, starting one if none is idle.
    pub fn diag(&self) -> Result<WorkerDiag, ReportError> {
        let settings = &self.shared.settings;
        let request = encode(&Request::Diag, HELLO_FRAME_BYTES).map_err(|_| {
            ReportError::InvalidDocument {
                reason: "the diag message cannot be written as a frame",
            }
        })?;
        let mut worker = self.shared.acquire()?;
        let answer = worker.exchange(settings.millis(Setting::StartTimeoutMs), |worker| {
            worker.send(&request)?;
            match worker.message()? {
                Response::Diag(report) => Ok(report),
                Response::Ready { .. } | Response::Rendered { .. } | Response::Failed(_) => {
                    Err(other_reply())
                }
            }
        });
        match answer {
            Ok(report) => {
                let (log, log_dropped) = worker.log.snapshot();
                self.shared.release(worker);
                Ok(WorkerDiag {
                    report,
                    log,
                    log_dropped,
                })
            }
            Err(fault) => {
                let death = worker.autopsy(fault);
                self.shared.forget();
                Err(ReportError::WorkerUnavailable {
                    cause: match death {
                        Death::Deadline => format!(
                            "a worker did not answer a diag message within {} ms",
                            settings.get(Setting::StartTimeoutMs)
                        ),
                        Death::Monitor(cause) => cause,
                        Death::Crash { reason, detail, .. } => {
                            format!("a worker stopped under a diag message: {reason} ({detail})")
                        }
                    },
                })
            }
        }
    }
}

#[cfg(unix)]
fn read_only(directory: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o555))
}
#[cfg(not(unix))]
fn read_only(directory: &Path) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(directory)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(directory, permissions)
}

impl LayoutEngine for WorkerPool {
    /// Lay `document` out in a worker.
    ///
    /// - A document over its deadline is `Deadline`; one whose worker passed the memory cap is
    ///   `WorkerCrashed { reason: MemoryCap }`. Neither is sent again.
    /// - A document whose worker was lost for another reason is sent once more to another worker;
    ///   the result then says `retried() == 1`, and a second loss is `WorkerCrashed` with
    ///   `retried: 1`.
    /// - A document the worker refuses comes back as the error an in-process engine returns.
    fn render(&self, document: &DocTree) -> Result<RenderResult, ReportError> {
        let shared = &self.shared;
        let settings = &shared.settings;
        // The admission an in-process engine applies, before any process is involved.
        document.check_limits(settings.limits())?;
        let id = document.options().id();
        let cap = request_frame_bytes(settings.limits());
        let frame =
            encode(&Request::Render(WireDocument::from_tree(document)?), cap).map_err(|error| {
                match error {
                    FrameError::TooLarge { actual, max } => ReportError::DocumentLimit {
                        kind: LimitKind::Bytes,
                        actual,
                        max,
                    },
                    _ => ReportError::InvalidDocument {
                        reason: "the document cannot be written as a frame",
                    },
                }
            })?;
        let mut retried = 0u8;
        loop {
            let mut worker = shared.acquire()?;
            let fault = match worker.render(id, &frame, settings) {
                Ok(Reply::Rendered(result)) => {
                    shared.release(worker);
                    return result
                        .with_retried(retried)
                        .with_document_warnings(document);
                }
                Ok(Reply::Failed(failure)) => {
                    shared.release(worker);
                    return Err(failure.into_error(id));
                }
                Err(fault) => fault,
            };
            let process_id = worker.process_id;
            let death = worker.autopsy(fault);
            shared.forget();
            match death {
                Death::Deadline => {
                    tracing::warn!(
                        document = id,
                        worker = process_id,
                        "a report worker was stopped at the document's deadline"
                    );
                    return Err(ReportError::Deadline {
                        document: id.into(),
                        deadline_ms: settings.get(Setting::DeadlineMs),
                    });
                }
                Death::Monitor(cause) => return Err(ReportError::WorkerUnavailable { cause }),
                Death::Crash {
                    reason,
                    detail,
                    log,
                } => {
                    tracing::warn!(document = id, worker = process_id, %reason, %detail, retried, "a report worker stopped under a document");
                    if reason != WorkerCrash::MemoryCap && retried == 0 {
                        retried = 1;
                        continue;
                    }
                    return Err(ReportError::WorkerCrashed {
                        document: id.into(),
                        reason,
                        retried,
                        log,
                    });
                }
            }
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // Idle workers read the end of their input and exit; one that does not is stopped.
        lock(&self.state).idle.clear();
    }
}
