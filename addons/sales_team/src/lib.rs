// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addon `sales_team`: sales teams (`crm.team`), their memberships (`crm.team.member`), tags
//! (`crm.tag`), the team fields of a user and the Sales privilege with its three groups.
//! `sale` and the CRM addons build on it.

erp_sdk::addon!(
    name = "sales_team",
    exact_only = true,
    depends = ["base", "mail"]
);

pub mod member;
mod res_users;
pub mod tag;
pub mod team;

pub use member::CrmTeamMember;
pub use res_users::UsersSalesTeam;
pub use tag::CrmTag;
pub use team::{CrmTeam, MEMBERSHIP_MULTI_KEY};
