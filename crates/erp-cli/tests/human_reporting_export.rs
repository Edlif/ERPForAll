// SPDX-License-Identifier: MIT OR Apache-2.0
//! The dynamic report export through the real server binary: `reporting` is
//! installed from the browser route, the server restarts, and `POST /erp/reporting/export` answers the PDF
//! and the workbook of "Contacts by Country" with a browser session. The route's refusals (session,
//! origin, content type, method, bearer, body size by the name of its setting, unknown keys, a caller
//! without read access, the row bound by the name of its setting) are checked on the wire.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]
#[path = "support/native_http.rs"]
mod native_http;
#[path = "support/runtime_addons.rs"]
mod runtime_addons;

use erp_testkit::TestDb;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Cursor, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

struct Raw {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Raw {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// One exchange with the body kept as bytes (the answer is a zip archive).
fn raw(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Raw {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut wire = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        wire.push_str(&format!("{name}: {value}\r\n"));
    }
    wire.push_str("\r\n");
    stream.write_all(wire.as_bytes()).unwrap();
    // a refusal may be written before the whole body is read: ignore a closed connection here
    let _ = stream.write_all(body);
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("an HTTP answer");
    let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    Raw {
        status,
        headers,
        body: bytes[split + 4..].to_vec(),
    }
}

fn start(config: &Path, stderr: &Path, port: u16) -> Server {
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_erp"))
            .args(["server", "--config"])
            .arg(config)
            .stdout(Stdio::null())
            .stderr(fs::File::create(stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(180);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            panic!(
                "server exited {status}: {}",
                fs::read_to_string(stderr).unwrap_or_default()
            );
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok()
            && raw(port, "GET", "/erp/health/ready", &[], b"").status == 200
        {
            return server;
        }
        assert!(
            Instant::now() < until,
            "server did not become ready: {}",
            fs::read_to_string(stderr).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn login(port: u16, db: &str, user: &str, password: &str) -> String {
    let page = native_http::request(port, "GET", "/erp/login", &[], "");
    assert_eq!(page.status, 200);
    let prefs = native_http::cookie(&page, "prefs");
    let csrf = native_http::embedded(&page, "erp-login")["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let logged = native_http::request(
        port,
        "POST",
        "/erp/login",
        &[
            ("cookie", &prefs),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        &format!(
            "db={db}&login={user}&password={password}&redirect=%2Ferp&csrf_token={}",
            native_http::encoded(csrf.as_bytes())
        ),
    );
    assert_eq!(logged.status, 303, "login of {user}: {}", logged.body);
    format!("{prefs}; {}", native_http::cookie(&logged, "session_id"))
}

fn ok(reply: &native_http::Reply, what: &str) -> Value {
    assert_eq!(reply.status, 200, "{what}: {}", reply.body);
    serde_json::from_str(&reply.body).unwrap()
}

const OK_HEADERS: [(&str, &str); 2] = [
    ("content-type", "application/json"),
    ("sec-fetch-site", "same-origin"),
];

fn post(port: u16, cookies: Option<&str>, extra: &[(&str, &str)], body: &[u8]) -> Raw {
    let mut headers = OK_HEADERS.to_vec();
    if let Some(cookies) = cookies {
        headers.push(("cookie", cookies));
    }
    headers.extend_from_slice(extra);
    // a later header of the same name replaces the default one
    let mut unique: Vec<(&str, &str)> = Vec::new();
    for (name, value) in headers.into_iter().rev() {
        if !unique.iter().any(|(seen, _)| *seen == name) {
            unique.push((name, value));
        }
    }
    raw(port, "POST", "/erp/reporting/export", &unique, body)
}

fn part(bytes: &[u8], name: &str) -> String {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).expect("the answer is a zip archive");
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("the workbook has {name}"))
        .read_to_string(&mut text)
        .unwrap();
    text
}

/// The text of every page of a PDF, white space removed, read back with an independent parser.
fn pdf_text(bytes: &[u8]) -> String {
    let pdf = lopdf::Document::load_mem(bytes).expect("the answer is a PDF");
    pdf.get_pages()
        .keys()
        .map(|page| {
            pdf.extract_text(&[*page])
                .unwrap()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn the_report_export_answers_files_and_refuses_what_it_must() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(acceptance());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn acceptance() {
    let db = TestDb::create().await.unwrap();
    let role = format!("{}_web", db.name());
    let client = db.client().await.unwrap();
    let temporary = std::env::temp_dir().canonicalize().unwrap();
    let dir = tempfile::tempdir_in(temporary).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("addons");
    let addons = dir.path().join("runtime-addons");
    runtime_addons::stage_runtime_addons(&source, &addons).unwrap();
    let addons = addons.canonicalize().unwrap();
    let config = native_http::config(dir.path(), &db, port, &role, &addons);
    client
        .batch_execute(&format!(
            "CREATE ROLE {} LOGIN PASSWORD 'fixture-runtime-password'; CREATE ROLE {} LOGIN PASSWORD 'fixture-runtime-password' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION",
            erp_sql::quote(&role),
            erp_sql::quote(&format!("{role}_node"))
        ))
        .await
        .unwrap();
    native_http::private(
        &dir.path().join("password"),
        "fixture-administrator-password",
    );
    let init = Command::new(env!("CARGO_BIN_EXE_erp"))
        .args(["db", "init", "--config"])
        .arg(&config)
        .args(["--admin-login", "fixture-admin", "--admin-password-file"])
        .arg(dir.path().join("password"))
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    native_http::login_rollout::prepare(&db, &role).await;
    let stderr = dir.path().join("server.stderr");
    let server = start(&config, &stderr, port);
    let admin = login(
        port,
        db.name(),
        "fixture-admin",
        "fixture-administrator-password",
    );
    let call = |cookies: &str, path: &str, body: Value| {
        native_http::json_post(port, cookies, path, &body.to_string())
    };
    ok(
        &call(
            &admin,
            "/erp/module/install",
            json!({"modules":["reporting"]}),
        ),
        "install reporting",
    );
    // a deployment installs its addons and restarts: serve from the installed registry
    drop(server);
    let _server = start(&config, &stderr, port);

    let xid = async |module: &str, name: &str| -> i64 {
        client
            .query_one(
                "SELECT res_id::int8 FROM ir_model_data WHERE module=$1 AND name=$2",
                &[&module, &name],
            )
            .await
            .unwrap()
            .get(0)
    };
    let main = xid("base", "main_company").await;
    let portal_group = xid("base", "group_portal").await;
    let report = xid("reporting", "report_partner_country").await;
    let company_name: String = client
        .query_one("SELECT name FROM res_company WHERE id = $1", &[&main])
        .await
        .unwrap()
        .get(0);

    // 60 partners in 60 different countries
    let countries: Vec<i64> = client
        .query(
            "SELECT id::int8 FROM res_country ORDER BY id LIMIT 130",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(countries.len(), 130, "the base data has the countries");
    let make = |range: std::ops::Range<usize>| {
        let vals: Vec<Value> = range
            .map(|i| json!({"name": format!("Export Partner {i}"), "country_id": countries[i], "company_id": main}))
            .collect();
        ok(
            &call(
                &admin,
                "/erp/json/2/res.partner/create",
                json!({"vals_list": vals}),
            ),
            "partners",
        );
    };
    make(0..60);

    // a portal user cannot read the report definitions
    let portal_partner = ok(
        &call(
            &admin,
            "/erp/json/2/res.partner/create",
            json!({"vals_list":[{"name":"Export Portal","company_id":main}]}),
        ),
        "portal partner",
    )[0]
    .as_i64()
    .unwrap();
    ok(
        &call(
            &admin,
            "/erp/json/2/res.users/create",
            json!({"vals_list":[{
                "name":"export-portal", "login":"export-portal", "password":"fixture-export-portal",
                "partner_id":portal_partner, "company_id":main, "company_ids":[[6,0,[main]]],
                "group_ids":[[6,0,[portal_group]]], "lang":"en_US", "tz":"UTC"
            }]}),
        ),
        "portal",
    );
    let portal = login(port, db.name(), "export-portal", "fixture-export-portal");

    let request = |format: &str| {
        json!({"report": report, "options": {"unfold_all": true}, "format": format})
            .to_string()
            .into_bytes()
    };

    // the workbook: one row per country with its count, the header with the period, the options sheet
    let book = post(port, Some(&admin), &[], &request("xlsx"));
    assert_eq!(book.status, 200, "{}", book.text());
    assert_eq!(
        book.header("content-type"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
    );
    assert_eq!(
        book.header("content-disposition"),
        Some(
            "attachment; filename=\"Contacts by Country.xlsx\"; filename*=UTF-8''Contacts%20by%20Country.xlsx"
        )
    );
    assert_eq!(book.header("cache-control"), Some("private, no-store"));
    assert_eq!(book.header("x-content-type-options"), Some("nosniff"));
    let sheet = part(&book.body, "xl/worksheets/sheet1.xml");
    assert!(!sheet.contains("<f>"), "no cell is a formula");
    assert!(part(&book.body, "xl/sharedStrings.xml").contains(">Contacts<"));
    assert!(part(&book.body, "xl/workbook.xml").contains("name=\"Options\""));

    // the PDF: the letterhead, the title and the lines
    let pdf = post(port, Some(&admin), &[], &request("pdf"));
    assert_eq!(pdf.status, 200, "{}", pdf.text());
    assert_eq!(pdf.header("content-type"), Some("application/pdf"));
    assert!(pdf.body.starts_with(b"%PDF"));
    let text = pdf_text(&pdf.body);
    for want in [
        company_name.replace(' ', ""),
        "ContactsbyCountry".to_owned(),
        "Contacts".to_owned(),
    ] {
        assert!(text.contains(&want), "{want} in {text}");
    }

    // no session, a foreign origin, a cross-site fetch, the wrong content type, the wrong method, a bearer
    let nobody = post(port, None, &[], &request("xlsx"));
    assert_eq!(
        (nobody.status, nobody.text().as_str()),
        (401, "user is not connected")
    );
    let foreign = post(
        port,
        Some(&admin),
        &[("origin", "http://evil.example")],
        &request("xlsx"),
    );
    assert_eq!(foreign.status, 403, "{}", foreign.text());
    let cross = post(
        port,
        Some(&admin),
        &[("sec-fetch-site", "cross-site")],
        &request("xlsx"),
    );
    assert_eq!(cross.status, 403, "{}", cross.text());
    let own = post(
        port,
        Some(&admin),
        &[("origin", &format!("http://127.0.0.1:{port}"))],
        &request("xlsx"),
    );
    assert_eq!(own.status, 200, "the server's own origin is accepted");
    let form = post(
        port,
        Some(&admin),
        &[("content-type", "text/plain")],
        &request("xlsx"),
    );
    assert_eq!(form.status, 415);
    let get = raw(
        port,
        "GET",
        "/erp/reporting/export",
        &[("cookie", &admin), ("sec-fetch-site", "same-origin")],
        b"",
    );
    assert_eq!((get.status, get.header("allow")), (405, Some("POST")));
    let bearer = post(
        port,
        Some(&admin),
        &[("authorization", "Bearer nothing")],
        &request("xlsx"),
    );
    assert_eq!(bearer.status, 401);

    // a body over the declared bound is refused by the name of its setting, before it is parsed
    let huge = post(port, Some(&admin), &[], &vec![b' '; 1_048_577]);
    assert_eq!(huge.status, 413, "{}", huge.text());
    assert!(
        huge.text().contains("reporting.export_max_request_bytes"),
        "{}",
        huge.text()
    );

    // the options only: rows are an unknown key; a bad format and a foreign report id are refused
    let rows = post(
        port,
        Some(&admin),
        &[],
        json!({"report": report, "options": {}, "format": "pdf", "rows": [[1]]})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(rows.status, 422, "{}", rows.text());
    assert!(
        rows.text().contains("'rows' is not part"),
        "{}",
        rows.text()
    );
    let csv = post(
        port,
        Some(&admin),
        &[],
        json!({"report": report, "options": {}, "format": "csv"})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(csv.status, 422, "{}", csv.text());

    // a caller who may not read the definition gets the ordinary refusal and no file
    let denied = post(port, Some(&portal), &[], &request("xlsx"));
    assert_eq!(denied.status, 403, "{}", denied.text());

    // the row bound is a setting: refused by name over it, nothing truncated, and under it the file comes
    let write = |changes: Value| -> native_http::Reply {
        let revision = ok(
            &call(
                &admin,
                "/erp/json/2/ir.config.setting/erp_read_values",
                json!({"scope":{"kind":"global"},"keys":[]}),
            ),
            "revision",
        )["revision"]
            .clone();
        call(
            &admin,
            "/erp/json/2/ir.config.setting/erp_write_values",
            json!({"scope":{"kind":"global"},"expected_revision":revision,"changes":changes}),
        )
    };
    let below = write(json!([{"key":"reporting.export_max_rows","value":99}]));
    assert_ne!(below.status, 200, "under the floor: {}", below.body);
    let above = write(json!([{"key":"reporting.export_max_rows","value":500_001}]));
    assert_ne!(above.status, 200, "over the hard bound: {}", above.body);
    ok(
        &write(json!([{"key":"reporting.export_max_rows","value":100}])),
        "the row bound at its floor",
    );
    let under = post(port, Some(&admin), &[], &request("xlsx"));
    assert_eq!(
        under.status,
        200,
        "about 60 rows are inside 100: {}",
        under.text()
    );
    make(60..120);
    let over = post(port, Some(&admin), &[], &request("pdf"));
    assert_eq!(over.status, 422, "{}", over.text());
    assert!(
        over.text().contains("reporting.export_max_rows"),
        "{}",
        over.text()
    );
}
