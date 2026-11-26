// SPDX-License-Identifier: MIT OR Apache-2.0
//! The manager pages and the state behind them: sessions, CSRF tokens, the sign-in rate limit,
//! and the `erp server` processes this manager started.
//!
//! - `GET /`: the running instances, each linking to its own `/erp/login` (no sign-in).
//! - `GET /manager/login` and `POST /manager/login`: the master password form.
//! - `GET /manager/`: every instance with its database, port and state, and Start and Stop.
//! - `POST /manager/instances/<name>/start`, `.../stop` and `POST /manager/logout`.
//!
//! Every POST passes the server's same-origin rule for forms and carries a CSRF token: on the
//! sign-in form a token that must equal the sign-in cookie, after it the session's token.
//! Cookies are `HttpOnly` and `SameSite=Strict` and scoped to `/manager`, because a browser
//! sends the cookies of `127.0.0.1` to every port, the instances' included.

use super::ManagerConfig;
use super::http;
use super::http::{Fields, HttpError, Method, Request, Response};
use super::instances;
use super::instances::{Instance, ServerSettings, State};
use erp_security::auth::SessionSecret;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::process::Child;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a session lasts after the sign-in.
const SESSION_LIFETIME: Duration = Duration::from_secs(8 * 60 * 60);
/// The most sessions kept; a sign-in beyond it ends the oldest.
const SESSION_LIMIT: usize = 32;
/// Failed sign-ins from one address within [`FAILURE_WINDOW`] before it must wait.
const FAILURE_LIMIT: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
/// The most client addresses whose failures are kept.
const CLIENT_LIMIT: usize = 1024;
/// How long the sign-in form stays valid, in seconds.
const LOGIN_FORM_AGE: u64 = 600;
/// How long a stop waits for the server to end, as `scripts/install.sh` waits.
const STOP_WAIT: Duration = Duration::from_secs(60);
/// The most connections served at once; one more is closed unanswered.
const CONNECTION_LIMIT: usize = 16;
/// How long a connection may take to send its request or receive the answer.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_COOKIE: &str = "erp_manager_session";
const SESSION_PATH: &str = "/manager";
const LOGIN_COOKIE: &str = "erp_manager_login";
const LOGIN_PATH: &str = "/manager/login";
const LIST_PATH: &str = "/manager/";

/// Serves the pages on `listener` until the process ends, one thread per connection.
pub(super) fn serve(listener: TcpListener, config: ManagerConfig) -> crate::error::Result<()> {
    let manager = Arc::new(Manager::new(config));
    let active = Arc::new(AtomicUsize::new(0));
    for connection in listener.incoming() {
        let stream = match connection {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(target: "erp::manager", error = %error, "accept failed");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let Some(slot) = Slot::take(&active) else {
            tracing::warn!(target: "erp::manager", "too many connections: one was closed");
            continue;
        };
        let manager = Arc::clone(&manager);
        let thread = std::thread::Builder::new().name("erp-manager".to_owned());
        if let Err(error) = thread.spawn(move || connection_thread(&manager, stream, slot)) {
            tracing::warn!(target: "erp::manager", error = %error, "no connection thread");
        }
    }
    Ok(())
}

/// A place among the connections served at once, given back when dropped.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(active: &Arc<AtomicUsize>) -> Option<Self> {
        if active.fetch_add(1, Ordering::AcqRel) >= CONNECTION_LIMIT {
            active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(Arc::clone(active)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn connection_thread(manager: &Manager, mut stream: TcpStream, _slot: Slot) {
    let peer = match stream.peer_addr() {
        Ok(peer) => peer.ip(),
        Err(error) => {
            tracing::debug!(target: "erp::manager", error = %error, "no peer address");
            return;
        }
    };
    if let Err(error) = set_timeouts(&stream) {
        tracing::debug!(target: "erp::manager", error = %error, "no socket timeout");
        return;
    }
    let response = match http::read_request(&mut stream) {
        Ok(request) => manager.respond(&request, peer),
        Err(error) => error_page(&error),
    };
    if let Err(error) = response.write_to(&mut stream) {
        tracing::debug!(target: "erp::manager", error = %error, "response not delivered");
    }
}

fn set_timeouts(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))
}

/// What the pages share between connections.
struct Manager {
    config: ManagerConfig,
    sessions: Mutex<Sessions>,
    failures: Mutex<Failures>,
    /// The servers this manager started, kept to collect them when they end.
    children: Mutex<HashMap<String, Child>>,
    /// The instances a start or stop is running for.
    busy: Mutex<HashSet<String>>,
}

/// What a POST does to an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Start,
    Stop,
}

impl Manager {
    fn new(config: ManagerConfig) -> Self {
        Self {
            config,
            sessions: Mutex::new(Sessions::default()),
            failures: Mutex::new(Failures::default()),
            children: Mutex::new(HashMap::new()),
            busy: Mutex::new(HashSet::new()),
        }
    }

    /// The answer to one request; a refusal becomes an error page.
    fn respond(&self, request: &Request, peer: IpAddr) -> Response {
        match self.route(request, peer) {
            Ok(response) => response,
            Err(error) => error_page(&error),
        }
    }

    fn route(&self, request: &Request, peer: IpAddr) -> Result<Response, HttpError> {
        match (request.method, request.path.as_str()) {
            (Method::Get, "/") => self.selector(),
            (Method::Get, "/manager") => Ok(Response::redirect(LIST_PATH)),
            (Method::Get, LIST_PATH) => self.list(request),
            (Method::Get, LOGIN_PATH) => self.login_form(request, None),
            (Method::Post, LOGIN_PATH) => self.login(request, peer),
            (Method::Post, "/manager/logout") => self.logout(request),
            (Method::Post, path) => match instance_action(path) {
                Some((name, action)) => self.act(request, name, action),
                None => Err(not_found()),
            },
            (Method::Get, _) => Err(not_found()),
        }
    }

    /// `GET /`: the running instances, each linking to its own sign-in page. An instance whose
    /// state cannot be read is left out here; the list page shows why.
    fn selector(&self) -> Result<Response, HttpError> {
        self.reap()?;
        let mut items = String::new();
        for instance in self.discover()? {
            let Ok(settings) = &instance.settings else {
                continue;
            };
            if instances::state(&instance.dir, settings.bind) != Ok(State::Running) {
                continue;
            }
            let link = http::escape(&format!("{}/erp/login", settings.public_origin));
            let name = http::escape(&instance.name);
            let database = http::escape(&settings.database);
            items.push_str(&format!("<li><a href=\"{link}\">{name}</a> ({database})</li>\n"));
        }
        let mut body = String::from("<h1>Databases</h1>\n");
        if items.is_empty() {
            body.push_str("<p>No instance is running.</p>\n");
        } else {
            body.push_str(&format!("<ul>\n{items}</ul>\n"));
        }
        body.push_str(&format!("<p><a href=\"{LIST_PATH}\">Manage the instances</a></p>\n"));
        Ok(Response::html(200, page("Databases", &body)))
    }

    /// `GET /manager/`: every instance with its state and actions, after the sign-in.
    fn list(&self, request: &Request) -> Result<Response, HttpError> {
        let Some(token) = self.session_token(request)? else {
            return Ok(Response::redirect(LOGIN_PATH));
        };
        self.reap()?;
        let mut rows = String::new();
        for instance in self.discover()? {
            rows.push_str(&row(&instance, &token));
        }
        if rows.is_empty() {
            let dir = self.config.instances_dir.display().to_string();
            let dir = http::escape(&dir);
            rows = format!("<tr><td colspan=\"5\">No instance in {dir}.</td></tr>\n");
        }
        let mut body = String::from("<h1>Instances</h1>\n<table border=\"1\" cellpadding=\"4\">\n");
        body.push_str("<tr><th>Name</th><th>Database</th><th>Port</th><th>State</th>");
        body.push_str("<th>Actions</th></tr>\n");
        body.push_str(&rows);
        body.push_str("</table>\n<p><a href=\"/\">Database selector</a></p>\n");
        body.push_str("<form method=\"post\" action=\"/manager/logout\">\n");
        body.push_str(&hidden_token(&token));
        body.push_str("<button type=\"submit\">Sign out</button>\n</form>\n");
        Ok(Response::html(200, page("Instances", &body)))
    }

    /// `GET /manager/login`: the master password form, with a new sign-in token in the form and
    /// in a cookie that only the form's POST receives.
    fn login_form(&self, request: &Request, error: Option<&str>) -> Result<Response, HttpError> {
        if self.session_token(request)?.is_some() {
            return Ok(Response::redirect(LIST_PATH));
        }
        let token = new_token()?;
        let mut body = String::from("<h1>Database manager</h1>\n");
        if let Some(error) = error {
            body.push_str(&format!("<p role=\"alert\">{}</p>\n", http::escape(error)));
        }
        body.push_str(&format!("<form method=\"post\" action=\"{LOGIN_PATH}\">\n"));
        body.push_str(&hidden_token(&token));
        body.push_str("<label>Master password <input type=\"password\" name=\"password\" ");
        body.push_str("autocomplete=\"current-password\" required autofocus></label>\n");
        body.push_str("<button type=\"submit\">Sign in</button>\n</form>\n");
        let login = cookie(LOGIN_COOKIE, &token, LOGIN_PATH, Some(LOGIN_FORM_AGE));
        let response = Response::html(200, page("Sign in", &body));
        Ok(response.with_header("Set-Cookie", self.secured(login)))
    }

    /// `POST /manager/login`: the origin, the sign-in token, the rate limit, then the password;
    /// a session when it is right.
    fn login(&self, request: &Request, peer: IpAddr) -> Result<Response, HttpError> {
        self.check_origin(request)?;
        let fields = http::form(request, &["csrf_token", "password"])?;
        let presented = request_cookie(request, LOGIN_COOKIE)?;
        if !login_token_matches(presented.as_deref(), required(&fields, "csrf_token")?) {
            return Err(bad("the sign-in form expired; open it again"));
        }
        let now = Instant::now();
        if let Some(wait) = lock(&self.failures)?.wait(peer, now) {
            return Ok(too_many(wait));
        }
        let password = required(&fields, "password")?;
        let verified = match self.config.master.verify(password.as_bytes()) {
            Ok(verified) => verified,
            Err(error) => {
                tracing::error!(target: "erp::manager", error = %error, "no password check");
                return Err(server_error("the master password could not be checked".to_owned()));
            }
        };
        if !verified {
            lock(&self.failures)?.record(peer, now);
            tracing::warn!(target: "erp::manager", client = %peer, "wrong master password");
            return self.login_form(request, Some("Wrong master password."));
        }
        lock(&self.failures)?.clear(peer);
        let session = lock(&self.sessions)?.open(now)?;
        let opened = cookie(SESSION_COOKIE, &session, SESSION_PATH, None);
        let used = cookie(LOGIN_COOKIE, "", LOGIN_PATH, Some(0));
        let response = Response::redirect(LIST_PATH);
        let response = response.with_header("Set-Cookie", self.secured(opened));
        Ok(response.with_header("Set-Cookie", self.secured(used)))
    }

    /// `POST /manager/logout`: ends the session.
    fn logout(&self, request: &Request) -> Result<Response, HttpError> {
        self.authorize(request)?;
        if let Some(session) = request_cookie(request, SESSION_COOKIE)? {
            lock(&self.sessions)?.close(&session);
        }
        let ended = cookie(SESSION_COOKIE, "", SESSION_PATH, Some(0));
        let response = Response::redirect(LOGIN_PATH);
        Ok(response.with_header("Set-Cookie", self.secured(ended)))
    }

    /// `POST /manager/instances/<name>/start` and `.../stop`.
    fn act(&self, request: &Request, name: &str, action: Action) -> Result<Response, HttpError> {
        self.authorize(request)?;
        let instance = self.instance(name)?;
        let settings = match &instance.settings {
            Ok(settings) => settings,
            Err(reason) => return Err(conflict(format!("{name}: {reason}"))),
        };
        let _busy = Busy::claim(&self.busy, name)?;
        match action {
            Action::Start => self.start(&instance, settings)?,
            Action::Stop => self.stop(&instance)?,
        }
        Ok(Response::redirect(LIST_PATH))
    }

    /// Starts the instance's server unless it runs already.
    fn start(&self, instance: &Instance, settings: &ServerSettings) -> Result<(), HttpError> {
        self.reap()?;
        let state = instances::state(&instance.dir, settings.bind).map_err(conflict)?;
        if state != State::Stopped {
            return Err(conflict(format!("{} is {}", instance.name, state.label())));
        }
        if instances::port_in_use(settings.bind) {
            let port = settings.bind.port();
            return Err(conflict(format!("port {port} is in use by another program")));
        }
        let mut children = lock(&self.children)?;
        let erp = &self.config.erp_binary;
        let child = instances::spawn(erp, &instance.dir).map_err(server_error)?;
        let pid = child.id();
        tracing::info!(target: "erp::manager", instance = %instance.name, pid, "started");
        children.insert(instance.name.clone(), child);
        Ok(())
    }

    /// Sends SIGTERM to the instance's recorded server and waits up to 60 s for it to end.
    fn stop(&self, instance: &Instance) -> Result<(), HttpError> {
        let name = &instance.name;
        let Some(pid) = instances::recorded_pid(&instance.dir).map_err(conflict)? else {
            return Err(conflict(format!("{name} is not running")));
        };
        if !instances::process_alive(pid).map_err(conflict)? {
            return Err(conflict(format!("{name} is not running")));
        }
        instances::terminate(pid).map_err(server_error)?;
        tracing::info!(target: "erp::manager", instance = %name, pid, "stopping");
        let deadline = Instant::now() + STOP_WAIT;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            self.reap()?;
            if !instances::process_alive(pid).map_err(conflict)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let log = instance.dir.join("server.log").display().to_string();
                let message = format!("{name} (pid {pid}) did not stop within 60 s; see {log}");
                return Err(server_error(message));
            }
        }
    }

    /// Collects the servers this manager started that have ended, so none is left a zombie.
    fn reap(&self) -> Result<(), HttpError> {
        let mut children = lock(&self.children)?;
        children.retain(|name, child| match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                tracing::info!(target: "erp::manager", instance = %name, %status, "ended");
                false
            }
            Err(error) => {
                tracing::warn!(target: "erp::manager", instance = %name, %error, "no status");
                true
            }
        });
        Ok(())
    }

    fn discover(&self) -> Result<Vec<Instance>, HttpError> {
        instances::discover(&self.config.instances_dir).map_err(server_error)
    }

    fn instance(&self, name: &str) -> Result<Instance, HttpError> {
        for instance in self.discover()? {
            if instance.name == name {
                return Ok(instance);
            }
        }
        Err(HttpError::new(404, format!("no instance is named {name}")))
    }

    /// The server's same-origin rule for forms, against the manager's origin.
    fn check_origin(&self, request: &Request) -> Result<(), HttpError> {
        let site = request.header("sec-fetch-site")?;
        let origin = request.header("origin")?;
        let expected = &self.config.origin;
        if http::same_origin_form(site, origin, expected) {
            return Ok(());
        }
        let message = format!("cross-origin form refused: open the manager at {expected}");
        Err(HttpError::new(403, message))
    }

    /// The CSRF token of the request's live session, or `None` without one.
    fn session_token(&self, request: &Request) -> Result<Option<String>, HttpError> {
        let Some(session) = request_cookie(request, SESSION_COOKIE)? else {
            return Ok(None);
        };
        Ok(lock(&self.sessions)?.token(&session, Instant::now()))
    }

    /// The checks of a POST after the sign-in: the origin, a live session and its token.
    fn authorize(&self, request: &Request) -> Result<(), HttpError> {
        self.check_origin(request)?;
        let fields = http::form(request, &["csrf_token"])?;
        let Some(expected) = self.session_token(request)? else {
            return Err(HttpError::new(403, "the session ended; sign in again"));
        };
        if !token_matches(required(&fields, "csrf_token")?, &expected) {
            return Err(bad("Session expired (invalid CSRF token)"));
        }
        Ok(())
    }

    /// `cookie` with `Secure` when the manager is reached over HTTPS.
    fn secured(&self, cookie: String) -> String {
        if self.config.secure {
            cookie + "; Secure"
        } else {
            cookie
        }
    }
}

/// One row of the instance list.
fn row(instance: &Instance, token: &str) -> String {
    let name = http::escape(&instance.name);
    let settings = match &instance.settings {
        Ok(settings) => settings,
        Err(reason) => {
            let reason = http::escape(reason);
            let cell = format!("<td colspan=\"4\">cannot be used: {reason}</td>");
            return format!("<tr><td>{name}</td>{cell}</tr>\n");
        }
    };
    let database = http::escape(&settings.database);
    let port = settings.bind.port();
    let mut html = format!("<tr><td>{name}</td><td>{database}</td><td>{port}</td>");
    let state = match instances::state(&instance.dir, settings.bind) {
        Ok(state) => state,
        Err(reason) => {
            let reason = http::escape(&reason);
            html.push_str(&format!("<td colspan=\"2\">state unknown: {reason}</td></tr>\n"));
            return html;
        }
    };
    let mut actions = String::new();
    if state == State::Stopped {
        actions.push_str(&button(&instance.name, "start", "Start", token));
    } else {
        let link = http::escape(&format!("{}/erp/login", settings.public_origin));
        actions.push_str(&format!("<a href=\"{link}\">Open</a>\n"));
        actions.push_str(&button(&instance.name, "stop", "Stop", token));
    }
    let label = state.label();
    html.push_str(&format!("<td>{label}</td><td>{actions}</td></tr>\n"));
    html
}

/// A one-button form posting `action` for the instance `name`.
fn button(name: &str, action: &str, label: &str, token: &str) -> String {
    let target = format!("/manager/instances/{}/{action}", http::escape(name));
    let mut html = format!("<form method=\"post\" action=\"{target}\">");
    html.push_str(&hidden_token(token));
    html.push_str(&format!("<button type=\"submit\">{label}</button></form>"));
    html
}

/// The hidden field that carries a CSRF token.
fn hidden_token(token: &str) -> String {
    let token = http::escape(token);
    format!("<input type=\"hidden\" name=\"csrf_token\" value=\"{token}\">\n")
}

/// A whole page around `body`.
fn page(title: &str, body: &str) -> String {
    let title = http::escape(title);
    let mut html = String::from("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str(&format!("<title>{title} - erp manager</title>\n</head>\n<body>\n"));
    html.push_str(body);
    html.push_str("</body>\n</html>\n");
    html
}

/// The page of a refusal.
fn error_page(error: &HttpError) -> Response {
    let message = http::escape(&error.message);
    let mut body = format!("<h1>{}</h1>\n<p>{message}</p>\n", error.status);
    body.push_str(&format!("<p><a href=\"{LIST_PATH}\">Back to the manager</a></p>\n"));
    Response::html(error.status, page("Refused", &body))
}

/// The answer to a sign-in from an address that failed too often.
fn too_many(wait: Duration) -> Response {
    let seconds = wait.as_secs() + 1;
    let mut body = String::from("<h1>Too many failed sign-ins</h1>\n");
    body.push_str(&format!("<p>Try again in {seconds} seconds.</p>\n"));
    let response = Response::html(429, page("Too many sign-ins", &body));
    response.with_header("Retry-After", seconds.to_string())
}

/// The instance and action a `POST /manager/instances/<name>/<action>` path names.
fn instance_action(path: &str) -> Option<(&str, Action)> {
    let rest = path.strip_prefix("/manager/instances/")?;
    let (name, action) = rest.split_once('/')?;
    let action = match action {
        "start" => Action::Start,
        "stop" => Action::Stop,
        _ => return None,
    };
    instances::valid_name(name).then_some((name, action))
}

/// A `Set-Cookie` value: `HttpOnly` and `SameSite=Strict` always, `Max-Age` when given.
fn cookie(name: &str, value: &str, path: &str, max_age: Option<u64>) -> String {
    let mut text = format!("{name}={value}; Path={path}; HttpOnly; SameSite=Strict");
    if let Some(age) = max_age {
        text.push_str(&format!("; Max-Age={age}"));
    }
    text
}

/// The value of the cookie `name`, refusing a repeated or malformed one (the server's rule).
fn request_cookie(request: &Request, name: &str) -> Result<Option<String>, HttpError> {
    let headers: Vec<&str> = request.header("cookie")?.into_iter().collect();
    match erp_server::boundary::cookie(&headers, name) {
        Ok(value) => Ok(value.map(str::to_owned)),
        Err(_) => Err(bad("a manager cookie is repeated or malformed")),
    }
}

fn required<'a>(fields: &'a Fields, key: &str) -> Result<&'a str, HttpError> {
    match fields.get(key) {
        Some(value) => Ok(value.as_str()),
        None => Err(bad(format!("the form has no {key} field"))),
    }
}

/// 32 bytes from the OS random number generator, made as the server makes session cookies.
fn new_secret() -> Result<SessionSecret, HttpError> {
    SessionSecret::generate().map_err(|error| {
        tracing::error!(target: "erp::manager", error = %error, "no random bytes");
        server_error("the manager could not make a random token".to_owned())
    })
}

/// A new random token in cookie form (43 base64url characters).
fn new_token() -> Result<String, HttpError> {
    Ok(new_secret()?.to_cookie().as_str().to_owned())
}

/// The digest of a token in cookie form; `None` for anything else.
fn token_digest(text: &str) -> Option<[u8; 32]> {
    SessionSecret::from_cookie(text).map(|secret| secret.digest())
}

/// Whether `presented` is the token `expected`. Both are digested before they are compared, so
/// the time of the comparison says nothing about the expected token.
fn token_matches(presented: &str, expected: &str) -> bool {
    match (token_digest(presented), token_digest(expected)) {
        (Some(presented), Some(expected)) => presented == expected,
        _ => false,
    }
}

/// Whether the sign-in form's token equals its cookie (absent: the form was not opened here).
fn login_token_matches(cookie: Option<&str>, form: &str) -> bool {
    cookie.is_some_and(|cookie| token_matches(form, cookie))
}

/// The sessions of signed-in browsers, by the SHA-256 of their cookie; the cookie itself is not
/// kept.
#[derive(Default)]
struct Sessions {
    live: HashMap<[u8; 32], Session>,
}

struct Session {
    /// The CSRF token of the session's forms.
    token: String,
    expires: Instant,
}

impl Sessions {
    /// Opens a session; the answer is its cookie value.
    fn open(&mut self, now: Instant) -> Result<String, HttpError> {
        self.live.retain(|_, session| session.expires > now);
        if self.live.len() >= SESSION_LIMIT {
            let oldest = self.live.iter().min_by_key(|(_, s)| s.expires);
            if let Some(key) = oldest.map(|(key, _)| *key) {
                self.live.remove(&key);
            }
        }
        let secret = new_secret()?;
        let session = Session {
            token: new_token()?,
            expires: now + SESSION_LIFETIME,
        };
        self.live.insert(secret.digest(), session);
        Ok(secret.to_cookie().as_str().to_owned())
    }

    /// The CSRF token of the live session whose cookie is `cookie`.
    fn token(&self, cookie: &str, now: Instant) -> Option<String> {
        let session = self.live.get(&token_digest(cookie)?)?;
        (session.expires > now).then(|| session.token.clone())
    }

    /// Ends the session whose cookie is `cookie`.
    fn close(&mut self, cookie: &str) {
        if let Some(digest) = token_digest(cookie) {
            self.live.remove(&digest);
        }
    }
}

/// Failed sign-ins by client address, within the window.
#[derive(Default)]
struct Failures {
    by_client: HashMap<IpAddr, Vec<Instant>>,
}

impl Failures {
    /// How long `client` must wait before its next sign-in, or `None` when it may try now.
    fn wait(&mut self, client: IpAddr, now: Instant) -> Option<Duration> {
        let failures = self.by_client.get_mut(&client)?;
        failures.retain(|at| recent(now, *at));
        if failures.len() < FAILURE_LIMIT {
            return None;
        }
        let oldest = failures.first()?;
        Some(FAILURE_WINDOW.saturating_sub(now.duration_since(*oldest)))
    }

    fn record(&mut self, client: IpAddr, now: Instant) {
        let by_client = &mut self.by_client;
        if by_client.len() >= CLIENT_LIMIT {
            by_client.retain(|_, failures| failures.iter().any(|at| recent(now, *at)));
        }
        by_client.entry(client).or_default().push(now);
    }

    fn clear(&mut self, client: IpAddr) {
        self.by_client.remove(&client);
    }
}

/// Whether a failure at `at` still counts at `now`.
fn recent(now: Instant, at: Instant) -> bool {
    now.duration_since(at) < FAILURE_WINDOW
}

/// The mark that a start or stop runs for one instance, removed when dropped.
struct Busy<'a> {
    set: &'a Mutex<HashSet<String>>,
    name: String,
}

impl<'a> Busy<'a> {
    fn claim(set: &'a Mutex<HashSet<String>>, name: &str) -> Result<Self, HttpError> {
        if !lock(set)?.insert(name.to_owned()) {
            return Err(conflict(format!("a start or stop of {name} is running")));
        }
        Ok(Self {
            set,
            name: name.to_owned(),
        })
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        // A lock is poisoned only by a panic, which the lints rule out; nothing is left to undo.
        if let Ok(mut busy) = self.set.lock() {
            busy.remove(&self.name);
        }
    }
}

/// Locks shared state. A poisoned lock (after a panic) is a server error, never ignored.
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, HttpError> {
    mutex.lock().map_err(poisoned)
}

fn poisoned<E>(_: E) -> HttpError {
    server_error("the manager's state is unusable after a failure; restart it".to_owned())
}

fn bad(message: impl Into<String>) -> HttpError {
    HttpError::new(400, message)
}

fn not_found() -> HttpError {
    HttpError::new(404, "no such page")
}

fn conflict(message: String) -> HttpError {
    HttpError::new(409, message)
}

fn server_error(message: String) -> HttpError {
    HttpError::new(500, message)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]
    use super::*;

    #[test]
    fn a_token_matches_only_itself() {
        let token = new_token().unwrap();
        assert_eq!(token.len(), 43);
        assert!(token_matches(&token, &token));
        assert!(!token_matches(&new_token().unwrap(), &token));
        assert!(!token_matches("", &token));
        assert!(!token_matches(&token[..42], &token));
        assert!(!token_matches("not a token", "not a token"));
    }

    #[test]
    fn the_sign_in_token_must_equal_its_cookie() {
        let token = new_token().unwrap();
        assert!(login_token_matches(Some(&token), &token));
        assert!(!login_token_matches(None, &token));
        assert!(!login_token_matches(Some(&new_token().unwrap()), &token));
    }

    #[test]
    fn a_session_opens_answers_its_token_expires_and_closes() {
        let mut sessions = Sessions::default();
        let now = Instant::now();
        let cookie = sessions.open(now).unwrap();
        let token = sessions.token(&cookie, now).unwrap();
        assert_ne!(token, cookie);
        assert!(sessions.token(&new_token().unwrap(), now).is_none());
        assert!(sessions.token("malformed", now).is_none());
        assert!(sessions.token(&cookie, now + SESSION_LIFETIME).is_none());
        sessions.close(&cookie);
        assert!(sessions.token(&cookie, now).is_none());
    }

    #[test]
    fn sessions_beyond_the_limit_end_the_oldest() {
        let mut sessions = Sessions::default();
        let start = Instant::now();
        let first = sessions.open(start).unwrap();
        for second in 1..=SESSION_LIMIT as u64 {
            sessions.open(start + Duration::from_secs(second)).unwrap();
        }
        assert_eq!(sessions.live.len(), SESSION_LIMIT);
        assert!(sessions.token(&first, start).is_none());
    }

    #[test]
    fn five_failures_make_an_address_wait_until_the_window_passes() {
        let mut failures = Failures::default();
        let client: IpAddr = "127.0.0.1".parse().unwrap();
        let other: IpAddr = "127.0.0.2".parse().unwrap();
        let start = Instant::now();
        for _ in 0..FAILURE_LIMIT - 1 {
            failures.record(client, start);
        }
        assert_eq!(failures.wait(client, start), None);
        failures.record(client, start);
        assert_eq!(failures.wait(client, start), Some(FAILURE_WINDOW));
        let later = start + Duration::from_secs(60);
        let left = FAILURE_WINDOW - Duration::from_secs(60);
        assert_eq!(failures.wait(client, later), Some(left));
        assert_eq!(failures.wait(other, start), None);
        assert_eq!(failures.wait(client, start + FAILURE_WINDOW), None);
        failures.record(client, start);
        failures.clear(client);
        assert_eq!(failures.wait(client, start), None);
    }

    #[test]
    fn cookies_are_http_only_same_site_strict_and_secure_behind_https() {
        let session = cookie(SESSION_COOKIE, "abc", SESSION_PATH, None);
        let expected = "erp_manager_session=abc; Path=/manager; HttpOnly; SameSite=Strict";
        assert_eq!(session, expected);
        let login = cookie(LOGIN_COOKIE, "abc", LOGIN_PATH, Some(600));
        assert!(login.ends_with("; Path=/manager/login; HttpOnly; SameSite=Strict; Max-Age=600"));
    }

    #[test]
    fn action_paths_name_a_valid_instance_and_action() {
        let start = instance_action("/manager/instances/shop/start");
        assert_eq!(start, Some(("shop", Action::Start)));
        let stop = instance_action("/manager/instances/erpforall_local/stop");
        assert_eq!(stop, Some(("erpforall_local", Action::Stop)));
        assert_eq!(instance_action("/manager/instances/shop/delete"), None);
        assert_eq!(instance_action("/manager/instances/../start"), None);
        assert_eq!(instance_action("/manager/instances/a/b/start"), None);
        assert_eq!(instance_action("/manager/shop/start"), None);
    }

    #[test]
    fn a_refusal_is_a_page_with_its_status_and_escaped_message() {
        let mut wire = Vec::new();
        let refusal = HttpError::new(409, "<b>shop</b> is running");
        error_page(&refusal).write_to(&mut wire).unwrap();
        let text = String::from_utf8(wire).unwrap();
        assert!(text.starts_with("HTTP/1.1 409 Conflict\r\n"), "{text}");
        assert!(text.contains("&lt;b&gt;shop&lt;/b&gt; is"), "{text}");
    }
}
