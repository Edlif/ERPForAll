// SPDX-License-Identifier: MIT OR Apache-2.0
//! Turning a report and its records into a document (
//! §4).
//!
//! A print request names a report (its technical name, its database id or the external id of the
//! action) and the records. This module finds the report as the caller may see it, checks that
//! the caller may read every record, and runs the report library under the caller's rights:
//! compile the template and the templates it calls, plan the reads, load the records, evaluate.
//! The result is a [`DocTree`], the engine-neutral document. Laying it out into a PDF is the
//! node's work (the render worker pool), as is the HTTP route that answers with it: an addon has
//! no process to start.
//!
//! The template context is `docs` (the printed records), `doc` and `o` are the loop names the
//! templates choose, and `company` (the caller's current company, for the layouts).

use std::sync::Arc;

use erp_sdk::prelude::*;
use erp_sdk::report::{
    DataError, DocTree, DocumentOptions, EvalContext, EvalOptions, Language, Limits, Node,
    PageSetup, PlanContext, ReportError, ReportLimits, ReportProgram, TemplateSource, field_info,
    load_records,
};
use erp_sdk::{Operation, access::GroupId};

use crate::ir_actions::report::paperformat_of;
use crate::ir_ui_view::report_templates;

/// Longest downloaded file name, in bytes, before the extension.
const MAX_FILE_NAME_BYTES: usize = 200;
/// Points per millimetre.
const POINTS_PER_MM: f64 = 72.0 / 25.4;

/// A report action as a print reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrintReport {
    /// The action's id.
    pub id: i64,
    /// The action's name: the Print menu entry and the default file name.
    pub name: String,
    /// The model it prints.
    pub model: String,
    /// `qweb-pdf`, `qweb-html` or `qweb-text`.
    pub report_type: String,
    /// The key of its template.
    pub report_name: String,
    print_report_name: Option<String>,
}

fn report_error(report: &PrintReport, error: &ReportError) -> Error {
    Error::validation(format!(
        "Report '{}' ({}): {error}",
        report.name, report.report_name
    ))
}

/// Run compile and evaluation on the blocking pool: they are CPU work, and `erp server` may run
/// every request on one runtime thread.
async fn blocking<T: Send + 'static>(
    report: &PrintReport,
    work: impl FnOnce() -> std::result::Result<T, ReportError> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::internal(format!("Report '{}': the task failed: {e}", report.name)))?
        .map_err(|e| report_error(report, &e))
}

fn data_error(report: &PrintReport, error: DataError) -> Error {
    match error {
        // the caller's own refusal (no read access to a model, record or field) stays what it is
        DataError::Orm(error) => error,
        other => Error::validation(format!(
            "Report '{}' ({}): {other}",
            report.name, report.report_name
        )),
    }
}

/// The report `reference` names, if the caller may print it: a database id, the technical name
/// (`report_name`) or the external id of the action (`module.name`), tried in that order.
///
/// # Errors
///
/// `Missing` naming the reference when no report matches; `Access` when the report is limited to
/// groups the caller is not in; a database error.
pub async fn find(tx: &mut Transaction, reference: &str) -> Result<PrintReport> {
    tx.flush().await?;
    let by_id = reference.parse::<i64>().ok().filter(|id| *id > 0);
    let rows = tx
        .query(
            "SELECT r.id, r.name, r.model, r.report_type, r.report_name, r.print_report_name, \
                    ARRAY(SELECT g.gid::int8 FROM res_groups_report_rel g WHERE g.uid = r.id \
                          ORDER BY 1) \
               FROM ir_act_report_xml r \
              WHERE r.id = $1 OR r.report_name = $2 \
                 OR r.id = (SELECT d.res_id FROM ir_model_data d \
                             WHERE d.model = 'ir.actions.report' \
                               AND d.module || '.' || d.name = $2) \
              ORDER BY (r.id = $1) DESC, (r.report_name = $2) DESC, r.id LIMIT 1",
            &[&by_id.unwrap_or(0), &reference],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(Error::missing(format!(
            "There is no report '{reference}': no report action has that id, technical name or \
             external id."
        )));
    };
    let bad = |what: &str| Error::db(format!("unexpected value in ir_act_report_xml.{what}"));
    let text = |i: usize, what: &str| -> Result<String> {
        Ok(row
            .try_get::<_, Option<String>>(i)
            .map_err(|_| bad(what))?
            .unwrap_or_default())
    };
    let report = PrintReport {
        id: row.try_get(0).map_err(|_| bad("id"))?,
        name: text(1, "name")?,
        model: text(2, "model")?,
        report_type: text(3, "report_type")?,
        report_name: text(4, "report_name")?,
        print_report_name: row
            .try_get::<_, Option<String>>(5)
            .map_err(|_| bad("print_report_name"))?
            .filter(|expr| !expr.trim().is_empty()),
    };
    let groups: Vec<i64> = row.try_get(6).map_err(|_| bad("group_ids"))?;
    if !groups.is_empty() {
        let groups = groups
            .into_iter()
            .map(|g| GroupId::new(g).map_err(|e| Error::db(format!("report group {g}: {e}"))))
            .collect::<Result<Vec<GroupId>>>()?;
        let env = tx.env();
        if !tx.group_membership(&env).await?.has_any(&groups)? {
            return Err(Error::access(format!(
                "Report '{}' is limited to groups you are not a member of.",
                report.name
            )));
        }
    }
    Ok(report)
}

/// Refuse unless `ids` are records of the report's model that exist and that the caller may
/// read. `model`, when given, must be the report's model.
///
/// # Errors
///
/// `Validation` for another model or no id; `Missing` naming the ids that do not exist; `Access`
/// for a record the caller may not read.
pub async fn check_records(
    tx: &mut Transaction,
    report: &PrintReport,
    model: Option<&str>,
    ids: &[i64],
) -> Result<()> {
    if let Some(model) = model.filter(|model| *model != report.model) {
        return Err(Error::validation(format!(
            "Report '{}' prints {} records, not {model} records.",
            report.name, report.model
        )));
    }
    if ids.is_empty() {
        return Err(Error::validation("Select at least one record to print."));
    }
    let records = tx
        .env()
        .records(&report.model)?
        .browse(ids.iter().copied())?;
    let existing = records.exists(tx).await?.ids();
    let gone: Vec<String> = ids
        .iter()
        .filter(|id| !existing.contains(id))
        .map(ToString::to_string)
        .collect();
    if !gone.is_empty() {
        return Err(Error::missing(format!(
            "{} record(s) {} do not exist any more.",
            report.model,
            gone.join(", ")
        )));
    }
    records.check_access(tx, Operation::Read).await
}

/// The reports bound to the Print menu of `model` that the caller may see: `(report_name, name)`.
///
/// # Errors
///
/// `Validation` for a model the registry does not have; a database error.
pub async fn printable(tx: &mut Transaction, model: &str) -> Result<Vec<(String, String)>> {
    let env = tx.env();
    let bindings = crate::ir_actions::get_bindings(tx, &env, model).await?;
    let ids: Vec<i64> = bindings.report().iter().map(|b| b.id().get()).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = tx
        .query(
            "SELECT report_name, name FROM ir_act_report_xml WHERE id = ANY($1) ORDER BY id",
            &[&ids],
        )
        .await?;
    rows.iter()
        .map(|row| {
            let read = |i| {
                row.try_get::<_, Option<String>>(i)
                    .map_err(|_| Error::db("unexpected value in ir_act_report_xml"))
                    .map(Option::unwrap_or_default)
            };
            Ok((read(0)?, read(1)?))
        })
        .collect()
}

/// The page of the report for the caller's company, from its paper format.
// shortcut: a document has one symmetric margin, so the paper format's four margins become the
// larger side margin, at least the smaller of the top and bottom ones; upgrade when PageSetup
// takes per-side margins.
async fn page_of(
    tx: &mut Transaction,
    report: &PrintReport,
    company: Option<i64>,
) -> Result<PageSetup> {
    const FIELDS: [&str; 6] = [
        "print_page_width",
        "print_page_height",
        "margin_left",
        "margin_right",
        "margin_top",
        "margin_bottom",
    ];
    let format = paperformat_of(tx, report.id, company).await?;
    // the paper format is configuration of the report, not data of the caller
    let rows = tx
        .env()
        .sudo(true)
        .records("report.paperformat")?
        .browse([format])?
        .read(tx, &FIELDS)
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| Error::missing(format!("report.paperformat {format} does not exist")))?;
    let mut values = [0.0_f64; 6];
    for (value, field) in values.iter_mut().zip(FIELDS) {
        *value = match row.get(field) {
            Some(Value::Float(v)) => *v,
            #[allow(clippy::cast_precision_loss, reason = "a margin in millimetres")]
            Some(Value::Int(v)) => *v as f64,
            other => {
                return Err(Error::invariant(format!(
                    "report.paperformat.{field} read as {other:?}"
                )));
            }
        };
    }
    let [width, height, left, right, top, bottom] = values;
    let sides = left.max(right);
    let ends = top.min(bottom);
    PageSetup::new(
        width * POINTS_PER_MM,
        height * POINTS_PER_MM,
        sides.max(ends) * POINTS_PER_MM,
    )
    .map_err(|e| report_error(report, &e))
}

/// Compile, plan, load and evaluate `report` for `ids` (already checked with
/// [`check_records`]) into one document, under the caller's rights.
///
/// # Errors
///
/// `Validation` naming the report for a template the library refuses (missing, outside the print
/// profile, over a limit) or data it cannot print; the caller's `Access` refusal of a read; a
/// database error.
pub async fn prepare(tx: &mut Transaction, report: &PrintReport, ids: &[i64]) -> Result<DocTree> {
    let env = tx.env();
    let company = env.company_id()?;
    let companies: Vec<i64> = company.into_iter().collect();
    let program = compile(tx, report, &report.report_name, None).await?;
    let store = load(tx, report, &program, ids, &companies, &[]).await?;
    let context = EvalContext::standard()
        .with_var("docs", store.recordset(&report.model, ids))
        .with_var("company", store.recordset("res.company", &companies));
    let page = page_of(tx, report, company).await?;
    let options = DocumentOptions::new(format!("report-{}", report.id), report.name.clone(), page);
    blocking(report, move || {
        program.evaluate(&context, &store, &EvalOptions::default(), options)
    })
    .await
}

/// The language of the caller's context, or the reference's default.
fn language(report: &PrintReport, env: &Env) -> Result<Language> {
    Language::new(env.lang().unwrap_or("en_US")).map_err(|e| report_error(report, &e))
}

/// Compile `root` from the database templates, or from `inline` (key, arch) when given.
async fn compile(
    tx: &mut Transaction,
    report: &PrintReport,
    root: &str,
    inline: Option<(&str, &str)>,
) -> Result<ReportProgram> {
    let sources = match inline {
        Some((key, arch)) => vec![(key.to_owned(), arch.to_owned())],
        None => report_templates::template_sources(tx, &report.name, root).await?,
    };
    let env = tx.env();
    let language = language(report, &env)?;
    let root = root.to_owned();
    blocking(report, move || {
        let templates: Vec<TemplateSource<'_>> = sources
            .iter()
            .map(|(key, arch)| TemplateSource::new(key, arch))
            .collect();
        ReportProgram::compile(&root, &templates, &language, ReportLimits::default())
    })
    .await
}

/// Plan and run the reads of `program` for `ids` and the `company` ids, with `extra` record
/// bindings of the report's model.
async fn load(
    tx: &mut Transaction,
    report: &PrintReport,
    program: &ReportProgram,
    ids: &[i64],
    companies: &[i64],
    extra: &[&str],
) -> Result<Arc<erp_sdk::report::RecordStore>> {
    let env = tx.env();
    let registry = Arc::clone(env.registry());
    let mut bindings: Vec<(&str, Option<&str>)> = vec![("company", Some("res.company"))];
    bindings.extend(
        extra
            .iter()
            .map(|name| (*name, Some(report.model.as_str()))),
    );
    let plan = program
        .prefetch_plan(
            &PlanContext::new(&report.model).with_bindings(&bindings),
            &|model: &str, field: &str| field_info(&registry, model, field),
        )
        .map_err(|e| report_error(report, &e))?;
    let mut roots: Vec<(&str, &[i64])> = vec![("docs", ids), ("company", companies)];
    roots.extend(extra.iter().map(|name| (*name, ids)));
    let store = load_records(
        tx,
        &env,
        &plan,
        &roots,
        ReportLimits::default(),
        Limits::default(),
    )
    .await
    .map_err(|e| data_error(report, e))?;
    Ok(Arc::new(store))
}

/// The downloaded file's name for `ids`, with `extension` (`pdf`, `html`): the report's
/// `print_report_name` evaluated over `object` for one record, else the report's name. Path
/// separators and control characters are replaced, the length is bounded.
///
/// # Errors
///
/// `Validation` naming the report when the expression does not evaluate (it may read the
/// record's fields; `time` is not available); the caller's `Access` refusal of a read.
pub async fn file_name(
    tx: &mut Transaction,
    report: &PrintReport,
    ids: &[i64],
    extension: &str,
) -> Result<String> {
    let base = match (&report.print_report_name, ids) {
        (Some(expr), [_]) => {
            let arch = format!(
                "<t t-name=\"report.file_name\"><span t-out=\"{}\"/></t>",
                escape_attribute(expr)
            );
            let inline = Some(("report.file_name", arch.as_str()));
            let program = compile(tx, report, "report.file_name", inline).await?;
            let store = load(tx, report, &program, ids, &[], &["object"]).await?;
            let context = EvalContext::standard()
                .with_var("docs", store.recordset(&report.model, ids))
                .with_var("company", store.recordset("res.company", &[]))
                .with_var("object", store.recordset(&report.model, ids));
            let page = PageSetup::new(595.0, 842.0, 0.0).map_err(|e| report_error(report, &e))?;
            let document = blocking(report, move || {
                program.evaluate(
                    &context,
                    &store,
                    &EvalOptions::default(),
                    DocumentOptions::new("file-name", "file name", page),
                )
            })
            .await?;
            document
                .nodes()
                .iter()
                .filter_map(|node| match node {
                    Node::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        }
        _ => report.name.clone(),
    };
    Ok(format!("{}.{extension}", clean_file_name(&base)))
}

fn escape_attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// `name` without path separators or control characters, trimmed, at most
/// [`MAX_FILE_NAME_BYTES`] bytes; `report` when nothing is left.
fn clean_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut cleaned = cleaned.trim().to_owned();
    let mut end = cleaned.len().min(MAX_FILE_NAME_BYTES);
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned.truncate(end);
    if cleaned.is_empty() {
        "report".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn file_names_lose_separators_and_stay_bounded() {
        assert_eq!(clean_file_name("Book / Sheet\n"), "Book _ Sheet_");
        assert_eq!(clean_file_name("  "), "report");
        let long = "é".repeat(300);
        let cleaned = clean_file_name(&long);
        assert!(cleaned.len() <= MAX_FILE_NAME_BYTES);
        assert!(cleaned.chars().all(|c| c == 'é'));
        assert_eq!(
            escape_attribute("'a' + \"b\" & <c>"),
            "'a' + &quot;b&quot; &amp; &lt;c>"
        );
    }
}
