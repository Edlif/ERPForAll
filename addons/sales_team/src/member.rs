// SPDX-License-Identifier: MIT OR Apache-2.0
//! `crm.team.member`: the membership of a user in a sales team.
//!
//! An active pair of team and user exists once. Unless [`crate::MEMBERSHIP_MULTI_KEY`] is on, a
//! user has one active membership: creating or restoring one archives the user's others.
//! A team of a company accepts only users allowed in that company.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_mail::MailThread;
use erp_sdk::prelude::*;

use crate::team::membership_multi;

/// `crm.team.member`.
#[model(
    name = "crm.team.member",
    description = "Sales Team Member",
    inherit = [MailThread],
    order = "create_date ASC, id",
    company_access = parent(edge = "crm_team_id"),
)]
pub struct CrmTeamMember {
    /// The team.
    #[field(
        many2one,
        comodel = "crm.team",
        required = true,
        ondelete = "cascade",
        index = true,
        check_company = false,
        string = "Sales Team"
    )]
    pub crm_team_id: Option<RecId>,
    /// The salesperson.
    #[field(
        many2one,
        comodel = "res.users",
        required = true,
        ondelete = "cascade",
        index = true,
        string = "Salesperson"
    )]
    pub user_id: Option<RecId>,
    /// An archived membership is kept for history; only active ones count.
    #[field(boolean, default = true, string = "Active")]
    pub active: bool,
    /// The value of [`crate::MEMBERSHIP_MULTI_KEY`], for the form.
    #[field(
        boolean,
        compute = "_compute_is_membership_multi",
        string = "Multiple Memberships Allowed"
    )]
    pub is_membership_multi: bool,
    /// The salesperson's name.
    #[field(char, related = "user_id.name", string = "Name")]
    pub name: Option<Arc<str>>,
    /// The salesperson's e-mail.
    #[field(char, related = "user_id.partner_id.email", string = "Email")]
    pub email: Option<Arc<str>>,
    /// The salesperson's phone.
    #[field(char, related = "user_id.partner_id.phone", string = "Phone")]
    pub phone: Option<Arc<str>>,
    /// The salesperson's current company.
    #[field(
        many2one,
        comodel = "res.company",
        related = "user_id.company_id",
        string = "Company"
    )]
    pub company_id: Option<RecId>,
}

/// One membership row, archived ones included.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Membership {
    pub(crate) id: i64,
    pub(crate) team: i64,
    pub(crate) user: i64,
    pub(crate) active: bool,
}

/// The memberships matching `domain`, archived ones included, in the model's order (oldest
/// first).
///
/// # Errors
/// The errors of the search; `Invariant` for a row without team or user.
pub(crate) async fn memberships(
    tx: &mut Transaction,
    env: &Env,
    domain: &Domain,
) -> Result<Vec<Membership>> {
    let found = env
        .records("crm.team.member")?
        .with_context([("active_test", Value::Bool(false))])
        .search(tx, domain)
        .await?;
    let set = RecordSet::<CrmTeamMember>::try_from(found)?;
    let teams = set.mapped(tx, CrmTeamMember::CRM_TEAM_ID).await?;
    let users = set.mapped(tx, CrmTeamMember::USER_ID).await?;
    let active = set.mapped(tx, CrmTeamMember::ACTIVE).await?;
    let mut rows = Vec::with_capacity(teams.len());
    for (((id, team), user), active) in set
        .as_records()
        .ids()
        .into_iter()
        .zip(teams)
        .zip(users)
        .zip(active)
    {
        let (Some(team), Some(user)) = (team, user) else {
            return Err(Error::invariant(format!(
                "crm.team.member {id} has no team or no user"
            )));
        };
        rows.push(Membership {
            id,
            team: team.get(),
            user: user.get(),
            active,
        });
    }
    Ok(rows)
}

/// Refuse a membership whose team has a company the user is not allowed in.
///
/// # Errors
/// `User` naming the user, the company and the team.
pub(crate) async fn check_companies(tx: &mut Transaction, rows: &[Membership]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    tx.flush().await?;
    let teams: Vec<i64> = rows.iter().map(|row| row.team).collect();
    let users: Vec<i64> = rows.iter().map(|row| row.user).collect();
    let refused = tx
        .query(
            "SELECT u.login, c.name, t.name->>'en_US' FROM unnest($1::int8[], $2::int8[]) AS m(team, uid) \
             JOIN crm_team t ON t.id = m.team JOIN res_company c ON c.id = t.company_id \
             JOIN res_users u ON u.id = m.uid \
             WHERE NOT EXISTS (SELECT 1 FROM res_company_users_rel r \
                               WHERE r.user_id = m.uid AND r.cid = t.company_id) \
             ORDER BY u.login LIMIT 1",
            &[&teams, &users],
        )
        .await?;
    if let Some(row) = refused.first() {
        let (user, company, team): (String, String, Option<String>) =
            (row.get(0), row.get(1), row.get(2));
        return Err(Error::user(format!(
            "{user} cannot join the sales team {}: the team belongs to {company}, a company this user may not work in.",
            team.unwrap_or_default()
        )));
    }
    Ok(())
}

/// Archive the other active memberships of each (user, team) pair's user, so that a user keeps one
/// active team. `except` are the memberships being restored.
async fn archive_other_teams(
    tx: &mut Transaction,
    env: &Env,
    pairs: &[(i64, i64)],
    except: &[i64],
) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let users: Vec<i64> = pairs
        .iter()
        .map(|(user, _)| *user)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let target: BTreeMap<i64, i64> = pairs.iter().copied().collect();
    let existing = memberships(
        tx,
        env,
        &(Domain::condition("user_id", Operator::In, users)?
            & Domain::condition("active", Operator::Eq, true)?),
    )
    .await?;
    let archive: Vec<i64> = existing
        .iter()
        .filter(|row| !except.contains(&row.id) && target.get(&row.user) != Some(&row.team))
        .map(|row| row.id)
        .collect();
    if archive.is_empty() {
        return Ok(());
    }
    env.records("crm.team.member")?
        .with_context([("active_test", Value::Bool(false))])
        .browse(archive)?
        .write(tx, RawVals::new().set_cell("active", Cell::Bool(false)))
        .await
}

fn id_of(vals: &RawVals, field: &str) -> Option<i64> {
    match vals.get(field) {
        Some(Cell::Id(id)) => Some(*id),
        _ => None,
    }
}

#[methods(CrmTeamMember, addon = "sales_team")]
impl CrmTeamMember {
    #[compute(depends = ["crm_team_id"])]
    async fn _compute_is_membership_multi(
        this: &RecordSet<CrmTeamMember>,
        tx: &mut Transaction,
        out: &mut Assign<'_, CrmTeamMember>,
    ) -> Result<()> {
        let multi = membership_multi(tx).await?;
        for index in 0..this.len() {
            out.set(index, CrmTeamMember::IS_MEMBERSHIP_MULTI, multi)?;
        }
        Ok(())
    }

    /// An active pair of team and user exists once (archived
    /// duplicates are allowed, so this is not a unique index).
    #[constrains("crm_team_id", "user_id", "active")]
    async fn _check_membership(
        this: &RecordSet<CrmTeamMember>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let ids = this.as_records().ids();
        let own = memberships(tx, this.env(), &Domain::condition("id", Operator::In, ids)?).await?;
        let users: Vec<i64> = own.iter().map(|row| row.user).collect();
        let active = memberships(
            tx,
            this.env(),
            &(Domain::condition("user_id", Operator::In, users)?
                & Domain::condition("active", Operator::Eq, true)?),
        )
        .await?;
        let mut seen: BTreeMap<(i64, i64), usize> = BTreeMap::new();
        for row in &active {
            *seen.entry((row.team, row.user)).or_default() += 1;
        }
        let duplicates: Vec<String> = own
            .iter()
            .filter(|row| row.active && seen.get(&(row.team, row.user)).is_some_and(|n| *n > 1))
            .map(|row| format!("user {} in team {}", row.user, row.team))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !duplicates.is_empty() {
            return Err(Error::validation(format!(
                "A user can be an active member of a team only once; already a member: {}.",
                duplicates.join(", ")
            )));
        }
        Ok(())
    }

    #[constrains("crm_team_id", "user_id")]
    async fn _check_company_membership(
        this: &RecordSet<CrmTeamMember>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let ids = this.as_records().ids();
        let rows =
            memberships(tx, this.env(), &Domain::condition("id", Operator::In, ids)?).await?;
        check_companies(tx, &rows).await
    }

    /// In single-membership mode a new active membership archives the user's other ones; creation
    /// does not subscribe the creator.
    #[method(override)]
    async fn create(
        this: &RecordSet<CrmTeamMember>,
        tx: &mut Transaction,
        vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        if !membership_multi(tx).await? {
            let pairs: Vec<(i64, i64)> = vals_list
                .iter()
                .filter(|vals| !matches!(vals.get("active"), Some(Cell::Bool(false))))
                .filter_map(|vals| Some((id_of(vals, "user_id")?, id_of(vals, "crm_team_id")?)))
                .collect();
            archive_other_teams(tx, this.env(), &pairs, &[]).await?;
        }
        let quiet = this
            .as_records()
            .with_context([("mail_create_nosubscribe", Value::Bool(true))]);
        sup.call(tx, &quiet, (vals_list,)).await
    }

    /// In single-membership mode restoring memberships archives the users' other ones.
    /// A change of user or team is checked by the constraints only,
    /// as in the reference.
    #[method(override)]
    async fn write(
        this: &RecordSet<CrmTeamMember>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        if matches!(vals.get("active"), Some(Cell::Bool(true))) && !membership_multi(tx).await? {
            let ids = this.as_records().ids();
            let rows = memberships(
                tx,
                this.env(),
                &Domain::condition("id", Operator::In, ids.clone())?,
            )
            .await?;
            let pairs: Vec<(i64, i64)> = rows.iter().map(|row| (row.user, row.team)).collect();
            archive_other_teams(tx, this.env(), &pairs, &ids).await?;
        }
        sup.call(tx, this.as_records(), (vals,)).await
    }
}
