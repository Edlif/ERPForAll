// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use erp_sdk::ErrorKind;
use erp_sdk::prelude::*;
use erp_testkit::Harness;

async fn harness() -> Harness {
    Harness::builder([
        &erp_addon_base::ADDON,
        &erp_addon_mail::ADDON,
        &erp_addon_sales_team::ADDON,
    ])
    .build()
    .await
    .unwrap()
}

async fn user(tx: &mut Transaction, login: &str) -> i64 {
    tx.query(
        "INSERT INTO res_users (name, login, active) VALUES ($1::text, $1::text, true) RETURNING id::int8",
        &[&login],
    )
    .await
    .unwrap()[0]
        .get(0)
}

async fn team(tx: &mut Transaction, name: &str) -> i64 {
    tx.env()
        .records("crm.team")
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("name", Cell::Str(name.into()))],
        )
        .await
        .unwrap()
        .ids()[0]
}

async fn join(tx: &mut Transaction, team: i64, user: i64) -> Result<i64> {
    let created = tx
        .env()
        .records("crm.team.member")?
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("crm_team_id", Cell::Id(team))
                    .set_cell("user_id", Cell::Id(user)),
            ],
        )
        .await?;
    Ok(created.ids()[0])
}

/// The (team, active) pairs of `user`'s memberships, oldest first.
async fn memberships_of(tx: &mut Transaction, user: i64) -> Vec<(i64, bool)> {
    tx.flush().await.unwrap();
    tx.query(
        "SELECT crm_team_id::int8, active FROM crm_team_member WHERE user_id = $1 ORDER BY id",
        &[&user],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), row.get(1)))
    .collect()
}

async fn set_multi(tx: &mut Transaction, on: bool) {
    let _: bool = tx
        .env()
        .records("ir.config_parameter")
        .unwrap()
        .call(
            tx,
            "set_bool",
            (
                erp_addon_sales_team::MEMBERSHIP_MULTI_KEY.to_owned(),
                Some(on),
            ),
        )
        .await
        .unwrap();
}

/// Without the parameter a user keeps one active team: joining a second team archives the first.
#[tokio::test]
async fn joining_a_team_archives_the_other_membership_in_single_mode() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let (north, south) = (team(&mut tx, "North").await, team(&mut tx, "South").await);
    let ana = user(&mut tx, "ana").await;
    join(&mut tx, north, ana).await.unwrap();
    join(&mut tx, south, ana).await.unwrap();
    assert_eq!(
        memberships_of(&mut tx, ana).await,
        [(north, false), (south, true)]
    );

    // restoring the first archives the second
    tx.env()
        .records("crm.team.member")
        .unwrap()
        .with_context([("active_test", Value::Bool(false))])
        .search(
            &mut tx,
            &(Domain::condition("crm_team_id", Operator::Eq, north).unwrap()
                & Domain::condition("user_id", Operator::Eq, ana).unwrap()),
        )
        .await
        .unwrap()
        .write(&mut tx, RawVals::new().set_cell("active", Cell::Bool(true)))
        .await
        .unwrap();
    assert_eq!(
        memberships_of(&mut tx, ana).await,
        [(north, true), (south, false)]
    );
    tx.rollback().await.unwrap();
}

/// With `sales_team.membership_multi` on, a user is an active member of several teams.
#[tokio::test]
async fn the_system_parameter_allows_several_memberships() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    set_multi(&mut tx, true).await;
    let (north, south) = (team(&mut tx, "North").await, team(&mut tx, "South").await);
    let ana = user(&mut tx, "ana").await;
    join(&mut tx, north, ana).await.unwrap();
    join(&mut tx, south, ana).await.unwrap();
    assert_eq!(
        memberships_of(&mut tx, ana).await,
        [(north, true), (south, true)]
    );
    let multi = tx
        .env()
        .records("crm.team")
        .unwrap()
        .browse(vec![north])
        .unwrap()
        .cell(&mut tx, "is_membership_multi")
        .await
        .unwrap();
    assert_eq!(multi, Cell::Bool(true));
    tx.rollback().await.unwrap();
}

/// An active pair of team and user exists once.
#[tokio::test]
async fn a_duplicate_active_membership_is_refused() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    set_multi(&mut tx, true).await;
    let north = team(&mut tx, "North").await;
    let ana = user(&mut tx, "ana").await;
    join(&mut tx, north, ana).await.unwrap();
    let error = tx
        .savepoint(async |tx| join(tx, north, ana).await.map(|_| ()))
        .await
        .expect_err("a second active membership of the same pair");
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert!(
        error
            .message()
            .contains("active member of a team only once"),
        "{error:?}"
    );
    tx.rollback().await.unwrap();
}

/// Writing `member_ids` creates the missing memberships and archives the removed ones; the
/// members and the creator show the team on their dashboard.
#[tokio::test]
async fn writing_member_ids_keeps_the_memberships_in_step() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let north = team(&mut tx, "North").await;
    let (ana, ben) = (user(&mut tx, "ana").await, user(&mut tx, "ben").await);
    let teams = tx
        .env()
        .records("crm.team")
        .unwrap()
        .browse(vec![north])
        .unwrap();
    let both = [ana, ben].map(|id| RecId::from_real(id).unwrap());
    teams
        .write(
            &mut tx,
            RawVals::new().set_commands("member_ids", [Command::set(both)]),
        )
        .await
        .unwrap();
    assert_eq!(memberships_of(&mut tx, ana).await, [(north, true)]);
    assert_eq!(memberships_of(&mut tx, ben).await, [(north, true)]);
    let favorites = teams.cell(&mut tx, "favorite_user_ids").await.unwrap();
    let Cell::Ids(favorites) = favorites else {
        panic!("{favorites:?}")
    };
    let favorites: Vec<i64> = favorites.iter().map(|id| id.get()).collect();
    assert!(
        favorites.contains(&ana) && favorites.contains(&ben),
        "{favorites:?}"
    );

    teams
        .write(
            &mut tx,
            RawVals::new().set_commands("member_ids", [Command::set([both[0]])]),
        )
        .await
        .unwrap();
    assert_eq!(memberships_of(&mut tx, ben).await, [(north, false)]);
    tx.rollback().await.unwrap();
}

/// The main team of a user is the team of the oldest active membership, and it moves when that
/// membership is archived.
#[tokio::test]
async fn the_sale_team_of_a_user_is_the_oldest_active_membership() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    set_multi(&mut tx, true).await;
    let (north, south) = (team(&mut tx, "North").await, team(&mut tx, "South").await);
    let ana = user(&mut tx, "ana").await;
    let first = join(&mut tx, north, ana).await.unwrap();
    join(&mut tx, south, ana).await.unwrap();
    let users = tx
        .env()
        .records("res.users")
        .unwrap()
        .browse(vec![ana])
        .unwrap();
    assert_eq!(
        users.cell(&mut tx, "sale_team_id").await.unwrap(),
        Cell::Id(north)
    );
    tx.env()
        .records("crm.team.member")
        .unwrap()
        .browse(vec![first])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("active", Cell::Bool(false)),
        )
        .await
        .unwrap();
    assert_eq!(
        users.cell(&mut tx, "sale_team_id").await.unwrap(),
        Cell::Id(south)
    );
    tx.rollback().await.unwrap();
}

/// The default team of a document is a team the user belongs to, else the first visible team.
#[tokio::test]
async fn the_default_team_is_the_users_own_team() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let north = team(&mut tx, "North").await;
    let south = team(&mut tx, "South").await;
    let ana = user(&mut tx, "ana").await;
    let ben = user(&mut tx, "ben").await;
    join(&mut tx, south, ana).await.unwrap();
    let teams = tx.env().records("crm.team").unwrap();
    let own: Option<i64> = teams
        .call(
            &mut tx,
            "_get_default_team_id",
            (Some(ana), None::<i64>, None::<i64>),
        )
        .await
        .unwrap();
    assert_eq!(own, Some(south));
    let fallback: Option<i64> = teams
        .call(
            &mut tx,
            "_get_default_team_id",
            (Some(ben), None::<i64>, Some(north)),
        )
        .await
        .unwrap();
    // ben belongs to no team: the given default team wins over the first team
    assert_eq!(fallback, Some(north));
    tx.rollback().await.unwrap();
}

/// Tag names are unique.
#[tokio::test]
async fn tag_names_are_unique() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let tags = tx.env().records("crm.tag").unwrap();
    let vip = || vec![RawVals::new().set_cell("name", Cell::Str("VIP".into()))];
    let created = tags.create(&mut tx, vip()).await.unwrap();
    let color = created.cell(&mut tx, "color").await.unwrap();
    assert!(matches!(color, Cell::Int(1..=11)), "{color:?}");
    let error = tx
        .savepoint(async |tx| {
            tags.create(tx, vip()).await?;
            tx.flush().await
        })
        .await
        .expect_err("a second tag named VIP");
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    tx.rollback().await.unwrap();
}
