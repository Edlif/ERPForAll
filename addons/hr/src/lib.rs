// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `hr`: complete Employees application — employees, departments, job positions,
//! organizational hierarchy, contracts/employment history, resume, skills & certifications,
//! work locations, working schedules, and end-of-collaboration workflow.

erp_sdk::addon!(name = "hr", depends = ["base", "web", "mail"]);

mod counts;
pub mod department;
pub mod departure_wizard;
pub mod employee;
pub mod job;
pub mod reference;
pub mod skills;
pub mod version;

pub use department::HrDepartment;
pub use departure_wizard::HrDepartureWizard;
pub use employee::HrEmployee;
pub use job::HrJob;
pub use reference::{
    HrDepartureReason, HrEmployeeCategory, HrEmployeeType, HrWorkLocation, HrWorkSchedule,
};
pub use skills::{
    HrEmployeeSkill, HrResumeLine, HrResumeLineType, HrSkill, HrSkillLevel, HrSkillType,
};
pub use version::HrEmployeeVersion;

/// Most subordinates or sub-departments one org chart returns; the rest is reported as truncated.
pub const ORG_CHART_ROWS_KEY: &str = "hr.max_org_chart_rows";
/// Most manager or parent-department levels above one record; a deeper chain is refused.
pub const HIERARCHY_DEPTH_KEY: &str = "hr.max_hierarchy_depth";

fn parameters() -> erp_sdk::Result<Vec<erp_sdk::settings::ParameterDecl>> {
    Ok(vec![
        erp_sdk::settings::ParameterDecl::integer(
            ORG_CHART_ROWS_KEY,
            "Subordinates or sub-departments one org chart shows",
            100,
            (1, 1_000),
        )?,
        erp_sdk::settings::ParameterDecl::integer(
            HIERARCHY_DEPTH_KEY,
            "Manager or parent-department levels above one record",
            64,
            (1, 1_000),
        )?,
    ])
}
erp_sdk::system_parameters!(parameters);

/// The time zone a new employee or working schedule starts with: the creating user's, else the
/// current company's. `None` when neither is set.
pub(crate) async fn default_tz(
    tx: &mut erp_sdk::Transaction,
    env: &erp_sdk::Env,
) -> erp_sdk::Result<Option<std::sync::Arc<str>>> {
    use erp_sdk::Cell;
    let user = env.records("res.users")?.browse([env.uid()])?;
    match user.cell(tx, "tz").await {
        Ok(Cell::Str(tz)) if !tz.is_empty() => return Ok(Some(tz)),
        Ok(_) => {}
        // the bootstrap superuser may have no user row: it has no preference
        Err(error) if error.kind() == erp_sdk::ErrorKind::Missing => {}
        Err(error) => return Err(error),
    }
    let Some(company) = env.company_id()? else {
        return Ok(None);
    };
    match env
        .records("res.company")?
        .browse([company])?
        .cell(tx, "tz")
        .await?
    {
        Cell::Str(tz) if !tz.is_empty() => Ok(Some(tz)),
        _ => Ok(None),
    }
}

/// `default_get` with `tz` from [`default_tz`] when the caller asked for it and gave none.
pub(crate) async fn with_default_tz(
    tx: &mut erp_sdk::Transaction,
    env: &erp_sdk::Env,
    wants_tz: bool,
    mut values: erp_sdk::ValueMap,
) -> erp_sdk::Result<erp_sdk::ValueMap> {
    if wants_tz
        && !values.contains_key("tz")
        && let Some(tz) = default_tz(tx, env).await?
    {
        values.insert("tz".to_owned(), erp_sdk::Value::Str(tz));
    }
    Ok(values)
}

/// A declared HR parameter as a count (its floor is 1, so it is always positive).
pub(crate) async fn count_parameter(
    tx: &mut erp_sdk::Transaction,
    key: &str,
) -> erp_sdk::Result<usize> {
    let value = erp_sdk::params::get_int(tx, &ADDON, key).await?;
    usize::try_from(value).map_err(|_| {
        erp_sdk::Error::programming(format!("Parameter {key} is {value}, not a count."))
    })
}

#[erp_sdk::__private::linkme::distributed_slice(erp_sdk::views::FIELD_WIDGETS)]
#[linkme(crate = erp_sdk::__private::linkme)]
static HR_ORG_CHART_WIDGET: erp_sdk::views::WidgetDecl =
    erp_sdk::views::WidgetDecl::new("hr.org_chart", &["one2many"]);

#[erp_sdk::__private::linkme::distributed_slice(erp_sdk::views::FIELD_WIDGETS)]
#[linkme(crate = erp_sdk::__private::linkme)]
static HR_DEPARTMENT_CHART_WIDGET: erp_sdk::views::WidgetDecl =
    erp_sdk::views::WidgetDecl::new("hr.department_chart", &["one2many"]);

#[erp_sdk::__private::linkme::distributed_slice(erp_sdk::views::FIELD_WIDGETS)]
#[linkme(crate = erp_sdk::__private::linkme)]
static HR_RESUME_TIMELINE_WIDGET: erp_sdk::views::WidgetDecl =
    erp_sdk::views::WidgetDecl::new("hr.resume_timeline", &["one2many"]);

#[erp_sdk::__private::linkme::distributed_slice(erp_sdk::views::FIELD_WIDGETS)]
#[linkme(crate = erp_sdk::__private::linkme)]
static HR_SKILLS_PANEL_WIDGET: erp_sdk::views::WidgetDecl =
    erp_sdk::views::WidgetDecl::new("hr.skills_panel", &["one2many"]);
