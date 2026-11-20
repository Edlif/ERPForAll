// SPDX-License-Identifier: MIT OR Apache-2.0
//! Historical company-currency residual admission for the Partner Ledger.
use erp_addon_reporting::{
    definition::{DefinitionProvider, ReportDef},
    line::{DrillTarget, ExpandPage, Expansion, LineId},
    options::Options,
    provider::{ProviderSpec, ReportCx, ReportProvider},
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, FieldOp, SearchOptions};

pub(crate) struct PartnerLedger;
const SPEC: ProviderSpec = ProviderSpec::new("account_reporting.partner_ledger", 1);

async fn scoped(cx: &mut ReportCx<'_>, report: &ReportDef, options: &Options) -> Result<ReportDef> {
    if options.provider.get("unreconciled").and_then(Json::as_bool) != Some(true) {
        return Ok(report.clone());
    }
    let [company] = options.companies.as_slice() else {
        return Err(Error::validation(
            "Select one company for historical reconciliation.",
        ));
    };
    if options.column_groups.len() != 1 {
        return Err(Error::validation(
            "Use a single period for historical reconciliation.",
        ));
    }
    let cutoff = options.period.to;
    let marker = cx
        .tx()
        .query(
            "SELECT safe_from FROM account_reconcile_coverage WHERE name='partial_matches'",
            &[],
        )
        .await?;
    let [row] = marker.as_slice() else {
        return Err(Error::validation(
            "Reconciliation history coverage has not been established.",
        ));
    };
    let floor: Option<Date> = row
        .try_get(0)
        .map_err(|e| Error::db(format!("Reading historical coverage: {e}")))?;
    if let Some(floor) = floor.filter(|start| cutoff < *start) {
        let (y, m, d) = floor.ymd();
        return Err(Error::validation(format!(
            "Reconciliation history is available from {y:04}-{m:02}-{d:02}."
        )));
    }
    let env = cx.env().clone();
    for field in [
        "amount_residual",
        "balance",
        "company_id",
        "currency_id",
        "account_id",
        "partner_id",
        "date",
    ] {
        if !cx
            .tx()
            .has_field_access(&env, "account.move.line", field, FieldOp::Read)
            .await?
        {
            return Err(Error::access(
                "Reading historical residuals requires access to journal item reconciliation fields.",
            ));
        }
    }
    let (y, m, d) = cutoff.ymd();
    let source = Domain::from_json(&json!([
        ["company_id", "=", company],
        ["parent_state", "=", "posted"],
        ["date", "<=", format!("{y:04}-{m:02}-{d:02}")],
        ["journal_id.journal_group_id", "=", false],
        [
            "account_id.account_type",
            "in",
            ["asset_receivable", "liability_payable"]
        ]
    ]))?;
    let limit = cx.bounds().max_filter_source_rows;
    let ids = env
        .records("account.move.line")?
        .search_with(
            cx.tx(),
            &source,
            &SearchOptions::new().limit(limit as u64 + 1),
        )
        .await?
        .ids();
    if ids.len() > limit {
        return Err(Error::validation(
            "The journal items exceed reporting.max_filter_source_rows. Choose an earlier cutoff or ask your administrator to review the limit.",
        ));
    }
    let rows = cx
        .tx()
        .query(
            include_str!("partner_ledger_residual.sql"),
            &[&ids, company, &cutoff],
        )
        .await?;
    if rows.len() != ids.len() {
        return Err(Error::validation(
            "The journal item scope changed. Refresh the Partner Ledger.",
        ));
    }
    let mut eligible = Vec::<i64>::new();
    for row in rows {
        let valid: bool = row
            .try_get(1)
            .map_err(|e| Error::db(format!("Reading reconciliation evidence: {e}")))?;
        if !valid {
            return Err(Error::validation(
                "Historical residuals cannot be established for this cutoff. Check currency, dated matching evidence, and overlapping re-matches.",
            ));
        }
        let open: bool = row
            .try_get(2)
            .map_err(|e| Error::db(format!("Reading historical residual: {e}")))?;
        if open {
            eligible.push(
                row.try_get(0)
                    .map_err(|e| Error::db(format!("Reading historical journal item: {e}")))?,
            );
        }
    }
    report.restricted_to(
        "account.move.line",
        &Domain::from_json(&json!([["id", "in", eligible]]))?,
    )
}

impl ReportProvider for PartnerLedger {
    fn spec(&self) -> &ProviderSpec {
        &SPEC
    }
    fn init_options<'a>(
        &'a self,
        _cx: &'a mut ReportCx<'_>,
        _report: &'a ReportDef,
        options: &'a mut Options,
        _strict: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for (key, value) in &options.provider {
                if key != "unreconciled" || !value.is_boolean() {
                    return Err(Error::validation(format!(
                        "Invalid Partner Ledger option {key}. Use unreconciled with a boolean value."
                    )));
                }
            }
            options
                .provider
                .entry("unreconciled")
                .or_insert(Json::Bool(false));
            Ok(())
        })
    }
    fn lines<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move {
            let def = scoped(cx, report, options).await?;
            DefinitionProvider.lines(cx, &def, options).await
        })
    }
    fn expand<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        page: ExpandPage,
    ) -> BoxFuture<'a, Result<Expansion>> {
        Box::pin(async move {
            let def = scoped(cx, report, options).await?;
            DefinitionProvider
                .expand(cx, &def, options, line, page)
                .await
        })
    }
    fn drill<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a Options,
        line: &'a LineId,
        target: &'a DrillTarget,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(async move {
            let def = scoped(cx, report, options).await?;
            DefinitionProvider
                .drill(cx, &def, options, line, target)
                .await
        })
    }
}

erp_addon_reporting::provider_declarations!(PartnerLedger);
