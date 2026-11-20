// SPDX-License-Identifier: MIT OR Apache-2.0
//! Definition amounts with caller-scoped historical source admission.
use erp_addon_reporting::{
    definition::{DefinitionProvider, ReportDef, check_page_parent},
    line::{CellKind, DrillTarget, ExpandPage, Expansion, LineId},
    options::Options,
    provider::{ProviderSpec, ReportCx, ReportProvider},
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::{Value as Json, json};
use erp_sdk::{BoxFuture, ErrorKind};

pub(crate) struct BalanceSheet;
const SPEC: ProviderSpec = ProviderSpec::new("account_reporting.balance_sheet", 1);
async fn admit(cx: &mut ReportCx<'_>, options: &Options) -> Result<()> {
    let items = cx.env().records("account.move.line")?;
    for period in &options.column_groups {
        let (y, m, d) = period.to.ymd();
        let source = json!([
            ["company_id", "in", options.companies],
            ["parent_state", "=", "posted"],
            ["journal_id.journal_group_id", "=", false],
            ["date", "<=", format!("{y:04}-{m:02}-{d:02}")],
            ["account_id", "!=", false]
        ]);
        let off = Domain::from_json(&source)?
            & Domain::from_json(&json!([["account_id.account_type", "=", "off_balance"]]))?;
        // Both sides are searches under the reader's ACLs/record rules. Never use any!
        // (the bypassing operator), nor SQL over globally visible counterpart rows.
        let mixed = Domain::from_json(&source)?
            & Domain::from_json(&json!([
                ["account_id.account_type", "!=", "off_balance"],
                ["move_id", "any", [["line_ids", "any", off.to_json()?]]]
            ]))?;
        if items.search_count(cx.tx(), &mixed, Some(1)).await? != 0 {
            return Err(Error::validation(
                "Balance Sheet cannot include a posted entry mixing ordinary and off-balance accounts. Correct the ledger classification before continuing.",
            ));
        }
    }
    Ok(())
}
impl ReportProvider for BalanceSheet {
    fn spec(&self) -> &ProviderSpec {
        &SPEC
    }
    fn init_options<'a>(
        &'a self,
        cx: &'a mut ReportCx<'_>,
        report: &'a ReportDef,
        options: &'a mut Options,
        strict: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            DefinitionProvider
                .init_options(cx, report, options, strict)
                .await?;
            if report
                .columns
                .iter()
                .any(|c| matches!(c.kind, CellKind::Date | CellKind::Text))
            {
                return Err(Error::validation(
                    "Balance Sheet definition columns must be numeric.",
                ));
            }
            let mut stale = Vec::new();
            for key in options.loaded_pages.keys() {
                if let Err(error) =
                    check_page_parent(report, &LineId::parse(key)?, cx.bounds(), |parent| {
                        options.unfold_all || options.unfolded.contains(parent)
                    })
                {
                    // Match the definition provider: obsolete previous options are dropped;
                    // strict execution options and non-validation failures remain errors.
                    if strict || error.kind() != ErrorKind::Validation {
                        return Err(error);
                    }
                    stale.push(key.clone());
                }
            }
            options.loaded_pages.retain(|key, _| !stale.contains(key));
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
            admit(cx, options).await?;
            let result = DefinitionProvider.lines(cx, report, options).await?;
            admit(cx, options).await?;
            Ok(result)
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
            admit(cx, options).await?;
            let result = DefinitionProvider
                .expand(cx, report, options, line, page)
                .await?;
            admit(cx, options).await?;
            Ok(result)
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
            admit(cx, options).await?;
            let result = DefinitionProvider
                .drill(cx, report, options, line, target)
                .await?;
            admit(cx, options).await?;
            Ok(result)
        })
    }
}

erp_addon_reporting::provider_declarations!(BalanceSheet);
