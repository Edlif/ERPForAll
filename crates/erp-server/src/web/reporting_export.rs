// SPDX-License-Identifier: MIT OR Apache-2.0
//! `POST /erp/reporting/export`: a dynamic report's options,
//! answered as the PDF or `.xlsx` file the server computes from them. The route owns the transport: a browser session, the same
//! origin, a JSON body bounded by the declared setting, the download headers. The request's
//! checks, the recomputation as the caller and the file belong to the `reporting` addon behind
//! [`WebReportingExport`] (addons cannot depend on this crate: layer check).
use super::*;

/// The file a report export answers with.
#[derive(Debug)]
pub struct ReportingFile {
    /// The name offered to the user, already sanitized by the addon.
    pub file_name: String,
    /// The media type: PDF or spreadsheet.
    pub content_type: &'static str,
    /// The finished file.
    pub bytes: Vec<u8>,
}

/// Trusted composition adapter of the report export (the `reporting` addon's checked export).
pub trait WebReportingExport: Send + Sync {
    /// The largest request, in bytes, the route may buffer: the declared setting, read for the
    /// caller's session transaction (the route rolls it back afterwards).
    fn request_limit<'a>(
        &'a self,
        tx: &'a mut erp_orm::Transaction,
    ) -> erp_orm::BoxFuture<'a, erp_fields::Result<usize>>;
    /// Parse the request, recompute the report as the caller and write the file. Refusals are
    /// `Validation` (names the setting) and `Access`.
    fn export<'a>(
        &'a self,
        tx: &'a mut erp_orm::Transaction,
        body: &'a [u8],
    ) -> erp_orm::BoxFuture<'a, erp_fields::Result<ReportingFile>>;
}

impl WebApp {
    /// Supply the report export's composition. Without it the route answers `503` naming the
    /// missing composition.
    pub fn with_reporting_export(mut self, export: Arc<dyn WebReportingExport>) -> Self {
        self.reporting_export = Some(export);
        self
    }

    /// `POST /erp/reporting/export`.
    pub(super) async fn reporting_export(
        &self,
        parts: &axum::http::request::Parts,
        body: Body,
        auth: &Arc<Auth>,
    ) -> HttpResult<Response> {
        let headers = &parts.headers;
        if parts.method != Method::POST {
            let mut refused = response(StatusCode::METHOD_NOT_ALLOWED, "POST required");
            refused
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("POST"));
            return Err(refused);
        }
        boundary::same_origin_fetch(
            one(headers, "sec-fetch-site")?,
            one(headers, "origin")?,
            self.config.origin(),
        )
        .map_err(|error| response(StatusCode::FORBIDDEN, error.to_string()))?;
        if one(headers, "content-type")? != Some("application/json") {
            return Err(response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected application/json",
            ));
        }
        if headers.contains_key(header::AUTHORIZATION) {
            return Err(response(
                StatusCode::UNAUTHORIZED,
                "Bearer credentials are not accepted on the report export; sign in with a browser session.",
            ));
        }
        let exporter = self.reporting_export.as_ref().ok_or_else(|| {
            response(
                StatusCode::SERVICE_UNAVAILABLE,
                "report export composition is unavailable",
            )
        })?;
        let secret = self
            .session(headers)?
            .ok_or_else(|| response(StatusCode::UNAUTHORIZED, "user is not connected"))?;
        // The bound is a setting, read before the body is buffered; no connection is held while
        // the body arrives.
        let mut begun = auth
            .begin_session(&secret, CredentialOptions::new())
            .await
            .map_err(auth_error)?;
        let limit = exporter.request_limit(&mut begun.tx).await;
        let rolled_back = begun.tx.rollback().await;
        drop(begun);
        let limit = match limit {
            Ok(limit) => limit,
            Err(error) => return Err(self.rpc_error(error).await),
        };
        rolled_back.map_err(auth_error)?;
        let bytes = to_bytes(body, limit).await.map_err(|_| {
            response(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "The export request is larger than {limit} bytes, the limit set by reporting.export_max_request_bytes. Unfold fewer lines or ask an administrator to raise the setting."
                ),
            )
        })?;
        let mut begun = auth
            .begin_session(&secret, CredentialOptions::new())
            .await
            .map_err(auth_error)?;
        let written = exporter.export(&mut begun.tx, &bytes).await;
        let rolled_back = begun.tx.rollback().await;
        drop(begun);
        let file = match written {
            Ok(file) => file,
            Err(error) => return Err(self.rpc_error(error).await),
        };
        rolled_back.map_err(auth_error)?;
        let mut result = Response::new(Body::from(file.bytes));
        let out = result.headers_mut();
        out.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(file.content_type),
        );
        out.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&content::disposition(false, &file.file_name))
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
        Ok(result)
    }
}
