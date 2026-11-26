// SPDX-License-Identifier: MIT OR Apache-2.0
//! The instances of `instances_dir`: each subdirectory that holds `private/server.toml`. It is the
//! layout `scripts/install.sh` writes (`private/server.toml`, `server.pid`, `server.log`), so a
//! local install made with `--state-dir <instances_dir>/<name>` is an instance. The state follows
//! the script's rules: the recorded process is alive and looks like `erp`, and
//! `/erp/health/ready` answers 200. Starting and stopping do what the script does.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// What an instance name may be, for the pages.
const NAME_RULE: &str = "1 to 64 letters, digits, '_', '-' or '.', not starting with '.'";
/// The largest process id `server.pid` may hold.
const PID_MAX: u32 = 0x7fff_ffff;

/// One instance directory.
#[derive(Debug)]
pub(super) struct Instance {
    /// The directory name, which names the instance on the pages and in their paths.
    pub(super) name: String,
    pub(super) dir: PathBuf,
    /// What the pages need from its `server.toml`, or why it cannot be used.
    pub(super) settings: Result<ServerSettings, String>,
}

/// What the manager reads from an instance's `server.toml`. The credential files it names are
/// never opened by the manager; the server checks them when it starts.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ServerSettings {
    pub(super) database: String,
    pub(super) bind: SocketAddr,
    pub(super) public_origin: String,
}

/// The keys of `server.toml` the manager reads; the others are the server's.
#[derive(Deserialize)]
struct RawSettings {
    database: String,
    bind: SocketAddr,
    public_origin: String,
}

/// `private/server.toml` of the instance in `dir`.
pub(super) fn config_path(dir: &Path) -> PathBuf {
    dir.join("private").join("server.toml")
}

/// The instances in `dir`, by name. A subdirectory without `private/server.toml` is not one; an
/// instance whose name or `server.toml` cannot be used is listed with the reason.
pub(super) fn discover(dir: &Path) -> Result<Vec<Instance>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => return Err(format!("cannot list {}: {error}", dir.display())),
    };
    let mut found = BTreeMap::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return Err(format!("cannot list {}: {error}", dir.display())),
        };
        let path = entry.path();
        if !config_path(&path).is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let settings = if valid_name(&name) {
            read_settings(&config_path(&path))
        } else {
            Err(format!("the directory name must be {NAME_RULE}"))
        };
        let instance = Instance {
            name: name.clone(),
            dir: path,
            settings,
        };
        found.insert(name, instance);
    }
    Ok(found.into_values().collect())
}

/// Whether `name` can name an instance in a page path: 1 to 64 ASCII letters, digits, `_`, `-`
/// and `.`, not starting with `.`.
pub(super) fn valid_name(name: &str) -> bool {
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte);
    (1..=64).contains(&name.len()) && !name.starts_with('.') && name.bytes().all(allowed)
}

/// Reads what the pages need from the `server.toml` at `path`.
fn read_settings(path: &Path) -> Result<ServerSettings, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return Err(format!("cannot open private/server.toml: {error}")),
    };
    let mut text = String::new();
    if let Err(error) = file.take(65_537).read_to_string(&mut text) {
        return Err(format!("cannot read private/server.toml: {error}"));
    }
    if text.len() > 65_536 {
        return Err("private/server.toml is larger than 64 KiB".to_owned());
    }
    parse_settings(&text)
}

/// The keys the pages need, `database` checked with the server's rule.
pub(super) fn parse_settings(text: &str) -> Result<ServerSettings, String> {
    let raw: RawSettings = match toml::from_str(text) {
        Ok(raw) => raw,
        Err(error) => return Err(format!("private/server.toml: {error}")),
    };
    let database = raw.database;
    if !(1..=63).contains(&database.len()) || !database.bytes().all(database_byte) {
        return Err(format!("database = '{database}' is not a database name"));
    }
    let origin = raw.public_origin;
    if !origin.starts_with("http://") && !origin.starts_with("https://") {
        return Err(format!("public_origin = '{origin}' is not an http:// or https:// origin"));
    }
    Ok(ServerSettings {
        database,
        bind: raw.bind,
        public_origin: origin,
    })
}

fn database_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The state the pages show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    /// The process is alive and `/erp/health/ready` answers 200.
    Running,
    /// The process is alive and not ready (yet, or any more).
    Starting,
    /// No live `erp` process is recorded.
    Stopped,
}

impl State {
    /// The word on the pages.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Starting => "starting",
            Self::Stopped => "stopped",
        }
    }
}

/// The state from whether the recorded process is alive and whether it answered ready.
pub(super) fn decide(alive: bool, ready: bool) -> State {
    match (alive, ready) {
        (true, true) => State::Running,
        (true, false) => State::Starting,
        (false, _) => State::Stopped,
    }
}

/// The state of the instance in `dir` whose server listens on `bind`.
pub(super) fn state(dir: &Path, bind: SocketAddr) -> Result<State, String> {
    let alive = match recorded_pid(dir)? {
        Some(pid) => process_alive(pid)?,
        None => false,
    };
    Ok(decide(alive, alive && probe_ready(bind)))
}

/// The pid in `server.pid` of `dir`; `None` when there is no such file.
pub(super) fn recorded_pid(dir: &Path) -> Result<Option<u32>, String> {
    let file = match File::open(dir.join("server.pid")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read server.pid: {error}")),
    };
    let mut text = String::new();
    if let Err(error) = file.take(32).read_to_string(&mut text) {
        return Err(format!("cannot read server.pid: {error}"));
    }
    parse_pid(&text).map(Some)
}

/// A pid as `server.pid` holds it: decimal digits and a line end.
pub(super) fn parse_pid(text: &str) -> Result<u32, String> {
    let digits = text.trim_end();
    let shaped = !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    match digits.parse::<u32>().ok().filter(|_| shaped) {
        Some(pid) if (2..=PID_MAX).contains(&pid) => Ok(pid),
        _ => {
            let shown = digits.escape_debug();
            Err(format!("server.pid does not hold a process id: '{shown}'"))
        }
    }
}

/// Whether `pid` is a live process that looks like `erp`, by the rule of `scripts/install.sh`:
/// `ps -p PID -o stat=` is neither empty nor a zombie, and `ps -p PID -o comm=` contains `erp`.
pub(super) fn process_alive(pid: u32) -> Result<bool, String> {
    let stat = ps(pid, "stat=")?;
    let command = ps(pid, "comm=")?;
    Ok(looks_like_erp(&stat, &command))
}

/// The install script's rule on the two answers of `ps`.
pub(super) fn looks_like_erp(stat: &str, command: &str) -> bool {
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z') && command.contains("erp")
}

/// One column of `ps -p PID`; empty when there is no such process (`ps` then exits non-zero, and
/// the install script reads that as no process too).
fn ps(pid: u32, column: &str) -> Result<String, String> {
    let pid = pid.to_string();
    let mut command = Command::new("ps");
    command.args(["-p", pid.as_str(), "-o", column]);
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => return Err(format!("cannot run ps: {error}")),
    };
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether `GET /erp/health/ready` on the server listening on `bind` answers 200 within a few
/// seconds. A server that cannot be reached is not ready.
pub(super) fn probe_ready(bind: SocketAddr) -> bool {
    let address = probe_address(bind);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_secs(1)) else {
        return false;
    };
    let timeout = Some(Duration::from_secs(2));
    if stream.set_read_timeout(timeout).is_err() || stream.set_write_timeout(timeout).is_err() {
        return false;
    }
    let head = format!("GET /erp/health/ready HTTP/1.1\r\nHost: {address}\r\n");
    let request = head + "Connection: close\r\n\r\n";
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut answer = Vec::new();
    let mut chunk = [0u8; 64];
    while answer.len() < 13 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => answer.extend_from_slice(&chunk[..read]),
        }
    }
    answered_ok(&answer)
}

/// Whether something already accepts connections on `bind`, the install script's check before it
/// starts a server there.
pub(super) fn port_in_use(bind: SocketAddr) -> bool {
    TcpStream::connect_timeout(&probe_address(bind), Duration::from_secs(1)).is_ok()
}

/// Where to reach a server bound to `bind`: that address, or loopback when it is bound to every
/// address.
pub(super) fn probe_address(bind: SocketAddr) -> SocketAddr {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, bind.port())
}

/// Whether a response starts with the status line of a 200.
pub(super) fn answered_ok(answer: &[u8]) -> bool {
    answer.starts_with(b"HTTP/1.1 200 ") || answer.starts_with(b"HTTP/1.0 200 ")
}

/// Starts `erp server --config <dir>/private/server.toml` as `scripts/install.sh` does: no input,
/// output appended to `server.log`, the pid written to `server.pid`. The process gets a process
/// group of its own, so a Ctrl-C meant for the manager does not stop the servers it started.
pub(super) fn spawn(erp: &Path, dir: &Path) -> Result<Child, String> {
    let log_path = dir.join("server.log");
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    let log = match options.open(&log_path) {
        Ok(log) => log,
        Err(error) => return Err(format!("cannot open {}: {error}", log_path.display())),
    };
    let errors = match log.try_clone() {
        Ok(errors) => errors,
        Err(error) => return Err(format!("cannot open {}: {error}", log_path.display())),
    };
    let mut command = Command::new(erp);
    command.args(["server", "--config"]);
    command.arg(config_path(dir));
    command.current_dir(dir).stdin(Stdio::null());
    command.stdout(Stdio::from(log));
    command.stderr(Stdio::from(errors));
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Err(format!("cannot run {}: {error}", erp.display())),
    };
    let pid = child.id();
    if let Err(error) = std::fs::write(dir.join("server.pid"), format!("{pid}\n")) {
        // Unrecorded, the process could not be found again: end it and say so.
        let ended = match child.kill().and_then(|()| child.wait()) {
            Ok(_) => "the process was ended".to_owned(),
            Err(kill) => format!("the process {pid} could not be ended: {kill}"),
        };
        return Err(format!("cannot write server.pid: {error}; {ended}"));
    }
    Ok(child)
}

/// Sends SIGTERM to `pid` with `kill`, as the install script stops its server.
pub(super) fn terminate(pid: u32) -> Result<(), String> {
    let pid = pid.to_string();
    let mut command = Command::new("kill");
    command.args(["-TERM", pid.as_str()]);
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("kill -TERM {pid} failed ({status})")),
        Err(error) => Err(format!("cannot run kill: {error}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
    use super::*;
    use std::net::TcpListener;

    /// The `server.toml` that `scripts/install.sh` writes, with keys the manager does not read.
    const INSTALLED: &str = "database = 'shop'
database_url_file = 'request-url'
owner_database_url_file = 'owner-url'
bind = '127.0.0.1:8071'
public_origin = 'http://127.0.0.1:8071'
proxy_mode = 'direct'
addons_path = ['/x/addons']
assets_manifest = '/x/manifest.json'

[startup_limits]
\"process.memory_soft_bytes\" = 1000000000
";

    fn refusal(instance: &Instance) -> &str {
        match &instance.settings {
            Ok(_) => panic!("{} was admitted", instance.name),
            Err(reason) => reason,
        }
    }

    fn write_instance(root: &Path, name: &str, server_toml: &str) {
        let private = root.join(name).join("private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("server.toml"), server_toml).unwrap();
    }

    #[test]
    fn server_settings_are_the_database_bind_and_origin_of_an_installed_toml() {
        let settings = parse_settings(INSTALLED).unwrap();
        let bind: SocketAddr = "127.0.0.1:8071".parse().unwrap();
        assert_eq!(settings.database, "shop");
        assert_eq!(settings.bind, bind);
        assert_eq!(settings.public_origin, "http://127.0.0.1:8071");
    }

    #[test]
    fn server_settings_refuse_a_bad_database_name_origin_or_missing_key() {
        let refused = |text: &str| parse_settings(text).unwrap_err();
        let renamed = refused(&INSTALLED.replace("'shop'", "'shop; drop'"));
        assert!(renamed.contains("not a database name"), "{renamed}");
        let origin = INSTALLED.replace("'http://127.0.0.1:8071'", "'javascript:alert(1)'");
        let origin = refused(&origin);
        assert!(origin.contains("public_origin"), "{origin}");
        let unbound = refused(&INSTALLED.replace("bind = '127.0.0.1:8071'\n", ""));
        assert!(unbound.contains("bind"), "{unbound}");
    }

    #[test]
    fn discovery_lists_the_directories_with_a_server_toml_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_instance(root, "shop", INSTALLED);
        write_instance(root, "broken", "database = \n");
        write_instance(root, ".hidden", INSTALLED);
        std::fs::create_dir_all(root.join("not-an-instance/private")).unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();
        let found = discover(root).unwrap();
        let names: Vec<&str> = found.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, [".hidden", "broken", "shop"]);
        assert!(refusal(&found[0]).contains("directory name"));
        assert!(refusal(&found[1]).contains("server.toml"));
        assert_eq!(found[2].settings.as_ref().unwrap().database, "shop");
        assert_eq!(found[2].dir, root.join("shop"));
        let missing = discover(&root.join("missing")).unwrap_err();
        assert!(missing.contains("cannot list"), "{missing}");
    }

    #[test]
    fn instance_names_are_safe_in_a_path() {
        assert!(valid_name("erpforall_local"));
        assert!(valid_name("shop-2.test"));
        assert!(!valid_name(""));
        assert!(!valid_name(".shop"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a%2fb"));
        assert!(!valid_name(&"a".repeat(65)));
    }

    #[test]
    fn the_state_is_running_starting_or_stopped() {
        assert_eq!(decide(true, true), State::Running);
        assert_eq!(decide(true, false), State::Starting);
        assert_eq!(decide(false, false), State::Stopped);
        assert_eq!(decide(false, true), State::Stopped);
        assert_eq!(State::Starting.label(), "starting");
    }

    #[test]
    fn a_process_is_alive_by_the_install_scripts_rule() {
        assert!(looks_like_erp("S\n", "erp\n"));
        assert!(looks_like_erp("Ss+", "/Users/me/erpforall/target/release/erp"));
        assert!(!looks_like_erp("Z", "erp"));
        assert!(!looks_like_erp("Z+", "erp"));
        assert!(!looks_like_erp("", ""));
        assert!(!looks_like_erp("S", "postgres"));
    }

    #[test]
    fn the_pid_file_is_read_strictly() {
        assert_eq!(parse_pid("4242\n"), Ok(4242));
        assert!(parse_pid("").is_err());
        assert!(parse_pid("0\n").is_err());
        assert!(parse_pid("1").is_err());
        assert!(parse_pid("12 34").is_err());
        assert!(parse_pid("-5").is_err());
        assert!(parse_pid("99999999999").is_err());
        let temp = tempfile::tempdir().unwrap();
        let bind: SocketAddr = "127.0.0.1:9".parse().unwrap();
        assert_eq!(recorded_pid(temp.path()), Ok(None));
        assert_eq!(state(temp.path(), bind), Ok(State::Stopped));
        std::fs::write(temp.path().join("server.pid"), "77\n").unwrap();
        assert_eq!(recorded_pid(temp.path()), Ok(Some(77)));
    }

    #[test]
    fn a_server_bound_to_every_address_is_probed_on_loopback() {
        let address = |text: &str| text.parse::<SocketAddr>().unwrap();
        let loopback_v4 = address("127.0.0.1:8069");
        assert_eq!(probe_address(address("0.0.0.0:8069")), loopback_v4);
        assert_eq!(probe_address(address("[::]:8069")), address("[::1]:8069"));
        let own = address("127.0.0.2:8069");
        assert_eq!(probe_address(own), own);
    }

    /// One connection answered with `status_line` by a listener on loopback.
    fn answering(status_line: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 512];
            let read = stream.read(&mut request).unwrap();
            assert!(read > 0);
            stream.write_all(status_line.as_bytes()).unwrap();
        });
        address
    }

    #[test]
    fn ready_means_a_200_from_the_health_route() {
        assert!(probe_ready(answering("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nready")));
        assert!(!probe_ready(answering("HTTP/1.1 503 Service Unavailable\r\n\r\n")));
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_address = closed.local_addr().unwrap();
        assert!(port_in_use(closed_address));
        drop(closed);
        assert!(!probe_ready(closed_address));
        assert!(!port_in_use(closed_address));
        assert!(answered_ok(b"HTTP/1.0 200 OK\r\n"));
        assert!(!answered_ok(b"HTTP/1.1 2000"));
    }
}
