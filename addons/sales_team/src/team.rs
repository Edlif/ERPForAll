// SPDX-License-Identifier: MIT OR Apache-2.0
//! `crm.team`: a sales team.
//!
//! Members are `crm.team.member` rows; `member_ids` is their active users, and writing it adds the
//! missing memberships and archives or restores the others. Whether a user
//! may belong to several teams is the system parameter [`MEMBERSHIP_MULTI_KEY`] (
//! off when unset). `_get_default_team_id` picks the team of a sales document.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use erp_addon_mail::MailThread;
use erp_sdk::prelude::*;
use erp_sdk::{DefaultContext, Operation};

use crate::member::{Membership, memberships};

/// The system parameter that lets a user belong to several sales teams (`true`/`false`, unset
/// means `false`, as).
pub const MEMBERSHIP_MULTI_KEY: &str = "sales_team.membership_multi";

/// The external id of the default team (`sales_team/data/crm_team_data.xml:4-9`).
pub const DEFAULT_TEAM_XMLID: &str = "team_sales_department";

/// A badge colour index from 1 to 11, as the reference's random default.
pub(crate) fn random_color(_ctx: &DefaultContext) -> Value {
    // the clock's nanoseconds are variety enough for a badge colour; no generator dependency
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    Value::Int(i64::from(nanos % 11) + 1)
}

/// The declared parameter of this addon: the multiple-membership switch, off by default
/// (B4).
fn parameters() -> Result<Vec<erp_sdk::settings::ParameterDecl>> {
    Ok(vec![erp_sdk::settings::ParameterDecl::boolean(
        MEMBERSHIP_MULTI_KEY,
        "Multiple sales team memberships",
        false,
    )?])
}
erp_sdk::system_parameters!(parameters);

/// Whether a user may belong to several teams: [`MEMBERSHIP_MULTI_KEY`].
///
/// # Errors
/// `Validation` when the stored value is not a flag.
pub(crate) async fn membership_multi(tx: &mut Transaction) -> Result<bool> {
    erp_sdk::params::get_bool(tx, &crate::ADDON, MEMBERSHIP_MULTI_KEY).await
}

/// `crm.team`.
#[model(
    name = "crm.team",
    description = "Sales Team",
    inherit = [MailThread],
    order = "sequence ASC, create_date DESC, id DESC",
    company_access = direct(field = "company_id", branch = "exact", empty = "allow"),
)]
pub struct CrmTeam {
    /// Team name.
    #[field(char, required = true, translate = true, string = "Sales Team")]
    pub name: Option<Arc<str>>,
    /// Display order.
    #[field(integer, default = 10, string = "Sequence")]
    pub sequence: i32,
    /// An archived team is hidden but kept.
    #[field(boolean, default = true, string = "Active")]
    pub active: bool,
    /// The company of the team; empty: every company.
    #[field(many2one, comodel = "res.company", index = true, string = "Company")]
    pub company_id: Option<RecId>,
    /// The currency of the team's company.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// The team leader.
    #[field(
        many2one,
        comodel = "res.users",
        index = "btree_not_null",
        string = "Team Leader"
    )]
    pub user_id: Option<RecId>,
    /// The value of [`MEMBERSHIP_MULTI_KEY`], for the form.
    #[field(
        boolean,
        compute = "_compute_is_membership_multi",
        string = "Multiple Memberships Allowed"
    )]
    pub is_membership_multi: bool,
    /// The users of the active memberships. Writing it creates, archives and restores memberships.
    #[field(
        many2many,
        comodel = "res.users",
        compute = "_compute_member_ids",
        inverse = "_inverse_member_ids",
        string = "Salespersons"
    )]
    pub member_ids: Vec<RecId>,
    /// The memberships of the team.
    #[field(
        one2many,
        comodel = "crm.team.member",
        inverse_name = "crm_team_id",
        string = "Sales Team Members"
    )]
    pub crm_team_member_ids: Vec<RecId>,
    /// Every membership of the team, archived ones included.
    #[field(
        many2many,
        comodel = "crm.team.member",
        compute = "_compute_crm_team_member_all_ids",
        string = "All Memberships"
    )]
    pub crm_team_member_all_ids: Vec<RecId>,
    /// Colour index of the dashboard card.
    #[field(integer, default_fn = random_color, string = "Color Index")]
    pub color: i32,
    /// The users who show the team on their dashboard.
    #[field(
        many2many,
        comodel = "res.users",
        relation = "team_favorite_user_rel",
        column1 = "team_id",
        column2 = "user_id",
        check_company = false,
        string = "Favorite Members"
    )]
    pub favorite_user_ids: Vec<RecId>,
    /// Whether the current user shows the team on the dashboard; writing it adds or removes them.
    #[field(
        boolean,
        compute = "_compute_is_favorite",
        inverse = "_inverse_is_favorite",
        string = "Show on dashboard"
    )]
    pub is_favorite: bool,
}

#[methods(CrmTeam, addon = "sales_team")]
impl CrmTeam {
    /// Posting on a team needs read access.
    #[method(override, sealed)]
    async fn _mail_post_access(
        _this: &RecordSet<CrmTeam>,
        _tx: &mut Transaction,
        _sup: Super,
    ) -> Result<Operation> {
        Ok(Operation::Read)
    }

    #[compute(depends = ["sequence"])]
    async fn _compute_is_membership_multi(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
        out: &mut Assign<'_, CrmTeam>,
    ) -> Result<()> {
        let multi = membership_multi(tx).await?;
        for index in 0..this.len() {
            out.set(index, CrmTeam::IS_MEMBERSHIP_MULTI, multi)?;
        }
        Ok(())
    }

    #[compute(depends = ["crm_team_member_ids", "crm_team_member_ids.user_id", "crm_team_member_ids.active"])]
    async fn _compute_member_ids(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
        out: &mut Assign<'_, CrmTeam>,
    ) -> Result<()> {
        let ids = this.as_records().ids();
        let rows = memberships(
            tx,
            this.env(),
            &Domain::condition("crm_team_id", Operator::In, ids.clone())?,
        )
        .await?;
        let mut users: BTreeMap<i64, Vec<RecId>> = BTreeMap::new();
        for row in rows.iter().filter(|row| row.active) {
            users.entry(row.team).or_default().push(row.user_id()?);
        }
        for (index, id) in ids.iter().enumerate() {
            out.set(
                index,
                CrmTeam::MEMBER_IDS,
                users.remove(id).unwrap_or_default(),
            )?;
        }
        Ok(())
    }

    #[compute(depends = ["crm_team_member_ids"])]
    async fn _compute_crm_team_member_all_ids(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
        out: &mut Assign<'_, CrmTeam>,
    ) -> Result<()> {
        let ids = this.as_records().ids();
        let rows = memberships(
            tx,
            this.env(),
            &Domain::condition("crm_team_id", Operator::In, ids.clone())?,
        )
        .await?;
        for (index, id) in ids.iter().enumerate() {
            let mut all = Vec::new();
            for row in rows.iter().filter(|row| row.team == *id) {
                all.push(RecId::from_real(row.id).ok_or_else(|| {
                    Error::invariant(format!(
                        "crm.team.member id {} is not a database id",
                        row.id
                    ))
                })?);
            }
            out.set(index, CrmTeam::CRM_TEAM_MEMBER_ALL_IDS, all)?;
        }
        Ok(())
    }

    /// Make the active memberships of each team exactly the written users: create the missing
    /// ones, restore archived ones of a written user, archive the rest.
    #[method]
    async fn _inverse_member_ids(this: &RecordSet<CrmTeam>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        let ids = records.ids();
        if ids.is_empty() {
            return Ok(());
        }
        let mut wanted: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
        for (id, users) in ids.iter().zip(this.mapped(tx, CrmTeam::MEMBER_IDS).await?) {
            wanted.insert(*id, users.iter().map(|user| user.get()).collect());
        }
        let rows = memberships(
            tx,
            this.env(),
            &Domain::condition("crm_team_id", Operator::In, ids.clone())?,
        )
        .await?;
        let (mut restore, mut archive) = (Vec::new(), Vec::new());
        let mut known: BTreeSet<(i64, i64)> = BTreeSet::new();
        for row in &rows {
            let keep = wanted
                .get(&row.team)
                .is_some_and(|users| users.contains(&row.user));
            known.insert((row.team, row.user));
            match (keep, row.active) {
                (true, false) => restore.push(row.id),
                (false, true) => archive.push(row.id),
                _ => {}
            }
        }
        let members = this
            .env()
            .records("crm.team.member")?
            .with_context([("active_test", Value::Bool(false))]);
        if !archive.is_empty() {
            members
                .browse(archive)?
                .write(tx, RawVals::new().set_cell("active", Cell::Bool(false)))
                .await?;
        }
        if !restore.is_empty() {
            members
                .browse(restore)?
                .write(tx, RawVals::new().set_cell("active", Cell::Bool(true)))
                .await?;
        }
        let missing: Vec<RawVals> = wanted
            .iter()
            .flat_map(|(team, users)| users.iter().map(move |user| (*team, *user)))
            .filter(|pair| !known.contains(pair))
            .map(|(team, user)| {
                RawVals::new()
                    .set_cell("crm_team_id", Cell::Id(team))
                    .set_cell("user_id", Cell::Id(user))
            })
            .collect();
        if !missing.is_empty() {
            members.create(tx, missing).await?;
        }
        Ok(())
    }

    #[compute(depends = ["favorite_user_ids"], depends_context = ["uid"])]
    async fn _compute_is_favorite(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
        out: &mut Assign<'_, CrmTeam>,
    ) -> Result<()> {
        let uid = this.env().uid();
        let favorites = this.mapped(tx, CrmTeam::FAVORITE_USER_IDS).await?;
        for (index, users) in favorites.into_iter().enumerate() {
            out.set(
                index,
                CrmTeam::IS_FAVORITE,
                users.iter().any(|user| user.get() == uid),
            )?;
        }
        Ok(())
    }

    /// Add or remove the current user from the favorites of each team, as the superuser: a
    /// salesperson who may only read a team still chooses their own dashboard.
    #[method]
    async fn _inverse_is_favorite(this: &RecordSet<CrmTeam>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        let uid = this.env().uid();
        let user = RecId::from_real(uid)
            .ok_or_else(|| Error::programming("crm.team favorites need a real user id"))?;
        let flags = this.mapped(tx, CrmTeam::IS_FAVORITE).await?;
        let (mut add, mut remove) = (Vec::new(), Vec::new());
        for (id, flag) in records.rec_ids().iter().zip(flags) {
            if flag {
                add.push(*id)
            } else {
                remove.push(*id)
            }
        }
        let teams = records.sudo(true);
        if !add.is_empty() {
            teams
                .browse_ids(add)
                .write(
                    tx,
                    RawVals::new().set_commands("favorite_user_ids", [Command::link(user)]),
                )
                .await?;
        }
        if !remove.is_empty() {
            teams
                .browse_ids(remove)
                .write(
                    tx,
                    RawVals::new().set_commands("favorite_user_ids", [Command::unlink(user)]),
                )
                .await?;
        }
        Ok(())
    }

    /// A team of a company accepts only members allowed in that company.
    #[constrains("company_id")]
    async fn _check_company_members(this: &RecordSet<CrmTeam>, tx: &mut Transaction) -> Result<()> {
        let ids = this.as_records().ids();
        let rows = memberships(
            tx,
            this.env(),
            &Domain::condition("crm_team_id", Operator::In, ids)?,
        )
        .await?;
        crate::member::check_companies(tx, &rows).await
    }

    /// Creation does not subscribe the creator, and the creator and the members show the team on
    /// their dashboard (the creator: the reference's default favorite).
    #[method(override)]
    async fn create(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
        vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let quiet = this
            .as_records()
            .with_context([("mail_create_nosubscribe", Value::Bool(true))]);
        let teams = sup.call(tx, &quiet, (vals_list,)).await?;
        add_members_to_favorites(tx, &teams, true).await?;
        Ok(teams)
    }

    /// A new member list also adds the members to the favorites.
    #[method(override)]
    async fn write(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        let members_written = vals.contains("member_ids") || vals.commands("member_ids").is_some();
        sup.call(tx, this.as_records(), (vals,)).await?;
        if members_written {
            add_members_to_favorites(tx, this.as_records(), false).await?;
        }
        Ok(())
    }

    /// Install-time default (`sales_team/data/crm_team_data.xml:4-13`): the
    /// default team is led by the administrator, who is its one member. The administrator is the
    /// oldest active internal user holding `base.group_system`. Called by a `<function>` inside the
    /// `noupdate` data, so it runs on install only and an upgrade never brings back a membership
    /// an administrator removed. A database without an active administrator yet (the addon
    /// installed before the first administrator was created) keeps the team without leader and
    /// member.
    ///
    /// # Errors
    /// `Invariant` when the default team's record is missing.
    #[method]
    #[data_callable(model)]
    async fn _sales_team_seed_default_team(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
    ) -> Result<()> {
        tx.flush().await?;
        let team = tx
            .query(
                "SELECT res_id::int8 FROM ir_model_data WHERE module = 'sales_team' \
                 AND name = $1::text AND model = 'crm.team'",
                &[&DEFAULT_TEAM_XMLID],
            )
            .await?;
        let Some(team) = team.first().map(|row| row.get::<_, i64>(0)) else {
            return Err(Error::invariant(
                "the default sales team sales_team.team_sales_department is missing: \
                 its data file did not load",
            ));
        };
        let admin = tx
            .query(
                "SELECT u.id::int8 FROM res_users u \
                 JOIN res_groups_users_rel r ON r.uid = u.id \
                 JOIN ir_model_data d ON d.model = 'res.groups' AND d.res_id = r.gid \
                  AND d.module = 'base' AND d.name = 'group_system' \
                 WHERE u.active AND NOT coalesce(u.share, false) ORDER BY u.id LIMIT 1",
                &[],
            )
            .await?;
        let Some(admin) = admin.first().map(|row| row.get::<_, i64>(0)) else {
            return Ok(());
        };
        let env = this.env().sudo(true);
        env.records("crm.team")?
            .browse(vec![team])?
            .write(tx, RawVals::new().set_cell("user_id", Cell::Id(admin)))
            .await?;
        env.records("crm.team.member")?
            .create(
                tx,
                vec![
                    RawVals::new()
                        .set_cell("crm_team_id", Cell::Id(team))
                        .set_cell("user_id", Cell::Id(admin)),
                ],
            )
            .await?;
        Ok(())
    }

    /// The team of a sales document for `user_id` (the current user when `None`), in the order of
    /// the reference, where `company_id` replaces its filter domain and
    /// `default_team` its context key (typed arguments, R7):
    ///
    /// 1. a team the user leads or belongs to that fits `company_id` (the document's company: teams
    ///    of that company or without one), `default_team` when it is among them;
    /// 2. any team the user leads or belongs to, `default_team` when it is among them;
    /// 3. `default_team`;
    /// 4. the first team that fits `company_id`;
    /// 5. the first team.
    ///
    /// Only teams of the user's companies that the environment allows, or without a company, are
    /// considered. `None` when there is none.
    ///
    /// # Errors
    /// The errors of the searches.
    #[method]
    async fn _get_default_team_id(
        this: &RecordSet<CrmTeam>,
        tx: &mut Transaction,
        user_id: Option<i64>,
        company_id: Option<i64>,
        default_team: Option<i64>,
    ) -> Result<Option<i64>> {
        let env = this.env();
        let user = user_id.unwrap_or_else(|| env.uid());
        let user_companies: Vec<i64> = match env
            .sudo(true)
            .records("res.users")?
            .browse(vec![user])?
            .cell(tx, "company_ids")
            .await?
        {
            Cell::Ids(ids) => ids.iter().map(|id| id.get()).collect(),
            other => {
                return Err(Error::invariant(format!(
                    "res.users.company_ids is not an id list: {other:?}"
                )));
            }
        };
        // a System principal has no allowed companies of its own: the user's companies then
        let current: Vec<i64> = env
            .companies()?
            .iter()
            .map(|company| company.get())
            .collect();
        let allowed: Vec<i64> = user_companies
            .into_iter()
            .filter(|company| current.is_empty() || current.contains(company))
            .collect();
        let visible = Domain::condition("company_id", Operator::Eq, false)?
            | Domain::condition("company_id", Operator::In, allowed)?;
        let fits = |team_company: Option<i64>| match company_id {
            Some(company) => team_company.is_none_or(|own| own == company),
            None => true,
        };
        let teams = env.records("crm.team")?;
        let mine = teams
            .search(
                tx,
                &(visible.clone()
                    & (Domain::condition("user_id", Operator::Eq, user)?
                        | Domain::condition("crm_team_member_ids.user_id", Operator::Eq, user)?)),
            )
            .await?;
        let mine = with_companies(tx, mine).await?;
        let pick = |candidates: &[(i64, Option<i64>)], filtered: bool| {
            let pool: Vec<i64> = candidates
                .iter()
                .filter(|(_, company)| !filtered || fits(*company))
                .map(|(id, _)| *id)
                .collect();
            default_team
                .filter(|team| pool.contains(team))
                .or_else(|| pool.first().copied())
        };
        if let Some(team) = pick(&mine, true).or_else(|| pick(&mine, false)) {
            return Ok(Some(team));
        }
        if default_team.is_some() {
            return Ok(default_team);
        }
        let visible_teams = teams.search(tx, &visible).await?;
        let all = with_companies(tx, visible_teams).await?;
        let first_fit = all
            .iter()
            .find(|(_, company)| fits(*company))
            .map(|(id, _)| *id);
        Ok(first_fit.or_else(|| all.first().map(|(id, _)| *id)))
    }
}

/// The ids of `teams` with their companies, in the order of `teams`.
async fn with_companies(tx: &mut Transaction, teams: Records) -> Result<Vec<(i64, Option<i64>)>> {
    let set = RecordSet::<CrmTeam>::try_from(teams)?;
    let companies = set.mapped(tx, CrmTeam::COMPANY_ID).await?;
    Ok(set
        .as_records()
        .ids()
        .into_iter()
        .zip(companies)
        .map(|(id, company)| (id, company.map(|company| company.get())))
        .collect())
}

/// Link the members (and, on create, the acting user) of `teams` to their favorites, as the
/// superuser.
async fn add_members_to_favorites(
    tx: &mut Transaction,
    teams: &Records,
    creator: bool,
) -> Result<()> {
    if teams.is_empty() {
        return Ok(());
    }
    let set = RecordSet::<CrmTeam>::try_from(teams.clone())?;
    let members = set.mapped(tx, CrmTeam::MEMBER_IDS).await?;
    let acting = RecId::from_real(teams.env().uid());
    let sudo = teams.sudo(true);
    for (team, users) in teams.rec_ids().iter().zip(members) {
        let mut links: Vec<Command> = users.into_iter().map(Command::link).collect();
        if let Some(user) = acting.filter(|_| creator) {
            links.push(Command::link(user));
        }
        if !links.is_empty() {
            sudo.browse_ids([*team])
                .write(tx, RawVals::new().set_commands("favorite_user_ids", links))
                .await?;
        }
    }
    Ok(())
}

impl Membership {
    /// The user of the membership as a record id.
    pub(crate) fn user_id(&self) -> Result<RecId> {
        RecId::from_real(self.user)
            .ok_or_else(|| Error::invariant(format!("crm.team.member {} has no user", self.id)))
    }
}
