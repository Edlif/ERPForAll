// SPDX-License-Identifier: MIT OR Apache-2.0
//! `ir.actions.report`: a printable report of a model (
//! `:863-894`).
//!
//! A report action names the model it prints, its output type, the QWeb view that is its
//! template (`report_name` is the view's key) and, optionally, a paper format, the expression of
//! the downloaded file's name, and the expression of the attachment the printed file is kept as.
//! It is an action like the others: its ids come from the shared sequence and the key table
//! follows it, so `binding_model_id` puts it in the Print menu of its model
//! ([`super::get_bindings`]).
//!
//! Checked when the action is written, each refusal naming the action:
//!
//! - `model` is a model of the registry;
//! - `print_report_name` and `attachment` parse as expressions (`erp_sdk::expr`); they are
//!   evaluated when the report prints (the routes slice), never here;
//! - an action saved as attachment has an attachment expression;
//! - the template, when it is in the database, passes the report library's print profile and,
//!   for an action with an attachment expression, its field check
//!   ([`crate::ir_ui_view::report_templates`]). During a module operation the complete check
//!   runs once at the end, in `_register_hook`.
//!
//! Not here: rendering, the HTTP routes, the layouts and archiving (the later report slices).

use erp_sdk::expr::{EvalLimits, parse_with_limits};
use erp_sdk::prelude::*;
use erp_sdk::{Operation, RecId, ValueMap};

use super::{ACTIONS_CACHE, ACTIONS_ID_SEQUENCE, IrActionsBase, KEY_TABLE, row_ids};
use crate::ir_ui_view::report_templates;

/// The model name.
pub const MODEL: &str = "ir.actions.report";

/// The external id of the paper format used when neither the action nor the company names one
/// : `(module, name)`.
pub const FALLBACK_PAPERFORMAT: (&str, &str) = ("base", "paperformat_euro");

/// `ir.actions.report`.
#[model(
    name = "ir.actions.report",
    description = "Report Action",
    table = "ir_act_report_xml",
    inherit = [IrActionsBase],
    order = "name, id",
    allow_sudo_commands = false,
    id_sequence = ACTIONS_ID_SEQUENCE,
    follow_deletes = [
        ("_keys_follow", cache = ACTIONS_CACHE, key_table = KEY_TABLE),
        ("_groups_follow", cache = ACTIONS_CACHE, relation = "group_ids")
    ]
)]
pub struct IrActionsReport {
    /// The model whose records the report prints.
    #[field(char, required = true, string = "Model Name")]
    pub model: Option<Arc<str>>,
    /// What the report produces.
    #[field(
        selection,
        selection = [("qweb-html", "HTML"), ("qweb-pdf", "PDF"), ("qweb-text", "Text")],
        required = true,
        default = "qweb-pdf",
        string = "Format"
    )]
    pub report_type: Option<Arc<str>>,
    /// The key of the QWeb view that is the report's template.
    #[field(char, required = true, string = "Template Name")]
    pub report_name: Option<Arc<str>>,
    /// Kept for data files of the reference (the file a legacy report was read from); unused.
    #[field(char, string = "Report File")]
    pub report_file: Option<Arc<str>>,
    /// Groups that see the report in the Print menu; none: everybody.
    #[field(
        many2many,
        comodel = "res.groups",
        relation = "res_groups_report_rel",
        column1 = "uid",
        column2 = "gid",
        string = "Groups"
    )]
    pub group_ids: Vec<RecId>,
    /// Not shown in the Print menu of a form view.
    #[field(boolean, string = "On Multiple Doc.")]
    pub multi: bool,
    /// The paper format; none: the company's ([`paperformat_of`]).
    #[field(
        many2one,
        comodel = "report.paperformat",
        ondelete = "set null",
        index = true,
        string = "Paper Format"
    )]
    pub paperformat_id: Option<RecId>,
    /// Expression of the downloaded file's name over `object` and `time`; none: the action's
    /// name.
    #[field(char, string = "Printed Report Name")]
    pub print_report_name: Option<Arc<str>>,
    /// Keep the printed file as an attachment of the record.
    #[field(boolean, string = "Save as attachment")]
    pub save_as_attachment: bool,
    /// Print the stored attachment again instead of rendering anew.
    #[field(boolean, string = "Reuse the stored attachment")]
    pub attachment_use: bool,
    /// Expression of the attachment's name over `object` and `time`; none: nothing is kept.
    #[field(char, string = "Attachment Name")]
    pub attachment: Option<Arc<str>>,
    /// The administrator's acknowledgement that the template prints fields limited to groups and
    /// that the stored attachment shows them as the first printer saw them.
    #[field(boolean, string = "Allow group-restricted fields in the attachment")]
    pub allow_attachment_group_fields: bool,
    /// Domain of the records the report is offered for, as an expression; none: every record.
    #[field(char, string = "Domain")]
    pub domain: Option<Arc<str>>,
}

/// The fields whose change needs the template check again.
const TEMPLATE_INPUTS: [&str; 5] = [
    "report_name",
    "report_type",
    "model",
    "attachment",
    "allow_attachment_group_fields",
];

/// The text of `field` in `vals`: `None` when absent or cleared.
fn text<'v>(vals: &'v RawVals, field: &str) -> Result<Option<&'v str>> {
    match vals.get(field) {
        None | Some(Cell::Null | Cell::Bool(false)) => Ok(None),
        Some(Cell::Str(value)) => Ok(Some(value)),
        Some(_) => Err(Error::validation(format!(
            "ir.actions.report.{field} is text"
        ))),
    }
}

/// How a refusal names the action being written: its name when the values carry one.
fn action_label(vals: &RawVals) -> String {
    match vals.get("name") {
        Some(Cell::Str(name)) => format!("Report '{name}'"),
        _ => "The report".to_owned(),
    }
}

/// The checks of `vals` that need no row: the model exists, the expressions parse.
fn check_vals(registry: &erp_sdk::Registry, vals: &RawVals) -> Result<()> {
    if let Some(model) = text(vals, "model")?
        && registry.model(model).is_none()
    {
        return Err(Error::validation(format!(
            "{} prints model '{model}', which is not a model of this database",
            action_label(vals)
        )));
    }
    for field in ["print_report_name", "attachment"] {
        let Some(source) = text(vals, field)? else {
            continue;
        };
        if source.trim().is_empty() {
            continue;
        }
        parse_with_limits(source, EvalLimits::default()).map_err(|e| {
            Error::validation(format!(
                "{}: {field} is not a valid expression: {e}",
                action_label(vals)
            ))
        })?;
    }
    Ok(())
}

/// Refuse the rows of `ids` that are saved as attachment without an attachment expression.
async fn check_attachment_name(tx: &mut Transaction, ids: &[i64]) -> Result<()> {
    tx.flush().await?;
    let rows = tx
        .query(
            "SELECT name FROM ir_act_report_xml WHERE id = ANY($1) AND save_as_attachment \
               AND COALESCE(btrim(attachment), '') = '' ORDER BY id LIMIT 1",
            &[&ids],
        )
        .await?;
    match rows.first() {
        None => Ok(()),
        Some(row) => {
            let name: Option<String> = row
                .try_get(0)
                .map_err(|_| Error::db("unexpected value in ir_act_report_xml.name"))?;
            Err(Error::validation(format!(
                "Report '{}' is saved as attachment and needs an attachment name",
                name.unwrap_or_default()
            )))
        }
    }
}

#[methods(IrActionsReport)]
impl IrActionsReport {
    /// A report binds as `report` unless told otherwise; the values are checked
    /// ([`check_vals`]) and, once stored, the attachment rule and the template.
    #[method(override)]
    async fn create(
        this: &RecordSet<IrActionsReport>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let records = this.as_records();
        let registry = Arc::clone(records.env().registry());
        for vals in &mut vals_list {
            if !vals.contains("binding_type") {
                vals.insert("binding_type".into(), Cell::Str(Arc::from("report")));
            }
            check_vals(&registry, vals)?;
        }
        let stage = report_templates::stage_of(tx, records.env().context());
        let created: Records = sup.call(tx, records, (vals_list,)).await?;
        let ids = row_ids(&created)?;
        if !ids.is_empty() {
            check_attachment_name(tx, &ids).await?;
            // boxed: module lifecycle futures stay bounded (the 2 MiB lifecycle test)
            Box::pin(report_templates::check_reports(tx, &ids, stage)).await?;
        }
        Ok(created)
    }

    /// The same checks as `create` for what the write changes.
    #[method(override)]
    async fn write(
        this: &RecordSet<IrActionsReport>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        check_vals(records.env().registry(), &vals)?;
        let attachment = vals.contains("save_as_attachment") || vals.contains("attachment");
        let template = TEMPLATE_INPUTS.iter().any(|field| vals.contains(field));
        let stage = report_templates::stage_of(tx, records.env().context());
        sup.call(tx, records, (vals,)).await?;
        let ids = row_ids(records)?;
        if ids.is_empty() {
            return Ok(());
        }
        if attachment {
            check_attachment_name(tx, &ids).await?;
        }
        if template {
            // boxed: module lifecycle futures stay bounded (the 2 MiB lifecycle test)
            Box::pin(report_templates::check_reports(tx, &ids, stage)).await?;
        }
        Ok(())
    }

    /// The end of a module operation: every report action is checked against its template when
    /// the operation wrote a report action or a QWeb view ([`report_templates::check_all`]); a
    /// plain load reads nothing.
    #[method(override, sealed)]
    async fn _register_hook(
        this: &RecordSet<IrActionsReport>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        Box::pin(report_templates::check_all(tx)).await?;
        sup.call(tx, this.as_records(), ()).await
    }

    /// The client action that prints `docids` with this report: `type`,
    /// `name`, `report_name`, `report_type`, `model`, `data` as given (`false` when absent) and a
    /// `context` with `active_ids` when there are records.
    ///
    /// # Errors
    ///
    /// `Validation` unless called on exactly one report; `Access` when the caller cannot read
    /// it.
    #[method]
    #[rpc(effect = read)]
    pub async fn report_action(
        this: &RecordSet<IrActionsReport>,
        tx: &mut Transaction,
        docids: Vec<i64>,
        data: Option<Value>,
    ) -> Result<Value> {
        let records = this.as_records();
        if records.ids().len() != 1 {
            return Err(Error::validation(format!(
                "report_action expects one report, got {}",
                records.ids().len()
            )));
        }
        let one = |field: &'static str| -> Result<Value> {
            Err(Error::invariant(format!(
                "ir.actions.report.{field} was not read for one record"
            )))
        };
        let mut read = Vec::with_capacity(4);
        for field in ["name", "report_name", "report_type", "model"] {
            let mut values = records.values(tx, field).await?;
            read.push(match values.pop() {
                Some(value) if values.is_empty() => value,
                _ => one(field)?,
            });
        }
        let [name, report_name, report_type, model]: [Value; 4] = read
            .try_into()
            .map_err(|_| Error::invariant("report_action read four fields"))?;
        let mut context = ValueMap::new();
        if !docids.is_empty() {
            context.insert(
                "active_ids".to_owned(),
                Value::List(docids.into_iter().map(Value::Int).collect()),
            );
        }
        let mut action = ValueMap::new();
        action.insert("type".to_owned(), Value::str(MODEL));
        action.insert("name".to_owned(), name);
        action.insert("report_name".to_owned(), report_name);
        action.insert("report_type".to_owned(), report_type);
        // the model the web client prints the records of
        action.insert("model".to_owned(), model);
        action.insert("context".to_owned(), Value::Map(Arc::new(context)));
        action.insert("data".to_owned(), data.unwrap_or(Value::False));
        Ok(Value::Map(Arc::new(action)))
    }

    /// Put the reports in the Print menu of their model: `binding_model_id` becomes the model's
    /// `ir.model` row and `binding_type` `report` (the form's "Offer when printing").
    ///
    /// # Errors
    ///
    /// `Validation` for a report whose model has no `ir.model` row; the errors of `write`.
    #[method]
    #[rpc(effect = write)]
    pub async fn create_action(
        this: &RecordSet<IrActionsReport>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        let ids = row_ids(records)?;
        if ids.is_empty() {
            return Ok(Value::Bool(true));
        }
        records.check_access(tx, Operation::Write).await?;
        tx.flush().await?;
        let rows = tx
            .query(
                "SELECT r.id, r.name, r.model, m.id FROM ir_act_report_xml r \
                 LEFT JOIN ir_model m ON m.model = r.model WHERE r.id = ANY($1) ORDER BY r.id",
                &[&ids],
            )
            .await?;
        let bad = |what: &str| Error::db(format!("unexpected value in ir_act_report_xml.{what}"));
        for row in &rows {
            let id: i64 = row.try_get(0).map_err(|_| bad("id"))?;
            let name: Option<String> = row.try_get(1).map_err(|_| bad("name"))?;
            let model: Option<String> = row.try_get(2).map_err(|_| bad("model"))?;
            let model_id: Option<i64> = row.try_get(3).map_err(|_| bad("model"))?;
            let Some(model_id) = model_id else {
                return Err(Error::validation(format!(
                    "Report '{}' prints model '{}', which has no ir.model row",
                    name.unwrap_or_default(),
                    model.unwrap_or_default()
                )));
            };
            records
                .browse(vec![id])?
                .write(
                    tx,
                    RawVals::new()
                        .set_cell("binding_model_id", Cell::Id(model_id))
                        .set_cell("binding_type", Cell::Str(Arc::from("report"))),
                )
                .await?;
        }
        Ok(Value::Bool(true))
    }

    /// Take the reports out of the Print menu: `binding_model_id` is cleared (the form's "Remove
    /// from the Print menu").
    ///
    /// # Errors
    ///
    /// The errors of `write`.
    #[method]
    #[rpc(effect = write)]
    pub async fn unlink_action(
        this: &RecordSet<IrActionsReport>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        if row_ids(records)?.is_empty() {
            return Ok(Value::Bool(true));
        }
        records
            .write(tx, RawVals::new().set_cell("binding_model_id", Cell::Null))
            .await?;
        Ok(Value::Bool(true))
    }
}

/// The final template check of every report action, as the end of a module operation runs it
/// (`_register_hook`): for a loader that is not a module operation. Reads nothing unless the
/// transaction wrote a report action or a QWeb view while loading.
///
/// # Errors
///
/// `Validation` naming the report and what was refused; a database error.
pub async fn check_loaded_reports(tx: &mut Transaction) -> Result<()> {
    report_templates::check_all(tx).await
}

/// `None` when there is none. Read without the caller's access rules, as the
/// reference reads it with superuser rights: a caller learns only the id.
///
/// # Errors
///
/// A database error.
pub async fn report_from_name(tx: &mut Transaction, report_name: &str) -> Result<Option<i64>> {
    tx.flush().await?;
    let rows = tx
        .query(
            "SELECT id FROM ir_act_report_xml WHERE report_name = $1 ORDER BY id LIMIT 1",
            &[&report_name],
        )
        .await?;
    rows.first()
        .map(|row| {
            row.try_get::<_, i64>(0)
                .map_err(|_| Error::db("unexpected value in ir_act_report_xml.id"))
        })
        .transpose()
}

/// The paper format the report `report` prints on for the company `company`:
/// the action's, else the company's, else the installed fallback [`FALLBACK_PAPERFORMAT`]. The
/// template's own `data-report-*` attributes are applied on top by the renderer.
///
/// # Errors
///
/// `Missing` when the report does not exist; `Validation` naming the report when no tier gives a
/// format (the fallback record was deleted); a database error.
pub async fn paperformat_of(
    tx: &mut Transaction,
    report: i64,
    company: Option<i64>,
) -> Result<i64> {
    tx.flush().await?;
    let (module, name) = FALLBACK_PAPERFORMAT;
    let rows = tx
        .query(
            "SELECT r.name, r.paperformat_id::int8, \
                    (SELECT c.paperformat_id::int8 FROM res_company c WHERE c.id = $2), \
                    (SELECT d.res_id::int8 FROM ir_model_data d JOIN report_paperformat p \
                       ON p.id = d.res_id \
                      WHERE d.module = $3 AND d.name = $4 AND d.model = 'report.paperformat') \
               FROM ir_act_report_xml r WHERE r.id = $1",
            &[&report, &company, &module, &name],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(Error::missing(format!(
            "ir.actions.report {report} does not exist"
        )));
    };
    let bad = |what: &str| Error::db(format!("unexpected value reading the {what} paper format"));
    let own: Option<i64> = row.try_get(1).map_err(|_| bad("report's"))?;
    let of_company: Option<i64> = row.try_get(2).map_err(|_| bad("company's"))?;
    let fallback: Option<i64> = row.try_get(3).map_err(|_| bad("fallback"))?;
    own.or(of_company).or(fallback).ok_or_else(|| {
        let report_label: Option<String> = row.try_get(0).ok().flatten();
        Error::validation(format!(
            "Report '{}' has no paper format: neither the report nor the company names one, and \
             the default format {module}.{name} does not exist",
            report_label.unwrap_or_default()
        ))
    })
}
