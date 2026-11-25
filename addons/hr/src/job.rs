// SPDX-License-Identifier: MIT OR Apache-2.0
//! `hr.job`: a job position of a company or department.

use erp_addon_mail::MailThread;
use erp_sdk::ValueMap;
use erp_sdk::prelude::*;

/// `hr.job`: a named job position, optionally tied to a company and department, archivable.
#[model(
    name = "hr.job",
    description = "Job Position",
    inherit = [MailThread],
    order = "sequence, name, id",
    company_access = direct(field = "company_id", branch = "exact", empty = "allow")
)]
pub struct HrJob {
    /// Job position name.
    #[field(
        char,
        required = true,
        translate = true,
        index = "trigram",
        string = "Job Position"
    )]
    pub name: Option<Arc<str>>,
    /// Display order.
    #[field(integer, default = 10, string = "Sequence")]
    pub sequence: i32,
    /// Company the position belongs to.
    #[field(
        many2one,
        comodel = "res.company",
        ondelete = "restrict",
        index = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Department owning this job position.
    #[field(
        many2one,
        comodel = "hr.department",
        ondelete = "set null",
        index = true,
        tracking = true,
        string = "Department"
    )]
    pub department_id: Option<RecId>,
    /// Default employment type for hires in this role.
    #[field(
        many2one,
        comodel = "hr.employee.type",
        ondelete = "set null",
        string = "Employment Type"
    )]
    pub employee_type_id: Option<RecId>,
    /// Recruiter responsible for hiring into this position.
    #[field(
        many2one,
        comodel = "res.users",
        ondelete = "set null",
        string = "Recruiter"
    )]
    pub recruiter_id: Option<RecId>,
    /// Planned new hires for this job position.
    #[field(integer, default = 1, tracking = true, string = "Target New Hires")]
    pub no_of_recruitment: i32,
    /// Current active employees holding this job position.
    #[field(integer, compute = "_compute_employees", string = "Current Employees")]
    pub no_of_employee: i32,
    /// Current employees plus target new hires.
    #[field(integer, compute = "_compute_employees", string = "Expected Employees")]
    pub expected_employees: i32,
    /// Job summary and responsibilities.
    #[field(html, string = "Job Summary")]
    pub description: Option<Arc<str>>,
    /// Archived positions are hidden from searches.
    #[field(boolean, default = true, string = "Active")]
    pub active: bool,
}

#[methods(HrJob, addon = "hr")]
impl HrJob {
    #[compute(depends = ["no_of_recruitment"])]
    async fn _compute_employees(
        this: &RecordSet<HrJob>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrJob>,
    ) -> Result<()> {
        let targets = this.mapped(tx, HrJob::NO_OF_RECRUITMENT).await?;
        let employees = this.env().records("hr.employee")?;
        for (index, (id, target)) in this.as_records().rec_ids().iter().zip(targets).enumerate() {
            let domain = Domain::condition("job_id", Operator::Eq, id.get())?;
            let count = employees.search_count(tx, &domain, None).await?;
            let current = i32::try_from(count)
                .map_err(|_| Error::programming(format!("employee count {count} exceeds i32")))?;
            out.set(index, HrJob::NO_OF_EMPLOYEE, current)?;
            out.set(
                index,
                HrJob::EXPECTED_EMPLOYEES,
                current.saturating_add(target.max(0)),
            )?;
        }
        Ok(())
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_open_employees(this: &RecordSet<HrJob>, tx: &mut Transaction) -> Result<Value> {
        let id = this.as_records().ensure_one()?.int()?;
        let label = match this.as_records().cell(tx, "name").await? {
            Cell::Str(s) => format!("{s} — Employees"),
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
                    Value::str("job_id"),
                    Value::str("="),
                    Value::Int(id),
                ])]),
            ),
            (
                "context".into(),
                Value::Map(Arc::new(ValueMap::from([(
                    "default_job_id".into(),
                    Value::Int(id),
                )]))),
            ),
        ]))))
    }
}
