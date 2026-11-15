// SPDX-License-Identifier: MIT OR Apache-2.0
//! `ids` are comma-separated record ids; `?download=1` asks for an attachment instead of
//! an inline answer. The route owns the transport: a browser session, the path syntax, the
//! headers. Finding the report, the access checks, the document and its layout belong to the
//! composition behind [`WebReport`] (addons cannot depend on this crate: layer check).
use super::*;

/// Most record ids one report address takes before the database is asked; the composition behind
/// the seam then refuses more than `base.report_sync_max_records` and repeated ids.
const MAX_IDS: usize = 10_000;
/// Longest report reference in the path, in bytes.
const MAX_REPORT_BYTES: usize = 128;

/// What the route answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReportFormat {
    /// The laid-out PDF.
    Pdf,
    /// The HTML preview of the same document.
    Html,
}

/// A printed report.
#[derive(Debug)]
pub struct PrintedReport {
    /// The name offered to the user, already cleaned by the composition (with its extension).
    pub file_name: String,
    /// The finished file.
    pub bytes: Vec<u8>,
}

/// Trusted composition adapter of the report route.
pub trait WebReport: Send + Sync {
    /// Find `report` (technical name, id or external id) as the caller may see it, check that the
    /// caller may read every record of `ids`, and print them. Refusals are `Missing`, `Access`
    /// and `Validation` naming what was refused.
    fn print<'a>(
        &'a self,
        tx: &'a mut erp_orm::Transaction,
        format: ReportFormat,
        report: &'a str,
        ids: &'a [i64],
    ) -> erp_orm::BoxFuture<'a, erp_fields::Result<PrintedReport>>;
}

/// `<pdf|html>/<report>/<ids>`: the format, the reference and the ids, or why not.
fn parse(rest: &str) -> Result<(ReportFormat, &str, Vec<i64>), String> {
    let mut parts = rest.split('/');
    let (Some(format), Some(report), Some(ids), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("A report address is /erp/report/<pdf|html>/<report>/<ids>.".to_owned());
    };
    let format = match format {
        "pdf" => ReportFormat::Pdf,
        "html" => ReportFormat::Html,
        other => return Err(format!("'{other}' is not a report format: pdf or html.")),
    };
    if report.is_empty()
        || report.len() > MAX_REPORT_BYTES
        || !report
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.'))
    {
        return Err(format!(
            "A report reference is 1 to {MAX_REPORT_BYTES} letters, digits, '_' and '.'."
        ));
    }
    let ids = ids
        .split(',')
        .map(|id| id.parse::<i64>().ok().filter(|id| *id > 0))
        .collect::<Option<Vec<i64>>>()
        .ok_or_else(|| {
            "The record ids of a report address are positive integers separated by commas."
                .to_owned()
        })?;
    if ids.len() > MAX_IDS {
        return Err(format!(
            "A report address takes at most {MAX_IDS} record ids."
        ));
    }
    Ok((format, report, ids))
}

impl WebApp {
    /// Supply the report route's composition. Without it the route answers `503` naming the
    /// missing composition.
    pub fn with_report(mut self, report: Arc<dyn WebReport>) -> Self {
        self.report = Some(report);
        self
    }

    /// `GET /erp/report/<pdf|html>/<report>/<ids>`.
    pub(super) async fn report(
        &self,
        parts: &axum::http::request::Parts,
        rest: &str,
        auth: &Arc<Auth>,
    ) -> HttpResult<Response> {
        if parts.method != Method::GET {
            let mut refused = response(StatusCode::METHOD_NOT_ALLOWED, "GET required");
            refused
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET"));
            return Err(refused);
        }
        let (format, report, ids) =
            parse(rest).map_err(|reason| response(StatusCode::NOT_FOUND, reason))?;
        let download = matches!(parts.uri.query(), Some("download=1" | "download=true"));
        let headers = &parts.headers;
        if headers.contains_key(header::AUTHORIZATION) {
            return Err(response(
                StatusCode::UNAUTHORIZED,
                "Bearer credentials are not accepted on the report route; sign in with a browser session.",
            ));
        }
        let printer = self.report.as_ref().ok_or_else(|| {
            response(
                StatusCode::SERVICE_UNAVAILABLE,
                "report printing composition is unavailable",
            )
        })?;
        let secret = self
            .session(headers)?
            .ok_or_else(|| response(StatusCode::UNAUTHORIZED, "user is not connected"))?;
        let mut begun = auth
            .begin_session(&secret, CredentialOptions::new())
            .await
            .map_err(auth_error)?;
        // shortcut: the session transaction stays open while the document is laid out; upgrade
        // to a prepare step and a render step when long layouts hold too many connections.
        let printed = printer.print(&mut begun.tx, format, report, &ids).await;
        // printing reads only: nothing of the transaction is kept
        let rolled_back = begun.tx.rollback().await;
        drop(begun);
        let printed = match printed {
            Ok(printed) => printed,
            Err(error) => return Err(self.rpc_error(error).await),
        };
        rolled_back.map_err(auth_error)?;
        let content_type = match format {
            ReportFormat::Pdf => "application/pdf",
            ReportFormat::Html => "text/html; charset=utf-8",
        };
        let mut result = Response::new(Body::from(printed.bytes));
        let out = result.headers_mut();
        out.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        out.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&content::disposition(!download, &printed.file_name))
                .map_err(|_| internal())?,
        );
        out.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        out.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        if format == ReportFormat::Html {
            // the preview is our own escaped markup with a fixed stylesheet: no script runs in it
            out.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'none'; img-src data:; style-src 'unsafe-inline'; \
                     base-uri 'none'; form-action 'none'; frame-ancestors 'self'",
                ),
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn report_addresses_parse_or_name_the_refusal() {
        let (format, report, ids) = parse("pdf/sample_library.report_book/3,4").unwrap();
        assert_eq!(format, ReportFormat::Pdf);
        assert_eq!(report, "sample_library.report_book");
        assert_eq!(ids, vec![3, 4]);
        assert_eq!(parse("html/12/1").unwrap().0, ReportFormat::Html);
        assert!(
            parse("xlsx/a/1")
                .unwrap_err()
                .contains("not a report format")
        );
        assert!(parse("pdf/a/0").unwrap_err().contains("positive integers"));
        assert!(
            parse("pdf/a/1,x")
                .unwrap_err()
                .contains("positive integers")
        );
        assert!(parse("pdf/a b/1").unwrap_err().contains("report reference"));
        assert!(parse("pdf/a").unwrap_err().contains("/erp/report/"));
        assert!(parse("pdf/a/1/2").unwrap_err().contains("/erp/report/"));
    }
}
