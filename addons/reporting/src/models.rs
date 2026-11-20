// SPDX-License-Identifier: MIT OR Apache-2.0
//! The definition models (design §7) and the RPC methods of `reporting.report` (design §9).
//!
//! Every save of a definition record checks the whole report again ([`crate::definition::load`],
//! not strict: a line or label the next record of a data file adds is not refused yet; the RPC
//! calls load strictly).
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{self, Value as Json};
use erp_sdk::{FieldOp, Load, Operation};

use crate::runtime;

/// `reporting.report`: one report, its provider and its filter bar.
#[model(
    name = "reporting.report",
    description = "Dynamic Report",
    order = "name, id"
)]
pub struct ReportingReport {
    /// Report title.
    #[field(char, required = true, translate = true)]
    pub name: Option<Arc<str>>,
    /// The registered provider that answers its lines.
    #[field(char, required = true, default = "reporting.definition")]
    pub provider: Option<Arc<str>>,
    /// Lines (definition provider).
    #[field(
        one2many,
        comodel = "reporting.report.line",
        inverse_name = "report_id",
        string = "Lines"
    )]
    pub line_ids: Vec<RecId>,
    /// Columns.
    #[field(
        one2many,
        comodel = "reporting.report.column",
        inverse_name = "report_id",
        string = "Columns"
    )]
    pub column_ids: Vec<RecId>,
    /// A date range or one date ("as of").
    #[field(selection, selection = [("range", "Date Range"), ("single", "Single Date")], default = "range", required = true)]
    pub date_mode: Option<Arc<str>>,
    /// The period a new view opens with.
    #[field(
        selection,
        selection = [("today", "Today"), ("this_month", "This Month"), ("last_month", "Last Month"),
                     ("this_quarter", "This Quarter"), ("last_quarter", "Last Quarter"),
                     ("this_year", "This Year"), ("last_year", "Last Year")],
        default = "this_month",
        required = true,
        string = "Default Period"
    )]
    pub date_default: Option<Arc<str>>,
    /// The source stores current balances only. A historical cutoff would mislabel their values.
    #[field(boolean, default = false)]
    pub current_only: bool,
    /// Comparison periods are offered.
    #[field(boolean, default = true)]
    pub comparison: bool,
    /// Several companies may be selected.
    #[field(boolean, default = true)]
    pub multi_company: bool,
    /// "Unfold all" is offered.
    #[field(boolean, default = true)]
    pub unfold_all: bool,
    /// When zero lines may be hidden.
    #[field(
        selection,
        selection = [("never", "Never"), ("optional", "Optional"), ("by_default", "By Default")],
        default = "optional",
        required = true
    )]
    pub hide_zero: Option<Arc<str>>,
    /// The search bar is shown.
    #[field(boolean, default = true)]
    pub search_bar: bool,
    /// A total line follows the children of an unfolded line.
    #[field(boolean, default = false)]
    pub totals_below: bool,
    /// Children loaded per unfold page (bounded by `reporting.max_load_more_limit`).
    #[field(integer, default = 500, required = true)]
    pub load_more_limit: i32,
    /// Archived reports are hidden.
    #[field(boolean, default = true)]
    pub active: bool,
}

/// `reporting.report.line`: one line of a definition report.
#[model(
    name = "reporting.report.line",
    description = "Dynamic Report Line",
    order = "sequence, id",
    constraints = [("_code_unique", "UNIQUE(report_id, code)", "A line code is used once per report.")]
)]
pub struct ReportingReportLine {
    /// The report.
    #[field(
        many2one,
        comodel = "reporting.report",
        required = true,
        ondelete = "cascade",
        index = true
    )]
    pub report_id: Option<RecId>,
    /// Order among its siblings.
    #[field(integer, default = 10)]
    pub sequence: i32,
    /// Shown name.
    #[field(char, required = true, translate = true)]
    pub name: Option<Arc<str>>,
    /// The code formulas name it by (`A-Z`, `0-9`, `_`).
    #[field(char, required = true)]
    pub code: Option<Arc<str>>,
    /// The parent line.
    #[field(
        many2one,
        comodel = "reporting.report.line",
        ondelete = "cascade",
        index = true
    )]
    pub parent_id: Option<RecId>,
    /// Fields its records unfold by, comma-separated, outermost first.
    #[field(char, string = "Group By")]
    pub groupby: Option<Arc<str>>,
    /// Whether it folds.
    #[field(
        selection,
        selection = [("foldable", "Foldable"), ("always_unfolded", "Always Unfolded"), ("never_unfolded", "Never Unfolded")],
        default = "foldable",
        required = true
    )]
    pub foldable: Option<Arc<str>>,
    /// Hidden when it and its children are all zero.
    #[field(boolean, default = false)]
    pub hide_if_zero: bool,
    /// Left out (and named) for a caller who may not read its records.
    #[field(boolean, default = false)]
    pub optional: bool,
    /// Its expressions.
    #[field(
        one2many,
        comodel = "reporting.report.expression",
        inverse_name = "line_id",
        string = "Expressions"
    )]
    pub expression_ids: Vec<RecId>,
}

/// `reporting.report.column`: one column (an expression label shown per period).
#[model(
    name = "reporting.report.column",
    description = "Dynamic Report Column",
    order = "sequence, id",
    constraints = [("_label_unique", "UNIQUE(report_id, label)", "A column label is used once per report.")]
)]
pub struct ReportingReportColumn {
    /// The report.
    #[field(
        many2one,
        comodel = "reporting.report",
        required = true,
        ondelete = "cascade",
        index = true
    )]
    pub report_id: Option<RecId>,
    /// Order.
    #[field(integer, default = 10)]
    pub sequence: i32,
    /// Header text.
    #[field(char, required = true, translate = true)]
    pub name: Option<Arc<str>>,
    /// The expression label its cells show.
    #[field(char, required = true)]
    pub label: Option<Arc<str>>,
    /// Cell kind.
    #[field(
        selection,
        selection = [("monetary", "Monetary"), ("decimal", "Decimal"), ("integer", "Integer"), ("percent", "Percent"), ("date", "Date"), ("text", "Text")],
        default = "decimal",
        required = true
    )]
    pub kind: Option<Arc<str>>,
    /// Decimals shown (monetary cells use the currency's).
    #[field(integer, default = 2)]
    pub scale: i32,
    /// Lines may be sorted by it.
    #[field(boolean, default = false)]
    pub sortable: bool,
    /// Zero shows as nothing.
    #[field(boolean, default = false)]
    pub blank_if_zero: bool,
    /// `none`: shown only on lines grouped by the rollup field (no sum across units).
    #[field(selection, selection = [("sum", "Sum"), ("none", "None")], default = "sum", required = true)]
    pub rollup: Option<Arc<str>>,
    /// The group-by field a `none` column is shown on.
    #[field(char)]
    pub rollup_field: Option<Arc<str>>,
    /// Group spec (external ids, as `has_groups`): the column and its expressions exist only for
    /// its members; empty: for everyone who reads the report.
    #[field(char)]
    pub groups: Option<Arc<str>>,
}

/// `reporting.report.expression`: one value of a line.
#[model(
    name = "reporting.report.expression",
    description = "Dynamic Report Expression",
    order = "id",
    constraints = [("_label_unique", "UNIQUE(line_id, label)", "An expression label is used once per line.")]
)]
pub struct ReportingReportExpression {
    /// The line.
    #[field(
        many2one,
        comodel = "reporting.report.line",
        required = true,
        ondelete = "cascade",
        index = true
    )]
    pub line_id: Option<RecId>,
    /// The label columns and formulas name it by.
    #[field(char, required = true)]
    pub label: Option<Arc<str>>,
    /// How it is computed.
    #[field(selection, selection = [("aggregate", "Aggregate"), ("formula", "Formula"), ("native", "Native")], default = "aggregate", required = true)]
    pub engine: Option<Arc<str>>,
    /// Aggregate: the model read.
    #[field(char)]
    pub model: Option<Arc<str>>,
    /// Aggregate: a JSON domain on the model.
    #[field(text)]
    pub domain: Option<Arc<str>>,
    /// Aggregate: the number summed (empty: the record count).
    #[field(char)]
    pub field: Option<Arc<str>>,
    /// Formula: arithmetic over labels, `CODE.label` and `sum_children`.
    #[field(text)]
    pub formula: Option<Arc<str>>,
    /// Native: the registered function that computes it (it reads the aggregate fields' model,
    /// domain and date).
    #[field(char)]
    pub function: Option<Arc<str>>,
    /// Aggregate: the date or datetime the period applies to (empty: none).
    #[field(char)]
    pub date_field: Option<Arc<str>>,
    /// Aggregate: within the period, through its end, or strictly before its start.
    #[field(
        selection,
        selection = [("strict_range", "Within the Period"), ("from_beginning", "From the Beginning"), ("before_period", "Before the Period")],
        default = "strict_range",
        required = true
    )]
    pub date_scope: Option<Arc<str>>,
    /// Formula: a division by zero is an error or an empty cell.
    #[field(selection, selection = [("error", "Error"), ("empty", "Empty")], default = "error", required = true)]
    pub zero_division: Option<Arc<str>>,
}

async fn ids_of(records: &Records, tx: &mut Transaction, field: &str) -> Result<Vec<i64>> {
    let mut out = Vec::new();
    for row in records.read_with(tx, &[field], Load::Raw).await? {
        if let Some(Value::Int(id)) = row.get(field) {
            out.push(*id);
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// The JSON form of an RPC argument.
fn json(value: &Value) -> Result<Json> {
    let invalid = || Error::validation("A report option is not a JSON value.");
    Ok(match value {
        // JSON-2 binds null as the ORM sentinel and literal booleans as Value::Bool.
        Value::False => Json::Null,
        Value::Bool(v) => Json::Bool(*v),
        Value::Str(v) => Json::String(v.to_string()),
        Value::Int(v) => Json::from(*v),
        Value::Float(v) if v.is_finite() => Json::from(*v),
        Value::Json(v) => (**v).clone(),
        Value::Map(v) => Json::Object(
            v.iter()
                .map(|(k, v)| Ok((k.to_string(), json(v)?)))
                .collect::<Result<serde_json::Map<String, Json>>>()?,
        ),
        Value::List(v) => Json::Array(v.iter().map(json).collect::<Result<_>>()?),
        _ => return Err(invalid()),
    })
}

fn out(answer: Json) -> Value {
    Value::Json(Arc::new(answer))
}

#[methods(ReportingReport, addon = "reporting")]
impl ReportingReport {
    /// The whole definition is checked on every save.
    #[constrains(
        "provider",
        "load_more_limit",
        "date_mode",
        "date_default",
        "current_only",
        "comparison",
        "hide_zero"
    )]
    async fn _check_definition(
        this: &RecordSet<ReportingReport>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let ids: Vec<i64> = this
            .as_records()
            .rec_ids()
            .iter()
            .map(|r| r.get())
            .collect();
        runtime::check_saved(this.env(), tx, &ids).await
    }

    /// Open the saved definition in the report viewer as the current user.
    #[method]
    #[rpc(effect = read)]
    async fn action_open_report(
        this: &RecordSet<ReportingReport>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        let id = records.ensure_one()?.int()?;
        let names = records.values(tx, "name").await?;
        let name = names
            .first()
            .ok_or_else(|| Error::missing("Report definition no longer exists."))?;
        let Value::Str(name) = name else {
            return Err(Error::programming("The report name must be text."));
        };
        let name = name.trim();
        let title = &name[..name.floor_char_boundary(erp_addon_web::MAX_TITLE_BYTES)];
        Ok(out(serde_json::json!({
            "type": "ir.actions.client",
            "tag": crate::client_action::TAG,
            "name": if title.is_empty() { "Dynamic Report" } else { title },
            "params": { "report": id }
        })))
    }

    /// The options of report `report`, carrying `previous` over (SPEC-DYNREP, design §5).
    ///
    /// # Errors
    /// As [`runtime::report_options`].
    #[method]
    #[rpc(effect = read, model)]
    async fn erp_report_options(
        this: &RecordSet<ReportingReport>,
        tx: &mut Transaction,
        report: i64,
        previous: Value,
    ) -> Result<Value> {
        // Only the top-level previous argument accepts the first-opening false shorthand.
        let previous = match json(&previous)? {
            Json::Bool(false) => Json::Null,
            previous => previous,
        };
        Ok(out(runtime::report_options(
            this.env(),
            tx,
            report,
            &previous,
        )
        .await?))
    }

    /// The lines for `options`.
    ///
    /// # Errors
    /// As [`runtime::report_lines`].
    #[method]
    #[rpc(effect = read, model)]
    async fn erp_report_lines(
        this: &RecordSet<ReportingReport>,
        tx: &mut Transaction,
        options: Value,
    ) -> Result<Value> {
        Ok(out(
            runtime::report_lines(this.env(), tx, &json(&options)?).await?
        ))
    }

    /// One page of the children of `line_id` from `offset`.
    ///
    /// # Errors
    /// As [`runtime::report_expand`].
    #[method]
    #[rpc(effect = read, model)]
    async fn erp_report_expand(
        this: &RecordSet<ReportingReport>,
        tx: &mut Transaction,
        options: Value,
        line_id: String,
        offset: i64,
    ) -> Result<Value> {
        Ok(out(runtime::report_expand(
            this.env(),
            tx,
            &json(&options)?,
            &line_id,
            offset,
        )
        .await?))
    }

    /// The action behind a caret item or a cell of `line_id`.
    ///
    /// # Errors
    /// As [`runtime::report_drill`].
    #[method]
    #[rpc(effect = read, model)]
    async fn erp_report_drill(
        this: &RecordSet<ReportingReport>,
        tx: &mut Transaction,
        options: Value,
        line_id: String,
        target: Value,
    ) -> Result<Value> {
        Ok(out(runtime::report_drill(
            this.env(),
            tx,
            &json(&options)?,
            &line_id,
            &json(&target)?,
        )
        .await?))
    }
}

// Concurrent edits to different lines share a row version per report under REPEATABLE READ.
// Callers supply all affected IDs in ascending, unique order, including both sides of a transfer.
async fn version_line_owners(tx: &mut Transaction, reports: &[i64]) -> Result<()> {
    for report in reports {
        tx.execute(
            "UPDATE reporting_report SET write_date = write_date WHERE id = $1",
            &[report],
        )
        .await?;
    }
    Ok(())
}

#[methods(ReportingReportLine, addon = "reporting")]
impl ReportingReportLine {
    /// The constraint sees the new owner after a write. Also check every report the lines left.
    #[method(override)]
    async fn write(
        this: &RecordSet<ReportingReportLine>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let lines = this.as_records();
        if !vals.contains("report_id") {
            return sup.call(tx, lines, (vals,)).await;
        }
        lines.check_access(tx, Operation::Write).await?;
        if !tx
            .has_field_access(this.env(), lines.spec().name, "report_id", FieldOp::Write)
            .await?
        {
            return Err(Error::access(
                "Field reporting.report.line.report_id cannot be written.",
            ));
        }
        let mut sources = ids_of(lines, tx, "report_id").await?;
        let mut owners = sources.clone();
        if let Some(Cell::Id(destination)) = vals.get("report_id") {
            sources.retain(|id| id != destination);
            owners.push(*destination);
        }
        if sources.is_empty() {
            return sup.call(tx, lines, (vals,)).await;
        }
        owners.sort_unstable();
        owners.dedup();
        this.env()
            .records("reporting.report")?
            .browse(owners.iter().copied())?
            .check_access(tx, Operation::Read)
            .await?;
        version_line_owners(tx, &owners).await?;
        sup.call(tx, lines, (vals,)).await?;
        runtime::check_saved(this.env(), tx, &sources).await
    }

    /// The report is checked again.
    #[constrains("report_id", "code", "parent_id", "sequence", "groupby", "foldable")]
    async fn _check_definition(
        this: &RecordSet<ReportingReportLine>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let reports = ids_of(this.as_records(), tx, "report_id").await?;
        version_line_owners(tx, &reports).await?;
        runtime::check_saved(this.env(), tx, &reports).await
    }
}

#[methods(ReportingReportColumn, addon = "reporting")]
impl ReportingReportColumn {
    /// The report is checked again.
    #[constrains("report_id", "label", "kind", "scale", "rollup", "rollup_field")]
    async fn _check_definition(
        this: &RecordSet<ReportingReportColumn>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let reports = ids_of(this.as_records(), tx, "report_id").await?;
        runtime::check_saved(this.env(), tx, &reports).await
    }
}

#[methods(ReportingReportExpression, addon = "reporting")]
impl ReportingReportExpression {
    /// The report of its line is checked again.
    #[constrains(
        "line_id",
        "label",
        "engine",
        "model",
        "domain",
        "field",
        "formula",
        "date_field"
    )]
    async fn _check_definition(
        this: &RecordSet<ReportingReportExpression>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let lines = ids_of(this.as_records(), tx, "line_id").await?;
        let lines = this.env().records("reporting.report.line")?.browse(lines)?;
        let reports = ids_of(&lines, tx, "report_id").await?;
        runtime::check_saved(this.env(), tx, &reports).await
    }
}
