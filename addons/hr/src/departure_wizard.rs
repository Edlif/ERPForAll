// SPDX-License-Identifier: MIT OR Apache-2.0
//! `hr.departure.wizard`: transient wizard to register an employee's end of collaboration.

use erp_sdk::prelude::*;
use erp_sdk::{Operation, ValueMap};

/// `hr.departure.wizard`: modal dialog for registering an employee's departure.
#[model(
    name = "hr.departure.wizard",
    kind = "transient",
    description = "End of Collaboration Wizard",
    company_access = parent(edge = "employee_id"),
    transient_max_age_seconds = 7_200,
    transient_max_count = 500
)]
pub struct HrDepartureWizard {
    /// Employee leaving the organization.
    #[field(
        many2one,
        comodel = "hr.employee",
        required = true,
        readonly = true,
        string = "Employee"
    )]
    pub employee_id: Option<RecId>,
    /// Reason for departure.
    #[field(
        many2one,
        comodel = "hr.departure.reason",
        required = true,
        string = "Departure Reason"
    )]
    pub departure_reason_id: Option<RecId>,
    /// Effective last day of collaboration.
    #[field(date, required = true, string = "Departure Date")]
    pub departure_date: Option<Date>,
    /// Detailed notes on the departure.
    #[field(text, string = "Departure Description")]
    pub departure_description: Option<Arc<str>>,
    /// Update the employee's contract end date to match the departure date.
    #[field(boolean, default = true, string = "Set Contract End Date")]
    pub update_contract_end: bool,
    /// Archive the employee profile immediately.
    #[field(boolean, default = true, string = "Archive Employee")]
    pub archive_employee: bool,
}

#[methods(HrDepartureWizard, addon = "hr")]
impl HrDepartureWizard {
    #[method]
    #[rpc(effect = write)]
    async fn action_register_departure(
        this: &RecordSet<HrDepartureWizard>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let wiz = this.as_records();
        wiz.ensure_one()?;

        let Cell::Id(employee_id) = wiz.cell(tx, "employee_id").await? else {
            return Err(Error::validation("An employee is required."));
        };
        let reason_cell = wiz.cell(tx, "departure_reason_id").await?;
        let date_cell = wiz.cell(tx, "departure_date").await?;
        if !matches!(date_cell, Cell::Date(_)) {
            return Err(Error::validation(
                "Set the departure date before registering the departure.",
            ));
        }
        let desc_cell = wiz.cell(tx, "departure_description").await?;
        let update_end = matches!(wiz.cell(tx, "update_contract_end").await?, Cell::Bool(true));
        let archive = matches!(wiz.cell(tx, "archive_employee").await?, Cell::Bool(true));

        let employee = this.env().records("hr.employee")?.browse([employee_id])?;
        employee.check_access(tx, Operation::Write).await?;

        let mut vals = RawVals::new()
            .set_cell("departure_reason_id", reason_cell)
            .set_cell("departure_date", date_cell.clone())
            .set_cell("departure_description", desc_cell);
        if update_end {
            vals = vals.set_cell("contract_date_end", date_cell);
        }
        if archive {
            vals = vals.set_cell("active", Cell::Bool(false));
        }
        employee.write(tx, vals).await?;

        Ok(Value::Map(Arc::new(ValueMap::from([
            ("type".into(), Value::str("ir.actions.client")),
            ("tag".into(), Value::str("display_notification")),
            (
                "params".into(),
                Value::Map(Arc::new(ValueMap::from([
                    ("title".into(), Value::str("Departure Registered")),
                    (
                        "message".into(),
                        Value::str("Employee departure details have been saved."),
                    ),
                    ("type".into(), Value::str("success")),
                    (
                        "next".into(),
                        Value::Map(Arc::new(ValueMap::from([(
                            "type".into(),
                            Value::str("ir.actions.act_window_close"),
                        )]))),
                    ),
                ]))),
            ),
        ]))))
    }
}
