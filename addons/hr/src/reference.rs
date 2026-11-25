// SPDX-License-Identifier: MIT OR Apache-2.0
//! HR reference and configuration models:
//! - `hr.employee.category` (Tags)
//! - `hr.employee.type` (Employee / Employment Types)
//! - `hr.work.location` (Work Locations)
//! - `hr.work.schedule` (Working Schedules)
//! - `hr.departure.reason` (Departure Reasons)

use erp_sdk::ValueMap;
use erp_sdk::prelude::*;

/// `hr.employee.category`: colored tags attached to employees.
#[model(
    name = "hr.employee.category",
    description = "Employee Tag",
    order = "name, id",
    constraints = [(
        "_name_uniq",
        "UNIQUE (name)",
        "Employee tag name must be unique."
    )]
)]
pub struct HrEmployeeCategory {
    /// Tag label.
    #[field(char, required = true, string = "Tag Name")]
    pub name: Option<Arc<str>>,
    /// Palette index (0..11).
    #[field(integer, default = 0, string = "Color Index")]
    pub color: i32,
}

/// `hr.employee.type`: classification of employment contracts (Employee, Contractor, Intern, etc.).
#[model(
    name = "hr.employee.type",
    description = "Employee Type",
    order = "sequence, name, id",
    company_access = direct(field = "company_id", branch = "exact", empty = "allow")
)]
pub struct HrEmployeeType {
    /// Display label.
    #[field(char, required = true, translate = true, string = "Employee Type")]
    pub name: Option<Arc<str>>,
    /// Short stable code.
    #[field(char, string = "Code")]
    pub code: Option<Arc<str>>,
    /// Ordering sequence.
    #[field(integer, default = 10, string = "Sequence")]
    pub sequence: i32,
    /// Optional country scope.
    #[field(
        many2one,
        comodel = "res.country",
        ondelete = "set null",
        string = "Country"
    )]
    pub country_id: Option<RecId>,
    /// Optional company scope.
    #[field(
        many2one,
        comodel = "res.company",
        ondelete = "restrict",
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Active employees with this type.
    #[field(integer, compute = "_compute_employees_count", string = "Employees")]
    pub employees_count: i32,
}

#[methods(HrEmployeeType, addon = "hr")]
impl HrEmployeeType {
    #[compute(depends = [])]
    async fn _compute_employees_count(
        this: &RecordSet<HrEmployeeType>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrEmployeeType>,
    ) -> Result<()> {
        let employees = this.env().records("hr.employee")?;
        for (index, id) in this.as_records().rec_ids().iter().enumerate() {
            let domain = Domain::condition("employee_type_id", Operator::Eq, id.get())?;
            let count = employees.search_count(tx, &domain, None).await?;
            let count = i32::try_from(count)
                .map_err(|_| Error::programming(format!("employee count {count} exceeds i32")))?;
            out.set(index, HrEmployeeType::EMPLOYEES_COUNT, count)?;
        }
        Ok(())
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_open_employees(
        this: &RecordSet<HrEmployeeType>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let id = this.as_records().ensure_one()?.int()?;
        let label = match this.as_records().cell(tx, "name").await? {
            Cell::Str(s) => s.to_string(),
            _ => "Employees".to_string(),
        };
        Ok(Value::Map(Arc::new(ValueMap::from([
            ("type".into(), Value::str("ir.actions.act_window")),
            ("name".into(), Value::str(label)),
            ("res_model".into(), Value::str("hr.employee")),
            (
                "views".into(),
                Value::list([
                    Value::list([Value::Bool(false), Value::str("kanban")]),
                    Value::list([Value::Bool(false), Value::str("list")]),
                    Value::list([Value::Bool(false), Value::str("form")]),
                ]),
            ),
            (
                "domain".into(),
                Value::list([Value::list([
                    Value::str("employee_type_id"),
                    Value::str("="),
                    Value::Int(id),
                ])]),
            ),
            (
                "context".into(),
                Value::Map(Arc::new(ValueMap::from([(
                    "default_employee_type_id".into(),
                    Value::Int(id),
                )]))),
            ),
        ]))))
    }
}

/// `hr.work.location`: office, home, or external site where an employee works.
#[model(
    name = "hr.work.location",
    description = "Work Location",
    order = "name, id",
    company_access = direct(field = "company_id", branch = "exact", empty = "allow")
)]
pub struct HrWorkLocation {
    /// Location name.
    #[field(char, required = true, string = "Work Location")]
    pub name: Option<Arc<str>>,
    /// Archivable flag.
    #[field(boolean, default = true, string = "Active")]
    pub active: bool,
    /// Company owning the location.
    #[field(
        many2one,
        comodel = "res.company",
        ondelete = "restrict",
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Physical work address.
    #[field(
        many2one,
        comodel = "res.partner",
        ondelete = "set null",
        string = "Work Address"
    )]
    pub address_id: Option<RecId>,
    /// Category of location.
    #[field(
        selection,
        selection = [("office", "Office"), ("home", "Home"), ("other", "Other")],
        default = "office",
        required = true,
        string = "Location Type"
    )]
    pub location_type: Option<Arc<str>>,
}

/// `hr.work.schedule`: standard working hours schedule assigned to employees and contracts.
#[model(
    name = "hr.work.schedule",
    description = "Working Schedule",
    order = "name, id",
    company_access = direct(field = "company_id", branch = "exact", empty = "allow")
)]
pub struct HrWorkSchedule {
    /// Schedule name (e.g. "Standard 40 hours/week").
    #[field(char, required = true, string = "Working Schedule")]
    pub name: Option<Arc<str>>,
    /// Archivable flag.
    #[field(boolean, default = true, string = "Active")]
    pub active: bool,
    /// Optional company scope.
    #[field(
        many2one,
        comodel = "res.company",
        ondelete = "restrict",
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Average hours per working day.
    #[field(float, default = 8.0, string = "Average Hours per Day")]
    pub hours_per_day: f64,
    /// Total hours per week.
    #[field(float, default = 40.0, string = "Hours per Week")]
    pub hours_per_week: f64,
    /// Timezone identifier.
    #[field(char, default = "Asia/Jakarta", string = "Timezone")]
    pub tz: Option<Arc<str>>,
}

/// `hr.departure.reason`: reason recorded when ending collaboration with an employee.
#[model(
    name = "hr.departure.reason",
    description = "Departure Reason",
    order = "sequence, name, id"
)]
pub struct HrDepartureReason {
    /// Reason label.
    #[field(char, required = true, translate = true, string = "Reason")]
    pub name: Option<Arc<str>>,
    /// Display order.
    #[field(integer, default = 10, string = "Sequence")]
    pub sequence: i32,
    /// Structured reason category.
    #[field(
        selection,
        selection = [
            ("fired", "Fired"),
            ("resigned", "Resigned"),
            ("retired", "Retired"),
            ("other", "Other"),
        ],
        default = "resigned",
        required = true,
        string = "Reason Code"
    )]
    pub reason_code: Option<Arc<str>>,
}
