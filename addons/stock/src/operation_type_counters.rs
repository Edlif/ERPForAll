// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only overview hook. All aggregates use the reader's ordinary picking access.
use crate::warehouse::StockPickingType;
use erp_sdk::prelude::*;

/// Return a cutoff for each operation type, in record order, using its own company.
pub(crate) async fn local_cutoffs(
    this: &RecordSet<StockPickingType>,
    tx: &mut Transaction,
) -> Result<Vec<(i64, DateTime)>> {
    let companies = this
        .as_records()
        .cells(tx, "company_id")
        .await?
        .into_iter()
        .map(|value| match value {
            Cell::Id(id) => Ok(id),
            _ => Err(Error::validation(
                "Set the company of the operation type before opening its overview.",
            )),
        })
        .collect::<Result<Vec<_>>>()?;
    let unique: std::collections::BTreeSet<i64> = companies.iter().copied().collect();
    let days = erp_addon_base::company_calendar::business_days(
        tx,
        this.env(),
        &unique.into_iter().collect::<Vec<_>>(),
    )
    .await?;
    companies
        .into_iter()
        .map(|id| {
            days.get(&id)
                .map(|day| (id, day.start_utc))
                .ok_or_else(|| Error::missing(format!("Company {id} is unavailable.")))
        })
        .collect()
}

#[methods(StockPickingType, addon = "stock")]
impl StockPickingType {
    #[compute(depends=["company_id.tz","picking_ids.state","picking_ids.scheduled_date","picking_ids.has_deadline_issue","picking_ids.backorder_id"], depends_context=["uid","allowed_company_ids"])]
    async fn _compute_picking_count(
        this: &RecordSet<StockPickingType>,
        tx: &mut Transaction,
        out: &mut Assign<'_, StockPickingType>,
    ) -> Result<()> {
        if this.is_empty() {
            return Ok(());
        }
        let states = Domain::field("state")?;
        let active = states.in_(vec!["assigned", "waiting", "confirmed"]);
        let cutoffs: std::collections::BTreeMap<_, _> =
            local_cutoffs(this, tx).await?.into_iter().collect();
        let scheduled = Domain::field("scheduled_date")?;
        let company = Domain::field("company_id")?;
        let overdue =
            Domain::or(cutoffs.into_iter().map(|(id, cutoff)| {
                Domain::and([company.eq(id), scheduled.lt(cutoff.to_string())])
            }));
        let filters = [
            (StockPickingType::COUNT_PICKING_DRAFT, states.eq("draft")),
            (
                StockPickingType::COUNT_PICKING_WAITING,
                states.in_(vec!["confirmed", "waiting"]),
            ),
            (StockPickingType::COUNT_PICKING_READY, states.eq("assigned")),
            (StockPickingType::COUNT_PICKING, active.clone()),
            (
                StockPickingType::COUNT_PICKING_LATE,
                Domain::and([
                    active.clone(),
                    Domain::or([overdue, Domain::field("has_deadline_issue")?.eq(true)]),
                ]),
            ),
            (
                StockPickingType::COUNT_PICKING_BACKORDERS,
                Domain::and([active, Domain::field("backorder_id")?.ne(false)]),
            ),
        ];
        let model = this.env().records("stock.picking")?;
        for (field, filter) in filters {
            let domain = Domain::and([
                Domain::field("picking_type_id")?.in_(this.as_records().ids()),
                filter,
            ]);
            let grouped = model
                ._read_group(
                    tx,
                    ReadGroup::new(domain)
                        .groupby(["picking_type_id"])
                        .aggregates(["__count"]),
                )
                .await?;
            let mut counts = std::collections::BTreeMap::new();
            for (key, count) in grouped.column(0).iter().zip(grouped.column(1)) {
                let id = key
                    .record_id()
                    .ok_or_else(|| Error::internal("Picking count group has no operation type."))?;
                let GroupValue::Int(count) = count else {
                    return Err(Error::internal(
                        "Picking count aggregate is not an integer.",
                    ));
                };
                let count = i32::try_from(*count).map_err(|e| {
                    Error::validation("Picking count exceeds the supported integer range.")
                        .with_source(e)
                })?;
                counts.insert(id.get(), count);
            }
            for (i, id) in this.as_records().ids().iter().enumerate() {
                // A successful aggregate omits groups with no matching records (STKRPT-174).
                let count = counts.get(id).copied().unwrap_or(0);
                out.set(i, field, count)?;
            }
        }
        Ok(())
    }
}
