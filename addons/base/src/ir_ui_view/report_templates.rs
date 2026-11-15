// SPDX-License-Identifier: MIT OR Apache-2.0
//! Report templates checked when they load (T-RPT.4).
//!
//! A report action (`ir.actions.report`) names a QWeb view by its key. When the action and the
//! view are both in the database, the template and every template it calls by a literal `t-call`
//! are checked with the report library (`erp_sdk::report`):
//!
//! - the print profile ([`validate_arch`]) for the `qweb-pdf` and `qweb-html` types: a tag, class,
//!   attribute or directive outside the closed vocabulary is refused with the template, the line
//!   and the token;
//! - the attachment field check ([`check_attachment`]) for an action with an `attachment`
//!   expression: a template that prints a field limited to groups is refused unless the action
//!   sets `allow_attachment_group_fields`, and a template the check cannot follow is refused
//!   whatever the flag.
//!
//! When it runs:
//!
//! - outside a module operation (a user edits an action or a template): at once, in `create` and
//!   `write`, complete ([`Stage::Final`]);
//! - while a data file loads: at once for what is already there ([`Stage::Loading`]: a template
//!   or a called template that is not loaded yet is not an error yet), and the transaction is
//!   marked; the registry hook of `ir.actions.report` then checks every report action
//!   ([`check_all`], [`Stage::Final`]) once the install or upgrade has loaded everything.
//!
//! Not checked here: the vocabulary of `qweb-text` reports (no print profile applies to them; their
//! template must exist like any other), a called template
//! named by an expression (the compiler resolves it per document), and a change of a
//! called template outside a module operation (only the reports whose own template is in the
//! written tree are rechecked; the compiler checks the rest when the report prints).

use std::collections::BTreeSet;

use erp_sdk::load_state::INSTALL_MODE;
use erp_sdk::prelude::*;
use erp_sdk::report::{
    AttachmentPolicy, FieldInfo, ReportError, ReportLimits, TemplateSource, check_attachment,
    validate_arch,
};
use erp_sdk::views::{LimitKind, NodeRef, ViewCatalogue, ViewError, ViewLimits};
use erp_sdk::{Context, Registry};

use super::tree::fetch_trees_read_only;
use super::view_error;

/// How much of a report must be present for the check to pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// A data file is loading: what is missing may still come.
    Loading,
    /// Everything is loaded: a missing template is an error.
    Final,
}

/// Marker of a transaction that wrote a report action or a QWeb view during a module operation
/// (`Transaction::mark_scoped_id`); private, so only this module sets and reads it.
struct ReportCheckPending;

/// Whether `context` is the data loader's (`install_mode`, a server-only key).
pub(crate) fn loading(context: &Context) -> bool {
    context.get(INSTALL_MODE).is_some_and(|v| v.is_truthy())
}

/// The stage of a write in `context`; a loading write also marks `tx` for [`check_all`].
pub(crate) fn stage_of(tx: &mut Transaction, context: &Context) -> Stage {
    if loading(context) {
        tx.mark_scoped_id::<ReportCheckPending>(1);
        Stage::Loading
    } else {
        Stage::Final
    }
}

/// One report action as the check reads it.
struct Report {
    name: String,
    report_name: String,
    model: String,
    report_type: String,
    attachment: bool,
    acknowledged: bool,
}

const REPORT_COLUMNS: &str = "r.name, r.report_name, r.model, r.report_type, \
     COALESCE(btrim(r.attachment), '') <> '', COALESCE(r.allow_attachment_group_fields, false)";

fn report_of(row: &erp_sdk::Row) -> Result<Report> {
    let bad = |what: &str| Error::db(format!("unexpected value in ir_act_report_xml.{what}"));
    Ok(Report {
        name: row
            .try_get::<_, Option<String>>(0)
            .map_err(|_| bad("name"))?
            .unwrap_or_default(),
        report_name: row
            .try_get::<_, Option<String>>(1)
            .map_err(|_| bad("report_name"))?
            .unwrap_or_default(),
        model: row
            .try_get::<_, Option<String>>(2)
            .map_err(|_| bad("model"))?
            .unwrap_or_default(),
        report_type: row
            .try_get::<_, Option<String>>(3)
            .map_err(|_| bad("report_type"))?
            .unwrap_or_default(),
        attachment: row.try_get(4).map_err(|_| bad("attachment"))?,
        acknowledged: row
            .try_get(5)
            .map_err(|_| bad("allow_attachment_group_fields"))?,
    })
}

/// Check the report actions `ids` (rows of `ir_act_report_xml`).
///
/// # Errors
///
/// `Validation` naming the report and what the library refused; at [`Stage::Final`] also for a
/// template or a called template that is not a QWeb view of the database; a database error.
pub(crate) async fn check_reports(tx: &mut Transaction, ids: &[i64], stage: Stage) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    tx.flush().await?;
    let sql = format!(
        "SELECT {REPORT_COLUMNS} FROM ir_act_report_xml r WHERE r.id = ANY($1) ORDER BY r.id"
    );
    let rows = tx.query(&sql, &[&ids]).await?;
    for row in &rows {
        check_report(tx, &report_of(row)?, stage).await?;
    }
    Ok(())
}

/// Check the report actions whose own template is one of the QWeb views `view_ids` (the views of
/// the trees a view write touched).
///
/// # Errors
///
/// As [`check_reports`].
pub(crate) async fn check_reports_of_views(
    tx: &mut Transaction,
    view_ids: &[i64],
    stage: Stage,
) -> Result<()> {
    if view_ids.is_empty() {
        return Ok(());
    }
    tx.flush().await?;
    let sql = format!(
        "SELECT {REPORT_COLUMNS} FROM ir_act_report_xml r WHERE r.report_name IN \
         (SELECT v.key FROM ir_ui_view v WHERE v.id = ANY($1) AND v.type = 'qweb') ORDER BY r.id"
    );
    let rows = tx.query(&sql, &[&view_ids]).await?;
    for row in &rows {
        check_report(tx, &report_of(row)?, stage).await?;
    }
    Ok(())
}

/// The final check of a module operation: every report action, when the transaction wrote a
/// report action or a QWeb view while loading; nothing otherwise (a plain load reads nothing).
///
/// # Errors
///
/// As [`check_reports`] at [`Stage::Final`].
pub(crate) async fn check_all(tx: &mut Transaction) -> Result<()> {
    if tx.scoped_ids::<ReportCheckPending>().is_empty() {
        return Ok(());
    }
    tx.flush().await?;
    let sql = format!("SELECT {REPORT_COLUMNS} FROM ir_act_report_xml r ORDER BY r.id");
    let rows = tx.query(&sql, &[]).await?;
    for row in &rows {
        check_report(tx, &report_of(row)?, Stage::Final).await?;
    }
    Ok(())
}

/// The report's label in a refusal: its name and its template.
fn label(report: &Report) -> String {
    format!("Report '{}' ({})", report.name, report.report_name)
}

fn refused(report: &Report, error: &ReportError) -> Error {
    Error::validation(format!("{}: {error}", label(report)))
}

fn engine_refused(report: &Report, key: &str, error: &ViewError) -> Error {
    match error {
        ViewError::InvariantViolation(_) => Error::invariant(error.to_string()),
        other => Error::validation(format!("{}: template '{key}': {other}", label(report))),
    }
}

/// The active QWeb views of `keys`, one per key: a primary view first, then by priority and id.
async fn views_of_keys(tx: &mut Transaction, keys: &[String]) -> Result<Vec<(String, i64)>> {
    let rows = tx
        .query(
            "SELECT DISTINCT ON (key) key, id FROM ir_ui_view \
              WHERE key = ANY($1) AND type = 'qweb' AND active IS TRUE \
              ORDER BY key, (mode = 'primary') DESC, priority, id",
            &[&keys],
        )
        .await?;
    let bad = |what: &str| Error::db(format!("unexpected value in ir_ui_view.{what}"));
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get::<_, String>(0).map_err(|_| bad("key"))?,
                row.try_get::<_, i64>(1).map_err(|_| bad("id"))?,
            ))
        })
        .collect()
}

/// The literal `t-call` targets under `root`, in document order (an expression is not one).
fn literal_calls(root: NodeRef<'_>, out: &mut Vec<String>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if let Some(target) = node.attr("t-call") {
            let target = target.trim();
            if !target.is_empty() && !target.contains("{{") && !target.contains("#{") {
                out.push(target.to_owned());
            }
        }
        let children: Vec<NodeRef<'_>> = node.children().collect();
        stack.extend(children.into_iter().rev());
    }
}

/// The field metadata the attachment check asks for, from the registry: the comodel of a
/// relation, and whether the field is limited to groups. `None` for an unknown model or field
/// (the library refuses what it cannot resolve).
fn field_info(registry: &Registry, model: &str, field: &str) -> Option<FieldInfo> {
    let spec = registry.model(model)?.field(field)?;
    let restricted = spec.groups.is_some_and(|groups| !groups.trim().is_empty());
    Some(match spec.ty.comodel() {
        Some(comodel) => FieldInfo::relation(comodel, restricted),
        None => FieldInfo::scalar(restricted),
    })
}

/// The combined archs of `report`'s template and, level by level, of every template it calls by a
/// literal `t-call` when `follow_calls`, checked against the print profile when `profile`; with
/// the keys that are not an active QWeb view.
async fn gather(
    tx: &mut Transaction,
    report: &Report,
    profile: bool,
    follow_calls: bool,
) -> Result<(Vec<(String, String)>, Vec<String>)> {
    let limits = ReportLimits::default();
    let max_templates = ViewLimits::default().get(LimitKind::CatalogueViews);
    // two statements per level
    let mut seen: BTreeSet<String> = BTreeSet::from([report.report_name.clone()]);
    let mut level: Vec<String> = vec![report.report_name.clone()];
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    while !level.is_empty() {
        let found = views_of_keys(tx, &level).await?;
        for key in &level {
            if !found.iter().any(|(k, _)| k == key) {
                missing.push(key.clone());
            }
        }
        let ids: Vec<i64> = found.iter().map(|(_, id)| *id).collect();
        let mut next: Vec<String> = Vec::new();
        if !ids.is_empty() {
            let trees = fetch_trees_read_only(tx, &ids).await?;
            let mut descriptors = Vec::with_capacity(trees.rows.len());
            for row in trees.rows {
                descriptors.push(row.into_descriptor().map_err(view_error)?);
            }
            let catalogue = ViewCatalogue::new(descriptors)
                .map_err(|e| engine_refused(report, &report.report_name, &e))?;
            for (key, id) in &found {
                let combined = catalogue
                    .combine(*id)
                    .map_err(|e| engine_refused(report, key, &e))?;
                if profile {
                    validate_arch(key, combined.arch(), limits).map_err(|e| refused(report, &e))?;
                }
                let mut calls = Vec::new();
                if follow_calls {
                    literal_calls(combined.arch().root(), &mut calls);
                }
                for call in calls {
                    if seen.insert(call.clone()) {
                        next.push(call);
                    }
                }
                sources.push((key.clone(), combined.arch().to_xml()));
            }
        }
        if seen.len() > max_templates {
            return Err(Error::validation(format!(
                "{}: the template calls more than {max_templates} templates",
                label(report)
            )));
        }
        level = next;
    }
    Ok((sources, missing))
}

/// The combined archs (key, XML) of the report template `report_name` and of every template it
/// calls, checked against the print profile, as the report compiler takes them. `name` labels
/// the refusals.
///
/// # Errors
///
/// `Validation` naming the report for a template that is missing or outside the print profile;
/// a database error.
pub(crate) async fn template_sources(
    tx: &mut Transaction,
    name: &str,
    report_name: &str,
) -> Result<Vec<(String, String)>> {
    tx.flush().await?;
    let report = Report {
        name: name.to_owned(),
        report_name: report_name.to_owned(),
        model: String::new(),
        report_type: "qweb-pdf".to_owned(),
        attachment: false,
        acknowledged: false,
    };
    let (sources, missing) = gather(tx, &report, true, true).await?;
    if !missing.is_empty() {
        return Err(Error::validation(format!(
            "{}: template '{}' is not an active QWeb view of this database",
            label(&report),
            missing.join("', '")
        )));
    }
    Ok(sources)
}

async fn check_report(tx: &mut Transaction, report: &Report, stage: Stage) -> Result<()> {
    let profile = matches!(report.report_type.as_str(), "qweb-pdf" | "qweb-html");
    // a text report without attachment has nothing the library checks: only its own template
    // must exist
    let follow_calls = profile || report.attachment;
    let limits = ReportLimits::default();
    let (sources, missing) = gather(tx, report, profile, follow_calls).await?;
    if !missing.is_empty() {
        return match stage {
            // the rest of the module may still bring them; `check_all` decides
            Stage::Loading => Ok(()),
            Stage::Final => Err(Error::validation(format!(
                "{}: template '{}' is not an active QWeb view of this database",
                label(report),
                missing.join("', '")
            ))),
        };
    }
    if report.attachment {
        let registry = Arc::clone(tx.env().registry());
        let policy = AttachmentPolicy::new(&report.name, &report.model)
            .with_attachment(true)
            .with_acknowledgement(report.acknowledged);
        let templates: Vec<TemplateSource<'_>> = sources
            .iter()
            .map(|(key, arch)| TemplateSource::new(key, arch))
            .collect();
        check_attachment(
            &policy,
            &templates,
            &report.report_name,
            &|model: &str, field: &str| field_info(&registry, model, field),
            limits,
        )
        .map_err(|e| match e {
            // these two name the report and the template themselves
            ReportError::Restricted { .. } | ReportError::Unresolved { .. } => {
                Error::validation(e.to_string())
            }
            other => refused(report, &other),
        })?;
    }
    Ok(())
}
