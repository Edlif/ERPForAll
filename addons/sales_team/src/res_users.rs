// SPDX-License-Identifier: MIT OR Apache-2.0
//! The team fields of a user: the
//! memberships, the teams of the active ones, and the main team (the oldest active membership),
//! stored for record rules and defaults. Archiving a user archives their memberships.

use erp_sdk::prelude::*;

use crate::member::memberships;

/// `res.users` with its sales teams.
#[extend(ResUsers, addon = "sales_team")]
pub struct UsersSalesTeam {
    /// The user's memberships.
    #[field(
        one2many,
        comodel = "crm.team.member",
        inverse_name = "user_id",
        string = "Sales Team Members"
    )]
    pub crm_team_member_ids: Vec<RecId>,
    /// The teams of the user's active memberships.
    #[field(
        many2many,
        comodel = "crm.team",
        compute = "_compute_crm_team_ids",
        string = "Sales Teams"
    )]
    pub crm_team_ids: Vec<RecId>,
    /// The main team: the team of the oldest active membership.
    #[field(
        many2one,
        comodel = "crm.team",
        compute = "_compute_sale_team_id",
        compute_sudo = true,
        store = true,
        string = "User Sales Team"
    )]
    pub sale_team_id: Option<RecId>,
}

/// The active teams of each user of `this`, oldest membership first.
async fn active_teams(this: &RecordSet<ResUsers>, tx: &mut Transaction) -> Result<Vec<Vec<RecId>>> {
    let ids = this.as_records().ids();
    let env = this.env().sudo(true);
    let rows = memberships(
        tx,
        &env,
        &(Domain::condition("user_id", Operator::In, ids.clone())?
            & Domain::condition("active", Operator::Eq, true)?),
    )
    .await?;
    let mut teams = Vec::with_capacity(ids.len());
    for id in &ids {
        let mut own = Vec::new();
        for row in rows.iter().filter(|row| row.user == *id) {
            let team = RecId::from_real(row.team).ok_or_else(|| {
                Error::invariant(format!("crm.team.member {} has no team", row.id))
            })?;
            if !own.contains(&team) {
                own.push(team);
            }
        }
        teams.push(own);
    }
    Ok(teams)
}

#[methods(ResUsers, addon = "sales_team")]
impl UsersSalesTeam {
    #[compute(depends = ["crm_team_member_ids.crm_team_id", "crm_team_member_ids.active"])]
    async fn _compute_crm_team_ids(
        this: &RecordSet<ResUsers>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ResUsers>,
    ) -> Result<()> {
        for (index, teams) in active_teams(this, tx).await?.into_iter().enumerate() {
            out.set(index, UsersSalesTeam::CRM_TEAM_IDS, teams)?;
        }
        Ok(())
    }

    #[compute(depends = ["crm_team_member_ids.crm_team_id", "crm_team_member_ids.active", "crm_team_member_ids.create_date"])]
    async fn _compute_sale_team_id(
        this: &RecordSet<ResUsers>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ResUsers>,
    ) -> Result<()> {
        for (index, teams) in active_teams(this, tx).await?.into_iter().enumerate() {
            out.set(index, UsersSalesTeam::SALE_TEAM_ID, teams.first().copied())?;
        }
        Ok(())
    }

    /// Archiving users archives their memberships first.
    #[method(override)]
    async fn action_archive(
        this: &RecordSet<ResUsers>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let ids = this.as_records().ids();
        if !ids.is_empty() {
            let members = this
                .env()
                .records("crm.team.member")?
                .search(tx, &Domain::condition("user_id", Operator::In, ids)?)
                .await?;
            if !members.is_empty() {
                members
                    .write(tx, RawVals::new().set_cell("active", Cell::Bool(false)))
                    .await?;
            }
        }
        sup.call(tx, this.as_records(), ()).await
    }
}
