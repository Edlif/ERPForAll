// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `hr`: complete Employees application — employees, departments, job positions,
//! organizational hierarchy, contracts/employment history, resume, skills & certifications,
//! work locations, working schedules, and end-of-collaboration workflow.

erp_sdk::addon!(name = "hr", depends = ["base", "web", "mail"]);

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
