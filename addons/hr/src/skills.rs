// SPDX-License-Identifier: MIT OR Apache-2.0
//! Resume and Skills / Certifications models for `addons/hr`:
//! - `hr.resume.line.type`
//! - `hr.resume.line`
//! - `hr.skill.type`
//! - `hr.skill`
//! - `hr.skill.level`
//! - `hr.employee.skill`

use erp_sdk::prelude::*;

/// `hr.resume.line.type`: section categories for an employee's resume (Experience, Education, Courses).
#[model(
    name = "hr.resume.line.type",
    description = "Resume Line Type",
    order = "sequence, name, id"
)]
pub struct HrResumeLineType {
    /// Section name.
    #[field(char, required = true, translate = true, string = "Name")]
    pub name: Option<Arc<str>>,
    /// Display sequence.
    #[field(integer, default = 10, string = "Sequence")]
    pub sequence: i32,
    /// Marks this section as training / course entries.
    #[field(boolean, default = false, string = "Is Course")]
    pub is_course: bool,
}

/// `hr.resume.line`: one experience, education, or course item on an employee's timeline.
#[model(
    name = "hr.resume.line",
    description = "Resume Entry",
    order = "date_start desc, id desc",
    company_access = direct(field = "company_id", branch = "exact", empty = "allow")
)]
pub struct HrResumeLine {
    /// Owning employee.
    #[field(
        many2one,
        comodel = "hr.employee",
        required = true,
        ondelete = "cascade",
        index = true,
        string = "Employee"
    )]
    pub employee_id: Option<RecId>,
    /// Derived company from the employee.
    #[field(
        many2one,
        comodel = "res.company",
        compute = "_compute_company_id",
        store = true,
        index = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Resume section (Experience, Education, Courses, ...).
    #[field(
        many2one,
        comodel = "hr.resume.line.type",
        ondelete = "set null",
        string = "Section"
    )]
    pub line_type_id: Option<RecId>,
    /// Entry title (role, degree, or course name).
    #[field(char, required = true, string = "Title")]
    pub name: Option<Arc<str>>,
    /// Start date.
    #[field(date, required = true, string = "Start Date")]
    pub date_start: Option<Date>,
    /// End date (blank = Present).
    #[field(date, string = "End Date")]
    pub date_end: Option<Date>,
    /// Summary or achievements.
    #[field(text, string = "Description")]
    pub description: Option<Arc<str>>,
    /// Mirrors `line_type_id.is_course`.
    #[field(
        boolean,
        compute = "_compute_is_course",
        store = true,
        string = "Course"
    )]
    pub is_course: bool,
    /// Optional course duration in hours.
    #[field(float, default = 0.0, string = "Duration (Hours)")]
    pub duration_hours: f64,
    /// Optional credential or reference link.
    #[field(char, string = "Reference URL")]
    pub external_url: Option<Arc<str>>,
    /// Optional certificate attachment.
    #[field(binary, string = "Certificate")]
    pub certificate_file: Option<Blob>,
    /// Certificate filename.
    #[field(char, string = "Certificate Filename")]
    pub certificate_filename: Option<Arc<str>>,
}

#[methods(HrResumeLine, addon = "hr")]
impl HrResumeLine {
    #[compute(depends = ["employee_id", "employee_id.company_id"])]
    async fn _compute_company_id(
        this: &RecordSet<HrResumeLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrResumeLine>,
    ) -> Result<()> {
        let employees = this.env().records("hr.employee")?;
        let emp_ids = this.mapped(tx, HrResumeLine::EMPLOYEE_ID).await?;
        for (index, emp_opt) in emp_ids.into_iter().enumerate() {
            let company = if let Some(eid) = emp_opt {
                match employees
                    .browse([eid.get()])?
                    .cell(tx, "company_id")
                    .await?
                {
                    Cell::Id(cid) => RecId::from_real(cid),
                    _ => None,
                }
            } else {
                None
            };
            out.set(index, HrResumeLine::COMPANY_ID, company)?;
        }
        Ok(())
    }

    #[compute(depends = ["line_type_id", "line_type_id.is_course"])]
    async fn _compute_is_course(
        this: &RecordSet<HrResumeLine>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrResumeLine>,
    ) -> Result<()> {
        let types = this.env().records("hr.resume.line.type")?;
        let type_ids = this.mapped(tx, HrResumeLine::LINE_TYPE_ID).await?;
        for (index, tid_opt) in type_ids.into_iter().enumerate() {
            let is_course = if let Some(tid) = tid_opt {
                matches!(
                    types.browse([tid.get()])?.cell(tx, "is_course").await?,
                    Cell::Bool(true)
                )
            } else {
                false
            };
            out.set(index, HrResumeLine::IS_COURSE, is_course)?;
        }
        Ok(())
    }

    #[constrains("date_start", "date_end")]
    async fn _check_dates(this: &RecordSet<HrResumeLine>, tx: &mut Transaction) -> Result<()> {
        let starts = this.mapped(tx, HrResumeLine::DATE_START).await?;
        let ends = this.mapped(tx, HrResumeLine::DATE_END).await?;
        for (start_opt, end_opt) in starts.into_iter().zip(ends) {
            if let (Some(start), Some(end)) = (start_opt, end_opt)
                && end < start
            {
                return Err(Error::validation(
                    "Resume end date cannot be earlier than start date.",
                ));
            }
        }
        Ok(())
    }
}

/// `hr.skill.type`: a group of related skills or certifications (e.g. Languages, Programming, Cloud Certifications).
#[model(name = "hr.skill.type", description = "Skill Type", order = "name, id")]
pub struct HrSkillType {
    /// Skill category name.
    #[field(char, required = true, translate = true, string = "Skill Type")]
    pub name: Option<Arc<str>>,
    /// Archivable flag.
    #[field(boolean, default = true, string = "Active")]
    pub active: bool,
    /// True when entries of this type represent formal certifications with validity dates.
    #[field(boolean, default = false, string = "Certification")]
    pub is_certification: bool,
    /// Color index (0..11).
    #[field(integer, default = 0, string = "Color")]
    pub color: i32,
    /// Skills belonging to this type.
    #[field(
        one2many,
        comodel = "hr.skill",
        inverse_name = "skill_type_id",
        string = "Skills"
    )]
    pub skill_ids: Vec<RecId>,
    /// Proficiency levels belonging to this type.
    #[field(
        one2many,
        comodel = "hr.skill.level",
        inverse_name = "skill_type_id",
        string = "Levels"
    )]
    pub skill_level_ids: Vec<RecId>,
}

/// `hr.skill`: an individual skill or certification under a `hr.skill.type`.
#[model(name = "hr.skill", description = "Skill", order = "sequence, name, id")]
pub struct HrSkill {
    /// Skill name.
    #[field(char, required = true, string = "Skill")]
    pub name: Option<Arc<str>>,
    /// Display order.
    #[field(integer, default = 10, string = "Sequence")]
    pub sequence: i32,
    /// Parent skill type.
    #[field(
        many2one,
        comodel = "hr.skill.type",
        required = true,
        ondelete = "cascade",
        index = true,
        string = "Skill Type"
    )]
    pub skill_type_id: Option<RecId>,
}

/// `hr.skill.level`: a proficiency level (0..100%) under a `hr.skill.type`.
#[model(
    name = "hr.skill.level",
    description = "Skill Level",
    order = "level_progress desc, name, id"
)]
pub struct HrSkillLevel {
    /// Level label (e.g. Beginner, Intermediate, Advanced, Expert, Certified).
    #[field(char, required = true, string = "Level")]
    pub name: Option<Arc<str>>,
    /// Parent skill type.
    #[field(
        many2one,
        comodel = "hr.skill.type",
        required = true,
        ondelete = "cascade",
        index = true,
        string = "Skill Type"
    )]
    pub skill_type_id: Option<RecId>,
    /// Proficiency percentage (0..100).
    #[field(integer, default = 0, string = "Progress (%)")]
    pub level_progress: i32,
    /// Whether this is the default level for newly assigned skills of this type.
    #[field(boolean, default = false, string = "Default Level")]
    pub default_level: bool,
}

#[methods(HrSkillLevel, addon = "hr")]
impl HrSkillLevel {
    #[constrains("level_progress")]
    async fn _check_progress(this: &RecordSet<HrSkillLevel>, tx: &mut Transaction) -> Result<()> {
        for progress in this.mapped(tx, HrSkillLevel::LEVEL_PROGRESS).await? {
            if !(0..=100).contains(&progress) {
                return Err(Error::validation(
                    "Skill level progress must be between 0 and 100.",
                ));
            }
        }
        Ok(())
    }
}

/// `hr.employee.skill`: a skill or certification held by an employee at a proficiency level.
#[model(
    name = "hr.employee.skill",
    description = "Employee Skill",
    order = "skill_type_id, level_progress desc, id",
    company_access = direct(field = "company_id", branch = "exact", empty = "allow")
)]
pub struct HrEmployeeSkill {
    /// Owning employee.
    #[field(
        many2one,
        comodel = "hr.employee",
        required = true,
        ondelete = "cascade",
        index = true,
        string = "Employee"
    )]
    pub employee_id: Option<RecId>,
    /// Derived company from the employee.
    #[field(
        many2one,
        comodel = "res.company",
        compute = "_compute_company_id",
        store = true,
        index = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Skill category.
    #[field(
        many2one,
        comodel = "hr.skill.type",
        required = true,
        ondelete = "restrict",
        index = true,
        string = "Skill Type"
    )]
    pub skill_type_id: Option<RecId>,
    /// Specific skill or certification.
    #[field(
        many2one,
        comodel = "hr.skill",
        required = true,
        ondelete = "restrict",
        index = true,
        string = "Skill"
    )]
    pub skill_id: Option<RecId>,
    /// Attained proficiency level.
    #[field(
        many2one,
        comodel = "hr.skill.level",
        required = true,
        ondelete = "restrict",
        string = "Skill Level"
    )]
    pub skill_level_id: Option<RecId>,
    /// Percentage mirrored from `skill_level_id.level_progress`.
    #[field(
        integer,
        compute = "_compute_level_progress",
        store = true,
        string = "Progress"
    )]
    pub level_progress: i32,
    /// Mirrored from `skill_type_id.is_certification`.
    #[field(
        boolean,
        compute = "_compute_is_certification",
        store = true,
        string = "Certification"
    )]
    pub is_certification: bool,
    /// Certification issue date.
    #[field(date, string = "Valid From")]
    pub valid_from: Option<Date>,
    /// Certification expiry date.
    #[field(date, string = "Valid Until")]
    pub valid_to: Option<Date>,
    /// Optional uploaded certificate file.
    #[field(binary, string = "Certificate")]
    pub certificate_file: Option<Blob>,
    /// Certificate filename.
    #[field(char, string = "Certificate Filename")]
    pub certificate_filename: Option<Arc<str>>,
}

#[methods(HrEmployeeSkill, addon = "hr")]
impl HrEmployeeSkill {
    #[compute(depends = ["employee_id", "employee_id.company_id"])]
    async fn _compute_company_id(
        this: &RecordSet<HrEmployeeSkill>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrEmployeeSkill>,
    ) -> Result<()> {
        let employees = this.env().records("hr.employee")?;
        let emp_ids = this.mapped(tx, HrEmployeeSkill::EMPLOYEE_ID).await?;
        for (index, emp_opt) in emp_ids.into_iter().enumerate() {
            let company = if let Some(eid) = emp_opt {
                match employees
                    .browse([eid.get()])?
                    .cell(tx, "company_id")
                    .await?
                {
                    Cell::Id(cid) => RecId::from_real(cid),
                    _ => None,
                }
            } else {
                None
            };
            out.set(index, HrEmployeeSkill::COMPANY_ID, company)?;
        }
        Ok(())
    }

    #[compute(depends = ["skill_level_id", "skill_level_id.level_progress"])]
    async fn _compute_level_progress(
        this: &RecordSet<HrEmployeeSkill>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrEmployeeSkill>,
    ) -> Result<()> {
        let levels = this.env().records("hr.skill.level")?;
        let level_ids = this.mapped(tx, HrEmployeeSkill::SKILL_LEVEL_ID).await?;
        for (index, lid_opt) in level_ids.into_iter().enumerate() {
            let progress = if let Some(lid) = lid_opt {
                match levels
                    .browse([lid.get()])?
                    .cell(tx, "level_progress")
                    .await?
                {
                    Cell::Int(n) => n,
                    _ => 0,
                }
            } else {
                0
            };
            out.set(index, HrEmployeeSkill::LEVEL_PROGRESS, progress)?;
        }
        Ok(())
    }

    #[compute(depends = ["skill_type_id", "skill_type_id.is_certification"])]
    async fn _compute_is_certification(
        this: &RecordSet<HrEmployeeSkill>,
        tx: &mut Transaction,
        out: &mut Assign<'_, HrEmployeeSkill>,
    ) -> Result<()> {
        let types = this.env().records("hr.skill.type")?;
        let type_ids = this.mapped(tx, HrEmployeeSkill::SKILL_TYPE_ID).await?;
        for (index, tid_opt) in type_ids.into_iter().enumerate() {
            let is_cert = if let Some(tid) = tid_opt {
                matches!(
                    types
                        .browse([tid.get()])?
                        .cell(tx, "is_certification")
                        .await?,
                    Cell::Bool(true)
                )
            } else {
                false
            };
            out.set(index, HrEmployeeSkill::IS_CERTIFICATION, is_cert)?;
        }
        Ok(())
    }

    #[constrains("skill_type_id", "skill_id", "skill_level_id")]
    async fn _check_skill_consistency(
        this: &RecordSet<HrEmployeeSkill>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let skills = this.env().records("hr.skill")?;
        let levels = this.env().records("hr.skill.level")?;
        let types = this.mapped(tx, HrEmployeeSkill::SKILL_TYPE_ID).await?;
        let skill_ids = this.mapped(tx, HrEmployeeSkill::SKILL_ID).await?;
        let level_ids = this.mapped(tx, HrEmployeeSkill::SKILL_LEVEL_ID).await?;
        for ((type_opt, skill_opt), level_opt) in types.into_iter().zip(skill_ids).zip(level_ids) {
            let Some(type_id) = type_opt else { continue };
            if let Some(sid) = skill_opt
                && skills
                    .browse([sid.get()])?
                    .cell(tx, "skill_type_id")
                    .await?
                    != Cell::Id(type_id.get())
            {
                return Err(Error::validation(
                    "The selected skill does not belong to the chosen skill type.",
                ));
            }
            if let Some(lid) = level_opt
                && levels
                    .browse([lid.get()])?
                    .cell(tx, "skill_type_id")
                    .await?
                    != Cell::Id(type_id.get())
            {
                return Err(Error::validation(
                    "The selected skill level does not belong to the chosen skill type.",
                ));
            }
        }
        Ok(())
    }
}
