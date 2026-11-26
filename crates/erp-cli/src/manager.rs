// SPDX-License-Identifier: MIT OR Apache-2.0
//! `erp manager`: the database manager pages, served by a process of their own that is off unless
//! an operator starts it (`docs/design/database-manager.md`, stage 1).
//!
//! `manager.toml` (mode 600, read like the server's secret files) says where the pages listen,
//! where the PHC hash of the master password is, and which directory holds the instances: one
//! subdirectory each, in the layout `scripts/install.sh` writes. The pages list the instances
//! with their state and start and stop their `erp server`. The manager opens no database and
//! never reads an instance's credential files.

mod http;
mod instances;
mod pages;

use crate::args::{ManagerAction, ManagerHashPasswordArgs, ManagerServeArgs};
use crate::error::{CliError, Result};
use erp_security::auth::Zeroizing;
use erp_security::auth::password::{HashParams, StoredHash};
use serde::Deserialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Where the pages listen when `bind` is absent.
const DEFAULT_BIND: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8070));
/// What a remote manager needs, for the refusals that name it.
const REMOTE_RULE: &str = "another machine needs allow_remote = true and an https public_origin";
/// The shape of `public_origin`, for its refusal.
const ORIGIN_RULE: &str = "must be http:// or https://, a host and an optional port, no more";

/// Runs an `erp manager` action.
pub(crate) fn execute(action: &ManagerAction) -> Result<()> {
    match action {
        ManagerAction::Serve(args) => serve(args),
        ManagerAction::HashPassword(args) => hash_password(args),
    }
}

fn serve(args: &ManagerServeArgs) -> Result<()> {
    // As in `erp server`: until the subscriber is installed every `tracing` event is dropped.
    erp_server::logging::install_from_env().map_err(CliError::Execution)?;
    let config = ManagerConfig::load(&args.config)?;
    let listener = match TcpListener::bind(config.bind) {
        Ok(listener) => listener,
        Err(error) => {
            let message = format!("cannot listen on {}: {error}", config.bind);
            return Err(CliError::Execution(message));
        }
    };
    println!(
        "erp manager: serving {} for the instances in {}",
        config.origin,
        config.instances_dir.display()
    );
    pages::serve(listener, config)
}

/// `erp manager hash-password`: the PHC hash of a password file, written to a new mode-600 file.
fn hash_password(args: &ManagerHashPasswordArgs) -> Result<()> {
    let password = read_private(&args.password_file, "--password-file")?;
    let params = HashParams::DEFAULT;
    let hashed = match erp_security::auth::password::hash(password.as_bytes(), &params) {
        Ok(hashed) => hashed,
        Err(error) => return Err(CliError::Execution(format!("cannot hash: {error}"))),
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = match options.open(&args.output) {
        Ok(file) => file,
        Err(error) => return Err(io_error(&args.output, error)),
    };
    if let Err(error) = writeln!(file, "{hashed}") {
        return Err(io_error(&args.output, error));
    }
    let shown = args.output.display();
    println!("wrote the master password hash to {shown}");
    Ok(())
}

/// What `manager.toml` holds, as written. A misspelt key is refused, not ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    bind: Option<SocketAddr>,
    allow_remote: Option<bool>,
    public_origin: Option<String>,
    master_password_file: PathBuf,
    instances_dir: PathBuf,
    erp_binary: Option<PathBuf>,
}

/// `manager.toml` checked, before any file it names is opened.
#[derive(Debug)]
struct Settings {
    bind: SocketAddr,
    /// The origin forms must come from: `public_origin`, else `http://<bind>`.
    origin: String,
    /// Whether the pages are reached over HTTPS (cookies get `Secure`).
    secure: bool,
    master_password_file: PathBuf,
    instances_dir: PathBuf,
    erp_binary: Option<PathBuf>,
}

impl Settings {
    /// Checks the text of `manager.toml`; relative paths are taken from `base`, its directory.
    fn parse(text: &str, base: &Path) -> Result<Self> {
        let raw: RawConfig = match toml::from_str(text) {
            Ok(raw) => raw,
            Err(error) => return Err(config_error(format!("manager.toml: {error}"))),
        };
        let bind = raw.bind.unwrap_or(DEFAULT_BIND);
        let remote = !bind.ip().is_loopback();
        if remote && raw.allow_remote != Some(true) {
            let reason = format!("bind = '{bind}' is not a loopback address; {REMOTE_RULE}");
            return Err(config_error(reason));
        }
        let (origin, secure) = match raw.public_origin {
            Some(origin) => {
                let secure = https_origin(&origin)?;
                (origin, secure)
            }
            None => (format!("http://{bind}"), false),
        };
        if remote && !secure {
            let reason = format!("bind = '{bind}' serves other machines; {REMOTE_RULE}");
            return Err(config_error(reason));
        }
        Ok(Self {
            bind,
            origin,
            secure,
            master_password_file: base.join(raw.master_password_file),
            instances_dir: base.join(raw.instances_dir),
            erp_binary: raw.erp_binary.map(|binary| base.join(binary)),
        })
    }
}

/// Whether `origin` is HTTPS. It must be `http://` or `https://`, a host and an optional port,
/// with nothing after them: the shape of the server's `public_origin`.
fn https_origin(origin: &str) -> Result<bool> {
    let (secure, authority) = if let Some(rest) = origin.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = origin.strip_prefix("http://") {
        (false, rest)
    } else {
        (false, "")
    };
    if authority.is_empty() || !authority.bytes().all(authority_byte) {
        return Err(config_error(format!("public_origin = '{origin}' {ORIGIN_RULE}")));
    }
    Ok(secure)
}

fn authority_byte(byte: u8) -> bool {
    byte.is_ascii_graphic() && !b"/?#@\\".contains(&byte)
}

/// The manager's configuration: `manager.toml` checked, the master password hash read.
#[derive(Debug)]
struct ManagerConfig {
    bind: SocketAddr,
    origin: String,
    secure: bool,
    master: StoredHash,
    instances_dir: PathBuf,
    /// The program instances run as `erp server`: `erp_binary`, else this program.
    erp_binary: PathBuf,
}

impl ManagerConfig {
    /// Reads `path` and the files it names. `manager.toml` and the master password file are
    /// read as the server reads its secret files: regular files of mode 600, not symbolic links.
    fn load(path: &Path) -> Result<Self> {
        let path = match std::path::absolute(path) {
            Ok(path) => path,
            Err(error) => return Err(io_error(path, error)),
        };
        let text = read_private(&path, "--config")?;
        let Some(base) = path.parent() else {
            return Err(config_error(format!("{} has no directory", path.display())));
        };
        let settings = Settings::parse(&text, base)?;
        let master_file = &settings.master_password_file;
        let hash = read_private(master_file, "--config")?;
        let master = match StoredHash::parse(&hash) {
            Ok(master) => master,
            Err(error) => {
                let reason = format!("master_password_file {}: {error}", master_file.display());
                return Err(config_error(reason));
            }
        };
        if !settings.instances_dir.is_dir() {
            let shown = settings.instances_dir.display();
            return Err(config_error(format!("instances_dir {shown} is not a directory")));
        }
        let erp_binary = match settings.erp_binary {
            Some(binary) => binary,
            None => match std::env::current_exe() {
                Ok(binary) => binary,
                Err(error) => return Err(CliError::Execution(format!("no erp program: {error}"))),
            },
        };
        if !erp_binary.is_file() {
            let shown = erp_binary.display();
            return Err(config_error(format!("erp_binary {shown} is not a file")));
        }
        Ok(Self {
            bind: settings.bind,
            origin: settings.origin,
            secure: settings.secure,
            master,
            instances_dir: settings.instances_dir,
            erp_binary,
        })
    }
}

/// Reads a protected file with the server's reader ([`erp_server::config::read_secret`]: a
/// regular file of mode 600, not a symbolic link, 1 to 4096 bytes of UTF-8), after saying which
/// of these it is not, since that reader's refusal does not.
fn read_private(path: &Path, arg: &str) -> Result<Zeroizing<String>> {
    check_private(path, arg)?;
    match erp_server::config::read_secret(path) {
        Ok(text) => Ok(text),
        Err(_) => {
            let shown = path.display();
            let reason = format!("{shown} must hold 1 to 4096 bytes of UTF-8 text");
            Err(invalid(arg, reason))
        }
    }
}

/// Why `path` is not a regular file of mode 600, or nothing when it is one.
fn check_private(path: &Path, arg: &str) -> Result<()> {
    let shown = path.display();
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return Err(invalid(arg, format!("cannot read {shown}: {error}"))),
    };
    if metadata.file_type().is_symlink() {
        let reason = format!("{shown} is a symbolic link; name the file itself");
        return Err(invalid(arg, reason));
    }
    if !metadata.is_file() {
        return Err(invalid(arg, format!("{shown} is not a regular file")));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        let reason = format!("{shown} has mode {mode:03o}; it must be 600 (chmod 600 {shown})");
        return Err(invalid(arg, reason));
    }
    Ok(())
}

fn invalid(arg: &str, reason: String) -> CliError {
    CliError::InvalidInput {
        arg: arg.to_owned(),
        reason,
    }
}

fn config_error(reason: String) -> CliError {
    invalid("--config", reason)
}

fn io_error(path: &Path, source: std::io::Error) -> CliError {
    CliError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
    use super::*;
    use crate::args::{Cli, Commands};
    use clap::Parser;
    use clap::error::ErrorKind;
    use erp_security::auth::password::hash;
    use std::fs::Permissions;

    fn parse(line: &str) -> ManagerAction {
        match Cli::try_parse_from(line.split(' ')).unwrap().command {
            Commands::Manager(command) => command.action,
            other => panic!("not the manager command: {other:?}"),
        }
    }

    #[test]
    fn serve_and_hash_password_take_their_files() {
        match parse("erp manager serve --config m.toml") {
            ManagerAction::Serve(args) => assert_eq!(args.config, Path::new("m.toml")),
            other => panic!("not serve: {other:?}"),
        }
        match parse("erp manager hash-password --password-file p --output o") {
            ManagerAction::HashPassword(args) => {
                assert_eq!(args.password_file, Path::new("p"));
                assert_eq!(args.output, Path::new("o"));
            }
            other => panic!("not hash-password: {other:?}"),
        }
        let missing = Cli::try_parse_from("erp manager serve".split(' ')).unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);
    }

    fn refusal(result: Result<impl std::fmt::Debug>) -> String {
        match result {
            Err(CliError::InvalidInput { reason, .. }) => reason,
            other => panic!("expected an invalid input, got {other:?}"),
        }
    }

    fn private_file(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, Permissions::from_mode(0o600)).unwrap();
    }

    const MINIMAL: &str = "master_password_file = 'master.phc'\ninstances_dir = 'instances'\n";

    #[test]
    fn the_defaults_are_loopback_port_8070_and_paths_beside_the_file() {
        let settings = Settings::parse(MINIMAL, Path::new("/etc/erp")).unwrap();
        assert_eq!(settings.bind, DEFAULT_BIND);
        assert_eq!(settings.origin, "http://127.0.0.1:8070");
        assert!(!settings.secure);
        let master = Path::new("/etc/erp/master.phc");
        assert_eq!(settings.master_password_file, master);
        assert_eq!(settings.instances_dir, Path::new("/etc/erp/instances"));
        assert_eq!(settings.erp_binary, None);
        let absolute = "master_password_file = '/m'\ninstances_dir = '/i'\nerp_binary = '/b/erp'\n";
        let settings = Settings::parse(absolute, Path::new("/etc/erp")).unwrap();
        assert_eq!(settings.instances_dir, Path::new("/i"));
        assert_eq!(settings.erp_binary.as_deref(), Some(Path::new("/b/erp")));
    }

    #[test]
    fn another_machine_needs_allow_remote_and_an_https_origin() {
        let base = Path::new("/etc/erp");
        let open = format!("bind = '0.0.0.0:8070'\n{MINIMAL}");
        assert!(refusal(Settings::parse(&open, base)).contains("not a loopback address"));
        let allowed = format!("allow_remote = true\n{open}");
        assert!(refusal(Settings::parse(&allowed, base)).contains("https public_origin"));
        let plain = format!("public_origin = 'http://erp.example'\n{allowed}");
        assert!(refusal(Settings::parse(&plain, base)).contains("https public_origin"));
        let refused = format!("allow_remote = false\n{open}");
        assert!(refusal(Settings::parse(&refused, base)).contains("not a loopback address"));
        let secure = format!("public_origin = 'https://erp.example'\n{allowed}");
        let settings = Settings::parse(&secure, base).unwrap();
        assert_eq!(settings.origin, "https://erp.example");
        assert!(settings.secure);
        let local = format!("bind = '[::1]:9000'\n{MINIMAL}");
        let local = Settings::parse(&local, base).unwrap();
        assert_eq!(local.origin, "http://[::1]:9000");
    }

    #[test]
    fn unknown_keys_and_malformed_origins_are_refused() {
        let base = Path::new("/etc/erp");
        let misspelt = format!("instance_dir = 'x'\n{MINIMAL}");
        assert!(refusal(Settings::parse(&misspelt, base)).contains("manager.toml"));
        assert!(refusal(Settings::parse("bind = '127.0.0.1:8070'\n", base)).contains("manager"));
        let origins = [
            "erp.example",
            "https://",
            "https://erp.example/",
            "http://a b",
            "http://u@h",
        ];
        for origin in origins {
            let text = format!("public_origin = '{origin}'\n{MINIMAL}");
            let reason = refusal(Settings::parse(&text, base));
            assert!(reason.contains("public_origin"), "{origin}: {reason}");
        }
    }

    /// A manager directory with `manager.toml`, the hash of `secret` and an instances directory.
    fn manager_dir(root: &Path) -> PathBuf {
        std::fs::create_dir(root.join("instances")).unwrap();
        let master = hash(b"secret", &HashParams::DEFAULT).unwrap();
        private_file(&root.join("master.phc"), &master);
        let config = root.join("manager.toml");
        private_file(&config, MINIMAL);
        config
    }

    #[test]
    fn load_reads_the_master_password_hash_and_the_instances_dir() {
        let temp = tempfile::tempdir().unwrap();
        let config = ManagerConfig::load(&manager_dir(temp.path())).unwrap();
        assert!(config.master.verify(b"secret").unwrap());
        assert!(!config.master.verify(b"Secret").unwrap());
        assert_eq!(config.instances_dir, temp.path().join("instances"));
        assert!(config.erp_binary.is_file());
    }

    #[test]
    fn load_refuses_files_that_are_not_private() {
        let temp = tempfile::tempdir().unwrap();
        let config = manager_dir(temp.path());
        std::fs::set_permissions(&config, Permissions::from_mode(0o644)).unwrap();
        assert!(refusal(ManagerConfig::load(&config)).contains("has mode 644; it must be 600"));
        std::fs::set_permissions(&config, Permissions::from_mode(0o600)).unwrap();
        let link = temp.path().join("link.toml");
        std::os::unix::fs::symlink(&config, &link).unwrap();
        assert!(refusal(ManagerConfig::load(&link)).contains("symbolic link"));
        let master = temp.path().join("master.phc");
        std::fs::set_permissions(master, Permissions::from_mode(0o640)).unwrap();
        assert!(refusal(ManagerConfig::load(&config)).contains("master.phc has mode 640"));
    }

    #[test]
    fn load_refuses_a_master_password_that_is_not_a_phc_hash() {
        let temp = tempfile::tempdir().unwrap();
        let config = manager_dir(temp.path());
        let master = temp.path().join("master.phc");
        private_file(&master, "plaintext-password\n");
        let reason = refusal(ManagerConfig::load(&config));
        assert!(reason.contains("unrecognised password hash"), "{reason}");
        assert!(!reason.contains("plaintext-password"), "{reason}");
        std::fs::remove_dir(temp.path().join("instances")).unwrap();
        private_file(&master, &hash(b"x", &HashParams::DEFAULT).unwrap());
        assert!(refusal(ManagerConfig::load(&config)).contains("is not a directory"));
    }

    #[test]
    fn hash_password_writes_a_private_verifiable_hash_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let password_file = temp.path().join("password");
        let output = temp.path().join("master.phc");
        private_file(&password_file, "correct horse\n");
        let args = ManagerHashPasswordArgs {
            password_file: password_file.clone(),
            output: output.clone(),
        };
        hash_password(&args).unwrap();
        let written = erp_server::config::read_secret(&output).unwrap();
        let stored = StoredHash::parse(&written).unwrap();
        assert!(stored.verify(b"correct horse").unwrap());
        let again = hash_password(&args).unwrap_err();
        assert!(matches!(again, CliError::Io { .. }), "{again}");
        std::fs::set_permissions(password_file, Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_file(output).unwrap();
        assert!(refusal(hash_password(&args)).contains("must be 600"));
    }
}
