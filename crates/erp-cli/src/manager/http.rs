// SPDX-License-Identifier: MIT OR Apache-2.0
//! The HTTP/1.1 the manager pages need and no more: one request per connection, a bounded head
//! and body, `Content-Length` bodies only, and every response closes the connection. The manager
//! is a small operator tool, on loopback unless configured otherwise, so it keeps out of the
//! server's HTTP stack and its request path.

use std::collections::BTreeMap;
use std::io::{Read, Write};

/// The longest request head read.
const HEAD_LIMIT: usize = 8 * 1024;
/// The longest request body read: the forms hold a password and a token.
const BODY_LIMIT: usize = 8 * 1024;
/// The most header lines read.
const HEADER_LIMIT: usize = 64;
/// Every page: nothing is loaded, forms post only to the manager, no framing.
const CONTENT_SECURITY_POLICY: &str =
    "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// The fields of a form, by name.
pub(super) type Fields = BTreeMap<String, String>;

/// The request methods the pages use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    Get,
    Post,
}

/// A request read whole.
#[derive(Debug)]
pub(super) struct Request {
    pub(super) method: Method,
    /// The path, without the query string.
    pub(super) path: String,
    headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

/// A refusal: the status and a sentence for the page.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct HttpError {
    pub(super) status: u16,
    pub(super) message: String,
}

impl HttpError {
    pub(super) fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

fn bad(message: impl Into<String>) -> HttpError {
    HttpError::new(400, message)
}

impl Request {
    /// The value of the header `name` (lower case) when the request has it once. A repeated
    /// header is refused rather than one of its values chosen.
    pub(super) fn header(&self, name: &str) -> Result<Option<&str>, HttpError> {
        let mut found = None;
        for (key, value) in &self.headers {
            if key == name {
                if found.is_some() {
                    return Err(bad(format!("the request repeats the {name} header")));
                }
                found = Some(value.as_str());
            }
        }
        Ok(found)
    }

    /// The length of the body: a POST declares one, a GET has none.
    fn content_length(&self) -> Result<usize, HttpError> {
        if self.header("transfer-encoding")?.is_some() {
            return Err(HttpError::new(501, "chunked request bodies are not accepted"));
        }
        let declared = self.header("content-length")?;
        if declared.is_none() && self.method == Method::Post {
            return Err(HttpError::new(411, "a POST request must declare Content-Length"));
        }
        let length = match declared {
            Some(text) => parse_length(text)?,
            None => 0,
        };
        if length > BODY_LIMIT {
            return Err(HttpError::new(413, "the request body is too large"));
        }
        if self.method == Method::Get && length > 0 {
            return Err(bad("a GET request carries no body"));
        }
        Ok(length)
    }
}

fn parse_length(text: &str) -> Result<usize, HttpError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(bad("Content-Length is not a number"));
    }
    text.parse().map_err(|_| bad("Content-Length is too big"))
}

/// Reads one request from `stream`.
pub(super) fn read_request<R: Read>(stream: &mut R) -> Result<Request, HttpError> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(end) = find_head_end(&buffer) {
            break end;
        }
        if buffer.len() > HEAD_LIMIT {
            return Err(HttpError::new(431, "the request head is too large"));
        }
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(_) => return Err(HttpError::new(408, "the request was not received")),
        };
        if read == 0 {
            return Err(bad("the connection closed before the request head ended"));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    if head_end > HEAD_LIMIT {
        return Err(HttpError::new(431, "the request head is too large"));
    }
    let head = match std::str::from_utf8(&buffer[..head_end]) {
        Ok(head) => head,
        Err(_) => return Err(bad("the request head is not UTF-8")),
    };
    let mut request = parse_head(head)?;
    let length = request.content_length()?;
    let mut body = buffer.split_off(head_end + 4);
    while body.len() < length {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(_) => return Err(HttpError::new(408, "the request body was not received")),
        };
        if read == 0 {
            return Err(bad("the connection closed before the request body ended"));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    request.body = body;
    Ok(request)
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|w| w == b"\r\n\r\n")
}

/// A request head as a request without its body.
fn parse_head(head: &str) -> Result<Request, HttpError> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad("the request line is malformed"));
    };
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(HttpError::new(505, "only HTTP/1.1 is served"));
    }
    let method = match method {
        "GET" => Method::Get,
        "POST" => Method::Post,
        _ => return Err(HttpError::new(405, "only GET and POST are served")),
    };
    if !target.starts_with('/') {
        return Err(bad("the request target is not a path"));
    }
    let path = target.split('?').next().unwrap_or(target);
    let mut headers = Vec::new();
    for line in lines {
        if line.starts_with([' ', '\t']) {
            return Err(bad("folded header lines are not accepted"));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(bad("a header line is malformed"));
        };
        if name.is_empty() || !name.bytes().all(token_byte) {
            return Err(bad("a header name is malformed"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    if headers.len() > HEADER_LIMIT {
        return Err(HttpError::new(431, "the request has too many headers"));
    }
    Ok(Request {
        method,
        path: path.to_owned(),
        headers,
        body: Vec::new(),
    })
}

/// A character of an HTTP token (RFC 9110 `tchar`).
fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// The fields of an `application/x-www-form-urlencoded` body. A field outside `allowed`, a field
/// given twice and a malformed escape are refused, as in the server's forms.
pub(super) fn form(request: &Request, allowed: &[&str]) -> Result<Fields, HttpError> {
    if request.header("content-type")? != Some("application/x-www-form-urlencoded") {
        return Err(HttpError::new(415, "expected application/x-www-form-urlencoded"));
    }
    let text = match std::str::from_utf8(&request.body) {
        Ok(text) => text,
        Err(_) => return Err(bad("the form is not UTF-8")),
    };
    let mut fields = Fields::new();
    for pair in text.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key)?;
        if !allowed.contains(&key.as_str()) {
            return Err(bad("unknown or duplicate form field"));
        }
        if fields.insert(key, decode(value)?).is_some() {
            return Err(bad("unknown or duplicate form field"));
        }
    }
    Ok(fields)
}

/// One form component decoded: `+` is a space and `%XX` a byte; the result must be UTF-8.
fn decode(text: &str) -> Result<String, HttpError> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            let high = bytes.get(index + 1).copied().and_then(hex_digit);
            let low = bytes.get(index + 2).copied().and_then(hex_digit);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(bad("a form field has a malformed escape"));
            };
            decoded.push(high * 16 + low);
            index += 3;
        } else if byte == b'+' {
            decoded.push(b' ');
            index += 1;
        } else {
            decoded.push(byte);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| bad("a form field is not UTF-8"))
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `text` safe inside HTML text and quoted attribute values.
pub(super) fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// The rule of the server's login form (`erp-server`, `form_origin`): a `Sec-Fetch-Site` other
/// than `same-origin` or `none`, or an `Origin` other than `expected`, is refused; a browser that
/// sends neither header passes.
pub(super) fn same_origin_form(site: Option<&str>, origin: Option<&str>, expected: &str) -> bool {
    let site_ok = site.is_none_or(|site| matches!(site, "same-origin" | "none"));
    site_ok && origin.is_none_or(|origin| origin == expected)
}

/// A response. Every one closes the connection and carries the same protective headers.
pub(super) struct Response {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl Response {
    /// An HTML page.
    pub(super) fn html(status: u16, body: String) -> Self {
        let content_type = ("Content-Type", "text/html; charset=utf-8".to_owned());
        Self {
            status,
            headers: vec![content_type],
            body,
        }
    }

    /// A `303 See Other` to `location`: the answer to a form that succeeded.
    pub(super) fn redirect(location: &str) -> Self {
        Self {
            status: 303,
            headers: vec![("Location", location.to_owned())],
            body: String::new(),
        }
    }

    /// The response with one more header.
    pub(super) fn with_header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }

    /// Writes the response to `out`.
    pub(super) fn write_to<W: Write>(&self, out: &mut W) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        head.push_str(&format!("Content-Security-Policy: {CONTENT_SECURITY_POLICY}\r\n"));
        head.push_str("Cache-Control: no-store\r\n");
        head.push_str("Referrer-Policy: same-origin\r\n");
        head.push_str("X-Content-Type-Options: nosniff\r\n");
        head.push_str("X-Frame-Options: DENY\r\n");
        head.push_str("Connection: close\r\n\r\n");
        out.write_all(head.as_bytes())?;
        out.write_all(self.body.as_bytes())?;
        out.flush()
    }
}

/// The reason phrase of the statuses the manager sends.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        505 => "HTTP Version Not Supported",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]
    use super::*;

    fn read(wire: &str) -> Result<Request, HttpError> {
        read_request(&mut std::io::Cursor::new(wire.as_bytes().to_vec()))
    }

    #[test]
    fn a_form_post_is_read_with_its_headers_and_body() {
        let body = "csrf_token=a%2Bb&password=x+y";
        let wire = format!(
            "POST /manager/login?next=1 HTTP/1.1\r\nHost: 127.0.0.1:8070\r\n\
             Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let request = read(&wire).unwrap();
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.path, "/manager/login");
        assert_eq!(request.header("host").unwrap(), Some("127.0.0.1:8070"));
        assert_eq!(request.header("origin").unwrap(), None);
        let fields = form(&request, &["csrf_token", "password"]).unwrap();
        assert_eq!(fields["csrf_token"], "a+b");
        assert_eq!(fields["password"], "x y");
    }

    #[test]
    fn unsupported_requests_are_refused_with_their_status() {
        let status = |wire: &str| read(wire).unwrap_err().status;
        assert_eq!(status("DELETE / HTTP/1.1\r\n\r\n"), 405);
        assert_eq!(status("GET / HTTP/2\r\n\r\n"), 505);
        assert_eq!(status("GET http://x/ HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("POST / HTTP/1.1\r\n\r\n"), 411);
        let chunked = "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(status(chunked), 501);
        let large = "POST / HTTP/1.1\r\nContent-Length: 9000\r\n\r\n";
        assert_eq!(status(large), 413);
        let folded = "GET / HTTP/1.1\r\nX-A: 1\r\n folded\r\n\r\n";
        assert_eq!(status(folded), 400);
        let long_head = format!("GET / HTTP/1.1\r\nX-A: {}\r\n\r\n", "a".repeat(9000));
        assert_eq!(status(&long_head), 431);
        assert_eq!(status("GET / HTTP/1.1\r\nHost: x"), 400);
    }

    #[test]
    fn a_repeated_header_is_refused_rather_than_chosen() {
        let request = read("GET / HTTP/1.1\r\nCookie: a=1\r\nCookie: a=2\r\n\r\n").unwrap();
        assert_eq!(request.header("cookie").unwrap_err().status, 400);
    }

    #[test]
    fn form_fields_outside_the_list_twice_or_malformed_are_refused() {
        let post = |body: &str| {
            let wire = format!(
                "POST / HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            read(&wire).unwrap()
        };
        let refused = |body: &str| form(&post(body), &["a"]).unwrap_err().status;
        assert_eq!(refused("b=1"), 400);
        assert_eq!(refused("a=1&a=2"), 400);
        assert_eq!(refused("a=%zz"), 400);
        assert_eq!(refused("a=%ff"), 400);
        assert_eq!(form(&post("a=%C3%A9"), &["a"]).unwrap()["a"], "é");
        let plain = read("POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\na=1").unwrap();
        assert_eq!(form(&plain, &["a"]).unwrap_err().status, 415);
    }

    #[test]
    fn html_special_characters_are_escaped() {
        let escaped = escape("<a href=\"x\">&'</a>");
        assert_eq!(escaped, "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn the_same_origin_rule_is_the_login_forms() {
        let origin = "http://127.0.0.1:8070";
        assert!(same_origin_form(None, None, origin));
        assert!(same_origin_form(Some("same-origin"), Some(origin), origin));
        assert!(same_origin_form(Some("none"), None, origin));
        assert!(!same_origin_form(Some("same-site"), None, origin));
        assert!(!same_origin_form(Some("cross-site"), Some(origin), origin));
        assert!(!same_origin_form(None, Some("http://127.0.0.1:8069"), origin));
        assert!(!same_origin_form(None, Some("http://localhost:8070"), origin));
    }

    #[test]
    fn responses_close_the_connection_and_carry_the_protective_headers() {
        let mut wire = Vec::new();
        let response = Response::html(200, "<p>é</p>".to_owned());
        let response = response.with_header("Set-Cookie", "a=b".to_owned());
        response.write_to(&mut wire).unwrap();
        let text = String::from_utf8(wire).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("\r\nSet-Cookie: a=b\r\n"), "{text}");
        assert!(text.contains("\r\nContent-Length: 9\r\n"), "{text}");
        assert!(text.contains("\r\nConnection: close\r\n"), "{text}");
        assert!(text.contains("frame-ancestors 'none'"), "{text}");
        assert!(text.contains("nosniff"), "{text}");
        assert!(text.ends_with("\r\n\r\n<p>é</p>"), "{text}");
        let mut wire = Vec::new();
        Response::redirect("/manager/").write_to(&mut wire).unwrap();
        let text = String::from_utf8(wire).unwrap();
        let expected = "HTTP/1.1 303 See Other\r\nLocation: /manager/\r\n";
        assert!(text.starts_with(expected), "{text}");
    }
}
