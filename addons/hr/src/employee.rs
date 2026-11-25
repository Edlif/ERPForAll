// SPDX-License-Identifier: MIT OR Apache-2.0
//! `hr.employee`: full employee profile, organizational hierarchy, contract summary,
//! personal details, end-of-collaboration workflow, and org chart RPCs.

use std::collections::BTreeSet;

use erp_addon_base::ImageMixin;
use erp_addon_mail::{MailActivityMixin, MailThread};
use erp_sdk::prelude::*;
use erp_sdk::{Operation, ValueMap};

use crate::counts::{as_field, count_by};
use crate::{HIERARCHY_DEPTH_KEY, ORG_CHART_ROWS_KEY, count_parameter};

fn too_deep(depth: usize) -> Error {
    Error::validation(format!(
        "The manager chain is deeper than {HIERARCHY_DEPTH_KEY} ({depth}) levels."
    ))
}

/// Random codes tried before giving up on a company whose codes are nearly all taken.
const BADGE_ATTEMPTS: usize = 8;

/// A 12-digit badge code: `041` and nine digits from a randomly keyed hash of the clock, so a
/// code cannot be derived from the record id.
fn random_badge() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    hasher.write_u128(nanos);
    format!("041{:09}", hasher.finish() % 1_000_000_000)
}

fn recursive_managers() -> Error {
    Error::validation("You cannot create a recursive manager hierarchy.")
}

/// `hr.employee`: a person employed by one company, with full HR profile, org chart,
/// contract/payroll summary, resume & skills, personal data (protected by `hr.group_hr_user`),
/// and chatter/activities.
#[model(
    name = "hr.employee",
    description = "Employee",
    inherit = [ImageMixin, MailThread, MailActivityMixin],
    order = "name, id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        (
            "_user_company_uniq",
            "UNIQUE (company_id, user_id)",
            "This user is already linked to another employee of this company."
        ),
        (
            "_barcode_company_uniq",
            "UNIQUE (company_id, barcode)",
            "This Badge ID is already used by another employee of this company."
        ),
    ]
)]
pub struct HrEmployee {
    // ── Identity & Work Contact (Public Directory) ──────────────────────────
    /// Employee full name.
    #[field(
        char,
        required = true,
        index = "trigram",
        tracking = true,
        string = "Employee Name"
    )]
    pub name: Option<Arc<str>>,
    /// Company that employs the person.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict",
        index = true,
        tracking = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Company currency (for monetary wage / cost fields).
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Archived employees are hidden from active searches.
    #[field(boolean, default = true, tracking = true, string = "Active")]
    pub active: bool,
    /// Work email address.
    #[field(char, tracking = true, string = "Work Email")]
    pub work_email: Option<Arc<str>>,
    /// Work landline phone.
    #[field(char, tracking = true, string = "Work Phone")]
    pub work_phone: Option<Arc<str>>,
    /// Work mobile phone.
    #[field(char, string = "Work Mobile")]
    pub mobile_phone: Option<Arc<str>>,
    /// Colored tags attached to the employee.
    #[field(
        many2many,
        comodel = "hr.employee.category",
        relation = "hr_employee_category_rel",
        column1 = "emp_id",
        column2 = "category_id",
        string = "Tags"
    )]
    pub category_ids: Vec<RecId>,

    // ── Work & Organization ─────────────────────────────────────────────────
    /// Department assignment.
    #[field(
        many2one,
        comodel = "hr.department",
        ondelete = "set null",
        index = true,
        tracking = true,
        string = "Department"
    )]
    pub department_id: Option<RecId>,
    /// Department palette index mirrored for hierarchy card headers.
    #[field(
        integer,
        compute = "_compute_department_color",
        store = true,
        string = "Department Color"
    )]
    pub department_color: i32,
    /// Formal job position record.
    #[field(
        many2one,
        comodel = "hr.job",
        ondelete = "set null",
        index = true,
        tracking = true,
        string = "Job Position"
    )]
    pub job_id: Option<RecId>,
    /// Displayed job title on cards and org charts.
    #[field(char, tracking = true, string = "Job Title")]
    pub job_title: Option<Arc<str>>,
    /// Direct reporting manager.
    #[field(
        many2one,
        comodel = "hr.employee",
        ondelete = "set null",
        index = true,
        tracking = true,
        string = "Manager"
    )]
    pub parent_id: Option<RecId>,
    /// Coach / mentor.
    #[field(
        many2one,
        comodel = "hr.employee",
        ondelete = "set null",
        string = "Coach"
    )]
    pub coach_id: Option<RecId>,
    /// Direct reports.
    #[field(
        one2many,
        comodel = "hr.employee",
        inverse_name = "parent_id",
        string = "Subordinates"
    )]
    pub child_ids: Vec<RecId>,
    /// Count of direct reports.
    #[field(
        integer,
        compute = "_compute_subordinate_count",
        string = "Direct Reports"
    )]
    pub subordinate_count: i32,
    /// Physical work address.
    #[field(
        many2one,
        comodel = "res.partner",
        ondelete = "set null",
        string = "Work Address"
    )]
    pub address_id: Option<RecId>,
    /// Primary work location.
    #[field(
        many2one,
        comodel = "hr.work.location",
        ondelete = "set null",
        string = "Work Location"
    )]
    pub work_location_id: Option<RecId>,
    /// Weekly usual work locations (Monday through Sunday).
    #[field(
        many2one,
        comodel = "hr.work.location",
        ondelete = "set null",
        string = "Monday"
    )]
    pub monday_location_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "hr.work.location",
        ondelete = "set null",
        string = "Tuesday"
    )]
    pub tuesday_location_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "hr.work.location",
        ondelete = "set null",
        string = "Wednesday"
    )]
    pub wednesday_location_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "hr.work.location",
        ondelete = "set null",
        string = "Thursday"
    )]
    pub thursday_location_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "hr.work.location",
        ondelete = "set null",
        string = "Friday"
    )]
    pub friday_location_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "hr.work.location",
        ondelete = "set null",
        string = "Saturday"
    )]
    pub saturday_location_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "hr.work.location",
        ondelete = "set null",
        string = "Sunday"
    )]
    pub sunday_location_id: Option<RecId>,
    /// Internal work note.
    #[field(text, groups = "hr.group_hr_user", string = "Note")]
    pub additional_note: Option<Arc<str>>,

    // ── Payroll & Contract Summary ──────────────────────────────────────────
    /// Employment classification (Employee, Contractor, Intern, ...).
    #[field(
        many2one,
        comodel = "hr.employee.type",
        ondelete = "set null",
        index = true,
        tracking = true,
        string = "Employee Type"
    )]
    pub employee_type_id: Option<RecId>,
    /// Current contract start date.
    #[field(
        date,
        tracking = true,
        groups = "hr.group_hr_user",
        string = "Contract Start Date"
    )]
    pub contract_date_start: Option<Date>,
    /// Current contract end date.
    #[field(
        date,
        tracking = true,
        groups = "hr.group_hr_user",
        string = "Contract End Date"
    )]
    pub contract_date_end: Option<Date>,
    /// Formatted contract date span shown on Kanban cards (e.g. "Jan 1, 2025 - Dec 31, 2027").
    #[field(
        char,
        compute = "_compute_contract_period_label",
        groups = "hr.group_hr_user",
        string = "Contract Period"
    )]
    pub contract_period_label: Option<Arc<str>>,
    /// Current monthly gross wage (HR Officers only).
    #[field(
        monetary,
        currency_field = "currency_id",
        groups = "hr.group_hr_user",
        string = "Monthly Wage"
    )]
    pub wage: Decimal,
    /// Assigned working schedule.
    #[field(
        many2one,
        comodel = "hr.work.schedule",
        ondelete = "set null",
        groups = "hr.group_hr_user",
        string = "Working Hours"
    )]
    pub work_schedule_id: Option<RecId>,
    /// Contract / employment history versions.
    #[field(
        one2many,
        comodel = "hr.employee.version",
        inverse_name = "employee_id",
        groups = "hr.group_hr_user",
        string = "Contract History"
    )]
    pub version_ids: Vec<RecId>,
    /// Count of contract history versions.
    #[field(
        integer,
        compute = "_compute_versions_count",
        groups = "hr.group_hr_user",
        string = "History"
    )]
    pub versions_count: i32,

    // ── Resume & Skills ─────────────────────────────────────────────────────
    /// Timeline entries (Experience, Education, Courses).
    #[field(
        one2many,
        comodel = "hr.resume.line",
        inverse_name = "employee_id",
        string = "Resume"
    )]
    pub resume_line_ids: Vec<RecId>,
    /// Skills and certifications held by this employee.
    #[field(
        one2many,
        comodel = "hr.employee.skill",
        inverse_name = "employee_id",
        string = "Skills"
    )]
    pub employee_skill_ids: Vec<RecId>,

    // ── Personal Information (HR Officers only) ─────────────────────────────
    #[field(char, groups = "hr.group_hr_user", string = "Private Email")]
    pub private_email: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Private Phone")]
    pub private_phone: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Bank Name")]
    pub bank_name: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Bank Account Number")]
    pub bank_account_number: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Legal Name")]
    pub legal_name: Option<Arc<str>>,
    #[field(date, groups = "hr.group_hr_user", string = "Date of Birth")]
    pub birthday: Option<Date>,
    #[field(
        boolean,
        default = false,
        groups = "hr.group_hr_user",
        string = "Show Birthday Publicly"
    )]
    pub birthday_public_display: bool,
    #[field(char, groups = "hr.group_hr_user", string = "Place of Birth")]
    pub place_of_birth: Option<Arc<str>>,
    #[field(
        many2one,
        comodel = "res.country",
        ondelete = "set null",
        groups = "hr.group_hr_user",
        string = "Country of Birth"
    )]
    pub country_of_birth_id: Option<RecId>,
    #[field(
        selection,
        selection = [("male", "Male"), ("female", "Female"), ("other", "Other")],
        groups = "hr.group_hr_user",
        string = "Gender"
    )]
    pub sex: Option<Arc<str>>,
    #[field(
        many2one,
        comodel = "res.country",
        ondelete = "set null",
        groups = "hr.group_hr_user",
        string = "Nationality (Country)"
    )]
    pub country_id: Option<RecId>,
    #[field(char, groups = "hr.group_hr_user", string = "Identification No")]
    pub identification_id: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Passport No")]
    pub passport_id: Option<Arc<str>>,
    #[field(date, groups = "hr.group_hr_user", string = "Passport Expiration Date")]
    pub passport_expiration_date: Option<Date>,
    #[field(char, groups = "hr.group_hr_user", string = "Visa No")]
    pub visa_no: Option<Arc<str>>,
    #[field(date, groups = "hr.group_hr_user", string = "Visa Expiration Date")]
    pub visa_expire: Option<Date>,
    #[field(char, groups = "hr.group_hr_user", string = "Work Permit No")]
    pub permit_no: Option<Arc<str>>,
    #[field(
        date,
        groups = "hr.group_hr_user",
        string = "Work Permit Expiration Date"
    )]
    pub work_permit_expiration_date: Option<Date>,
    #[field(binary, groups = "hr.group_hr_user", string = "Work Permit")]
    pub work_permit_file: Option<Blob>,
    #[field(char, groups = "hr.group_hr_user", string = "Work Permit Filename")]
    pub work_permit_name: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Emergency Contact")]
    pub emergency_contact: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Emergency Phone")]
    pub emergency_phone: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Street")]
    pub private_street: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Street 2")]
    pub private_street2: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "City")]
    pub private_city: Option<Arc<str>>,
    #[field(
        many2one,
        comodel = "res.country.state",
        ondelete = "set null",
        groups = "hr.group_hr_user",
        string = "State"
    )]
    pub private_state_id: Option<RecId>,
    #[field(char, groups = "hr.group_hr_user", string = "ZIP")]
    pub private_zip: Option<Arc<str>>,
    #[field(
        many2one,
        comodel = "res.country",
        ondelete = "set null",
        groups = "hr.group_hr_user",
        string = "Country"
    )]
    pub private_country_id: Option<RecId>,
    #[field(
        integer,
        default = 0,
        groups = "hr.group_hr_user",
        string = "Home-Work Distance"
    )]
    pub distance_home_work: i32,
    #[field(
        selection,
        selection = [("kilometers", "Kilometers"), ("miles", "Miles")],
        default = "kilometers",
        groups = "hr.group_hr_user",
        string = "Distance Unit"
    )]
    pub distance_home_work_unit: Option<Arc<str>>,
    #[field(
        selection,
        selection = [
            ("single", "Single"),
            ("married", "Married"),
            ("cohabitant", "Legal Cohabitant"),
            ("widower", "Widower"),
            ("divorced", "Divorced"),
        ],
        default = "single",
        groups = "hr.group_hr_user",
        string = "Marital Status"
    )]
    pub marital: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Spouse Complete Name")]
    pub spouse_complete_name: Option<Arc<str>>,
    #[field(date, groups = "hr.group_hr_user", string = "Spouse Birthdate")]
    pub spouse_birthdate: Option<Date>,
    #[field(
        integer,
        default = 0,
        groups = "hr.group_hr_user",
        string = "Dependent Children"
    )]
    pub children: i32,
    #[field(
        selection,
        selection = [
            ("graduate", "Graduate"),
            ("bachelor", "Bachelor"),
            ("master", "Master"),
            ("doctor", "Doctor"),
            ("other", "Other"),
        ],
        groups = "hr.group_hr_user",
        string = "Certificate Level"
    )]
    pub certificate: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Field of Study")]
    pub study_field: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "School")]
    pub study_school: Option<Arc<str>>,
    #[field(binary, groups = "hr.group_hr_user", string = "ID Card Copy")]
    pub id_card: Option<Blob>,
    #[field(char, groups = "hr.group_hr_user", string = "ID Card Filename")]
    pub id_card_name: Option<Arc<str>>,
    #[field(binary, groups = "hr.group_hr_user", string = "Driving License")]
    pub driving_license: Option<Blob>,
    #[field(char, groups = "hr.group_hr_user", string = "Driving License Filename")]
    pub driving_license_name: Option<Arc<str>>,

    // ── End of Collaboration / Departure ────────────────────────────────────
    #[field(
        many2one,
        comodel = "hr.departure.reason",
        ondelete = "set null",
        groups = "hr.group_hr_user",
        tracking = true,
        string = "Departure Reason"
    )]
    pub departure_reason_id: Option<RecId>,
    #[field(text, groups = "hr.group_hr_user", string = "Additional Information")]
    pub departure_description: Option<Arc<str>>,
    #[field(
        date,
        groups = "hr.group_hr_user",
        tracking = true,
        string = "Departure Date"
    )]
    pub departure_date: Option<Date>,

    // ── Settings & Attendance ───────────────────────────────────────────────
    /// Linked system user account.
    #[field(
        many2one,
        comodel = "res.users",
        ondelete = "set null",
        index = true,
        string = "Related User"
    )]
    pub user_id: Option<RecId>,
    /// Defaults to the creating user's time zone, else the company's.
    #[field(char, string = "Timezone")]
    pub tz: Option<Arc<str>>,
    #[field(
        many2one,
        comodel = "res.users",
        ondelete = "set null",
        groups = "hr.group_hr_user",
        string = "HR Responsible"
    )]
    pub hr_responsible_id: Option<RecId>,
    #[field(
        monetary,
        currency_field = "currency_id",
        groups = "hr.group_hr_user",
        string = "Hourly Cost"
    )]
    pub hourly_cost: Decimal,
    #[field(char, groups = "hr.group_hr_user", string = "PIN Code")]
    pub pin: Option<Arc<str>>,
    #[field(char, groups = "hr.group_hr_user", string = "Badge ID")]
    pub barcode: Option<Arc<str>>,
}

#[methods(HrEmployee, addon = "hr")]
impl HrEmployee {
    /// `tz` defaults to the creating user's time zone, else the current company's.
    #[method(override)]
    async fn default_get(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<ValueMap> {
        let wants_tz = names.iter().any(|name| name == "tz");
        let values = sup.call(tx, this.as_records(), (names,)).await?;
        crate::with_default_tz(tx, this.env(), wants_tz, values).await
    }

    #[compute(depends = ["department_id", "department_id.color"])]
    async fn _compute_department_color(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrEmployee>,
    ) -> Result<()> {
        let depts = this.env().records("hr.department")?;
        let dept_ids = this.mapped(tx, HrEmployee::DEPARTMENT_ID).await?;
        for (index, dept_opt) in dept_ids.into_iter().enumerate() {
            let color = if let Some(did) = dept_opt {
                match depts.browse([did.get()])?.cell(tx, "color").await? {
                    Cell::Int(n) => n,
                    _ => 0,
                }
            } else {
                0
            };
            out.set(index, HrEmployee::DEPARTMENT_COLOR, color)?;
        }
        Ok(())
    }

    #[compute(depends = ["child_ids"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_subordinate_count(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrEmployee>,
    ) -> Result<()> {
        let ids: Vec<i64> = this
            .as_records()
            .rec_ids()
            .iter()
            .map(|id| id.get())
            .collect();
        let counts = count_by(tx, this.env(), "hr.employee", "parent_id", &ids).await?;
        for (index, id) in ids.iter().enumerate() {
            let count = as_field(counts.get(id).copied().unwrap_or(0), "subordinate")?;
            out.set(index, HrEmployee::SUBORDINATE_COUNT, count)?;
        }
        Ok(())
    }

    #[compute(depends = ["version_ids"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_versions_count(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrEmployee>,
    ) -> Result<()> {
        let ids: Vec<i64> = this
            .as_records()
            .rec_ids()
            .iter()
            .map(|id| id.get())
            .collect();
        let counts = count_by(tx, this.env(), "hr.employee.version", "employee_id", &ids).await?;
        for (index, id) in ids.iter().enumerate() {
            let count = as_field(counts.get(id).copied().unwrap_or(0), "version")?;
            out.set(index, HrEmployee::VERSIONS_COUNT, count)?;
        }
        Ok(())
    }

    #[compute(depends = ["contract_date_start", "contract_date_end"])]
    async fn _compute_contract_period_label(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrEmployee>,
    ) -> Result<()> {
        let starts = this.mapped(tx, HrEmployee::CONTRACT_DATE_START).await?;
        let ends = this.mapped(tx, HrEmployee::CONTRACT_DATE_END).await?;
        for (index, (start_opt, end_opt)) in starts.into_iter().zip(ends).enumerate() {
            let label = match (start_opt, end_opt) {
                (Some(s), Some(e)) => Some(Arc::<str>::from(format!("{s} – {e}"))),
                (Some(s), None) => Some(Arc::<str>::from(format!("Since {s}"))),
                (None, Some(e)) => Some(Arc::<str>::from(format!("Until {e}"))),
                (None, None) => None,
            };
            out.set(index, HrEmployee::CONTRACT_PERIOD_LABEL, label)?;
        }
        Ok(())
    }

    #[constrains("parent_id")]
    async fn _check_parent_recursion(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let depth = count_parameter(tx, HIERARCHY_DEPTH_KEY).await?;
        let employees = this.env().records("hr.employee")?;
        for id in this.as_records().rec_ids() {
            let start = id.get();
            let mut seen = BTreeSet::from([start]);
            let mut cursor = start;
            let mut hops = 0;
            loop {
                let row = employees.browse([cursor])?;
                let next = match row.cell(tx, "parent_id").await? {
                    Cell::Id(pid) => pid,
                    _ => break,
                };
                if !seen.insert(next) {
                    return Err(recursive_managers());
                }
                hops += 1;
                if hops > depth {
                    return Err(too_deep(depth));
                }
                cursor = next;
            }
        }
        Ok(())
    }

    #[constrains("contract_date_start", "contract_date_end")]
    async fn _check_contract_dates(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let starts = this.mapped(tx, HrEmployee::CONTRACT_DATE_START).await?;
        let ends = this.mapped(tx, HrEmployee::CONTRACT_DATE_END).await?;
        for (start_opt, end_opt) in starts.into_iter().zip(ends) {
            if let (Some(start), Some(end)) = (start_opt, end_opt)
                && end < start
            {
                return Err(Error::validation(
                    "Contract end date cannot be earlier than contract start date.",
                ));
            }
        }
        Ok(())
    }

    #[onchange("job_id")]
    async fn _onchange_job_id(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<OnchangeResult> {
        let row = this.as_records();
        if let Cell::Id(job_id) = row.cell(tx, "job_id").await? {
            let job = this.env().records("hr.job")?.browse([job_id])?;
            let mut vals = RawVals::new();
            if let Cell::Str(name) = job.cell(tx, "name").await? {
                vals = vals.set_cell("job_title", Cell::Str(name));
            }
            if matches!(row.cell(tx, "department_id").await?, Cell::Null)
                && let Cell::Id(dept_id) = job.cell(tx, "department_id").await?
            {
                vals = vals.set_cell("department_id", Cell::Id(dept_id));
            }
            if !vals.is_empty() {
                row.write_base(tx, vals).await?;
            }
        }
        Ok(OnchangeResult::none())
    }

    #[onchange("department_id")]
    async fn _onchange_department_id(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<OnchangeResult> {
        let row = this.as_records();
        if let Cell::Id(dept_id) = row.cell(tx, "department_id").await? {
            let dept = this.env().records("hr.department")?.browse([dept_id])?;
            if let Cell::Id(manager_id) = dept.cell(tx, "manager_id").await? {
                let own_id = row.ids().first().copied();
                if own_id != Some(manager_id)
                    && matches!(row.cell(tx, "parent_id").await?, Cell::Null)
                {
                    row.write_base(
                        tx,
                        RawVals::new().set_cell("parent_id", Cell::Id(manager_id)),
                    )
                    .await?;
                }
            }
        }
        Ok(OnchangeResult::none())
    }

    #[onchange("user_id")]
    async fn _onchange_user_id(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<OnchangeResult> {
        let row = this.as_records();
        if let Cell::Id(uid) = row.cell(tx, "user_id").await? {
            let user = this.env().records("res.users")?.browse([uid])?;
            let mut vals = RawVals::new();
            if matches!(row.cell(tx, "work_email").await?, Cell::Null)
                && let Cell::Str(email) = user.cell(tx, "email").await?
                && !email.is_empty()
            {
                vals = vals.set_cell("work_email", Cell::Str(email));
            }
            if !vals.is_empty() {
                row.write_base(tx, vals).await?;
            }
        }
        Ok(OnchangeResult::none())
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_open_versions(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let id = this.as_records().ensure_one()?.int()?;
        let name = match this.as_records().cell(tx, "name").await? {
            Cell::Str(s) => format!("{s} — Contract History"),
            _ => "Contract History".to_string(),
        };
        let ctx = ValueMap::from([("default_employee_id".into(), Value::Int(id))]);
        Ok(Value::Map(Arc::new(ValueMap::from([
            ("type".into(), Value::str("ir.actions.act_window")),
            ("name".into(), Value::str(name)),
            ("res_model".into(), Value::str("hr.employee.version")),
            (
                "views".into(),
                Value::list([
                    Value::list([Value::Bool(false), Value::str("list")]),
                    Value::list([Value::Bool(false), Value::str("form")]),
                ]),
            ),
            (
                "domain".into(),
                Value::list([Value::list([
                    Value::str("employee_id"),
                    Value::str("="),
                    Value::Int(id),
                ])]),
            ),
            ("context".into(), Value::Map(Arc::new(ctx))),
        ]))))
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_open_subordinates(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let id = this.as_records().ensure_one()?.int()?;
        let name = match this.as_records().cell(tx, "name").await? {
            Cell::Str(s) => format!("{s} — Team"),
            _ => "Direct Reports".to_string(),
        };
        Ok(Value::Map(Arc::new(ValueMap::from([
            ("type".into(), Value::str("ir.actions.act_window")),
            ("name".into(), Value::str(name)),
            ("res_model".into(), Value::str("hr.employee")),
            (
                "views".into(),
                Value::list([
                    Value::list([Value::Bool(false), Value::str("kanban")]),
                    Value::list([Value::Bool(false), Value::str("list")]),
                    Value::list([Value::Bool(false), Value::str("hierarchy")]),
                    Value::list([Value::Bool(false), Value::str("form")]),
                ]),
            ),
            (
                "domain".into(),
                Value::list([Value::list([
                    Value::str("parent_id"),
                    Value::str("="),
                    Value::Int(id),
                ])]),
            ),
            (
                "context".into(),
                Value::Map(Arc::new(ValueMap::from([(
                    "default_parent_id".into(),
                    Value::Int(id),
                )]))),
            ),
        ]))))
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_open_org_chart(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let id = this.as_records().ensure_one()?.int()?;
        let name = match this.as_records().cell(tx, "name").await? {
            Cell::Str(s) => format!("{s} — Org Chart"),
            _ => "Organization Chart".to_string(),
        };
        Ok(Value::Map(Arc::new(ValueMap::from([
            ("type".into(), Value::str("ir.actions.act_window")),
            ("name".into(), Value::str(name)),
            ("res_model".into(), Value::str("hr.employee")),
            (
                "views".into(),
                Value::list([
                    Value::list([Value::Bool(false), Value::str("hierarchy")]),
                    Value::list([Value::Bool(false), Value::str("kanban")]),
                    Value::list([Value::Bool(false), Value::str("list")]),
                    Value::list([Value::Bool(false), Value::str("form")]),
                ]),
            ),
            (
                "context".into(),
                Value::Map(Arc::new(ValueMap::from([(
                    "default_parent_id".into(),
                    Value::Int(id),
                )]))),
            ),
        ]))))
    }

    #[method]
    #[rpc(effect = read)]
    async fn action_open_departure_wizard(
        this: &RecordSet<HrEmployee>,
        _tx: &mut Transaction,
    ) -> Result<Value> {
        let id = this.as_records().ensure_one()?.int()?;
        Ok(Value::Map(Arc::new(ValueMap::from([
            ("type".into(), Value::str("ir.actions.act_window")),
            ("name".into(), Value::str("End of Collaboration")),
            ("res_model".into(), Value::str("hr.departure.wizard")),
            ("target".into(), Value::str("new")),
            (
                "views".into(),
                Value::list([Value::list([Value::Bool(false), Value::str("form")])]),
            ),
            (
                "context".into(),
                Value::Map(Arc::new(ValueMap::from([(
                    "default_employee_id".into(),
                    Value::Int(id),
                )]))),
            ),
        ]))))
    }

    #[method]
    #[rpc(effect = write)]
    async fn action_cancel_departure(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.as_records();
        rec.ensure_one()?;
        rec.check_access(tx, Operation::Write).await?;
        rec.write(
            tx,
            RawVals::new()
                .set_cell("active", Cell::Bool(true))
                .set_cell("departure_reason_id", Cell::Null)
                .set_cell("departure_date", Cell::Null)
                .set_cell("departure_description", Cell::Null),
        )
        .await?;
        Ok(Value::Map(Arc::new(ValueMap::from([
            ("type".into(), Value::str("ir.actions.client")),
            ("tag".into(), Value::str("display_notification")),
            (
                "params".into(),
                Value::Map(Arc::new(ValueMap::from([
                    ("title".into(), Value::str("Collaboration Restored")),
                    (
                        "message".into(),
                        Value::str("Employee has been reactivated and departure details cleared."),
                    ),
                    ("type".into(), Value::str("success")),
                ]))),
            ),
        ]))))
    }

    #[method]
    #[rpc(effect = write)]
    async fn action_generate_barcode(
        this: &RecordSet<HrEmployee>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.as_records();
        rec.ensure_one()?;
        rec.check_access(tx, Operation::Write).await?;
        let Cell::Id(company) = rec.cell(tx, "company_id").await? else {
            return Err(Error::validation("The employee has no company."));
        };
        let employees = this.env().records("hr.employee")?;
        let mut code = None;
        for _ in 0..BADGE_ATTEMPTS {
            let candidate = random_badge();
            let taken = Domain::and([
                Domain::field("company_id")?.eq(company),
                Domain::field("barcode")?.eq(candidate.as_str()),
            ]);
            let search = SearchOptions {
                active_test: false,
                ..SearchOptions::default()
            };
            if employees.search_with(tx, &taken, &search).await?.is_empty() {
                code = Some(candidate);
                break;
            }
        }
        let Some(code) = code else {
            return Err(Error::user(
                "No free Badge ID was found for this company; try again.",
            ));
        };
        rec.write(
            tx,
            RawVals::new().set_cell("barcode", Cell::Str(code.clone().into())),
        )
        .await?;
        Ok(Value::Map(Arc::new(ValueMap::from([
            ("type".into(), Value::str("ir.actions.client")),
            ("tag".into(), Value::str("display_notification")),
            (
                "params".into(),
                Value::Map(Arc::new(ValueMap::from([
                    ("title".into(), Value::str("Badge ID Generated")),
                    (
                        "message".into(),
                        Value::str(format!("Assigned Badge ID {code}.")),
                    ),
                    ("type".into(), Value::str("info")),
                ]))),
            ),
        ]))))
    }

    /// Returns `{ self, managers, subordinates, subordinates_total, subordinates_truncated }` for
    /// the inline `hr.org_chart` widget. At most `hr.max_org_chart_rows` subordinates are listed;
    /// a manager chain longer than `hr.max_hierarchy_depth` or a cycle is an error.
    #[method]
    #[rpc(effect = read)]
    async fn get_org_chart(this: &RecordSet<HrEmployee>, tx: &mut Transaction) -> Result<Value> {
        let current_id = this.as_records().ensure_one()?.int()?;
        let depth = count_parameter(tx, HIERARCHY_DEPTH_KEY).await?;
        let rows = count_parameter(tx, ORG_CHART_ROWS_KEY).await?;
        let employees = this.env().records("hr.employee")?;
        let depts = this.env().records("hr.department")?;

        let (self_val, mut manager_cursor) =
            read_employee_node(&employees, &depts, current_id, tx).await?;
        let mut managers = Vec::new();
        let mut seen = BTreeSet::from([current_id]);
        while let Some(mid) = manager_cursor {
            if !seen.insert(mid) {
                return Err(recursive_managers());
            }
            if managers.len() == depth {
                return Err(too_deep(depth));
            }
            let (val, next_parent) = read_employee_node(&employees, &depts, mid, tx).await?;
            managers.push(val);
            manager_cursor = next_parent;
        }
        managers.reverse();

        let sub_domain = Domain::condition("parent_id", Operator::Eq, current_id)?;
        let total = employees.search_count(tx, &sub_domain, None).await?;
        let limit = u64::try_from(rows)
            .map_err(|_| Error::programming(format!("{ORG_CHART_ROWS_KEY} {rows} exceeds u64")))?;
        let sub_recs = employees
            .search_with(tx, &sub_domain, &SearchOptions::new().limit(limit))
            .await?;
        let mut subordinates = Vec::with_capacity(sub_recs.len());
        for sid in sub_recs.ids().iter().copied() {
            let (val, _) = read_employee_node(&employees, &depts, sid, tx).await?;
            subordinates.push(val);
        }
        let shown = u64::try_from(subordinates.len())
            .map_err(|_| Error::programming("subordinate rows exceed u64"))?;
        let total_val = i64::try_from(total)
            .map_err(|_| Error::programming(format!("subordinate count {total} exceeds i64")))?;

        Ok(Value::Map(Arc::new(ValueMap::from([
            ("self".into(), self_val),
            ("managers".into(), Value::list(managers)),
            ("subordinates".into(), Value::list(subordinates)),
            ("subordinates_total".into(), Value::Int(total_val)),
            ("subordinates_truncated".into(), Value::Bool(total > shown)),
        ]))))
    }
}

async fn read_employee_node(
    employees: &Records,
    depts: &Records,
    id: i64,
    tx: &mut Transaction,
) -> Result<(Value, Option<i64>)> {
    let rec = employees.browse([id])?;
    let name = match rec.cell(tx, "name").await? {
        Cell::Str(s) => s.to_string(),
        _ => format!("#{id}"),
    };
    let job_title = match rec.cell(tx, "job_title").await? {
        Cell::Str(s) => s.to_string(),
        _ => String::new(),
    };
    let dept_name = match rec.cell(tx, "department_id").await? {
        Cell::Id(did) => match depts.browse([did])?.cell(tx, "name").await? {
            Cell::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    let color: i64 = match rec.cell(tx, "department_color").await? {
        Cell::Int(n) => i64::from(n),
        _ => 0,
    };
    let img_cell = rec.cell(tx, "image_128").await?;
    let image_val = match &img_cell {
        Cell::Bytes(b) if !b.is_empty() && b.len() <= 256 * 1024 => Value::from_cell(&img_cell),
        _ => Value::False,
    };
    let has_image = !matches!(image_val, Value::False);
    let direct_sub: i64 = match rec.cell(tx, "subordinate_count").await? {
        Cell::Int(n) => i64::from(n),
        _ => 0,
    };
    let parent_id = match rec.cell(tx, "parent_id").await? {
        Cell::Id(pid) => Some(pid),
        _ => None,
    };
    let map = ValueMap::from([
        ("id".into(), Value::Int(id)),
        ("name".into(), Value::str(name)),
        ("job_title".into(), Value::str(job_title)),
        ("department_name".into(), Value::str(dept_name)),
        ("color".into(), Value::Int(color)),
        ("has_image".into(), Value::Bool(has_image)),
        ("image_128".into(), image_val),
        ("direct_sub_count".into(), Value::Int(direct_sub)),
    ]);
    Ok((Value::Map(Arc::new(map)), parent_id))
}
