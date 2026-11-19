// SPDX-License-Identifier: MIT OR Apache-2.0
//! Composition of the dynamic report export (`POST /erp/reporting/export`): the
//! server's [`WebReportingExport`] seam backed by the `reporting` addon's checked export. Addons
//! cannot depend on the server crate (layer check), so the adapter lives here and holds no
//! state. The spreadsheet is CPU work and runs on the blocking pool; the PDF is laid out by the
//! node's report runtime (`report_job::layout`).
use erp_addon_reporting::export::{self, Format};
use erp_server::web::{ReportingFile, WebReportingExport};

/// The `reporting` addon's export behind the server's route.
pub(crate) struct ReportingAddonExport;

impl WebReportingExport for ReportingAddonExport {
    fn request_limit<'a>(
        &'a self,
        tx: &'a mut erp_orm::Transaction,
    ) -> erp_orm::BoxFuture<'a, erp_fields::Result<usize>> {
        Box::pin(export::request_limit(tx))
    }

    fn export<'a>(
        &'a self,
        tx: &'a mut erp_orm::Transaction,
        body: &'a [u8],
    ) -> erp_orm::BoxFuture<'a, erp_fields::Result<ReportingFile>> {
        Box::pin(async move {
            let prepared = export::prepare(tx, body).await?;
            let file_name = prepared.file_name();
            let format = prepared.format;
            let content_type = format.content_type();
            let bytes = match format {
                Format::Xlsx => blocking(move || prepared.xlsx().map(|book| book.bytes)).await?,
                Format::Pdf => {
                    let document = blocking(move || prepared.document()).await?;
                    erp_addon_base::report_job::layout(document).await?
                }
                _ => {
                    return Err(erp_fields::Error::internal(
                        "reporting export: a format this server does not write",
                    ));
                }
            };
            Ok(ReportingFile {
                file_name,
                content_type,
                bytes,
            })
        })
    }
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> erp_fields::Result<T> + Send + 'static,
) -> erp_fields::Result<T> {
    tokio::task::spawn_blocking(job).await.map_err(|error| {
        erp_fields::Error::internal(format!("the report export task failed: {error}"))
    })?
}
