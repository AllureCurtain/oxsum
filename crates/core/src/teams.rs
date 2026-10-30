//! Teams: the budget layer between members and the organization credit line
//! (issue #440).
//!
//! A team is a named subgroup of one organization's members carrying its own
//! `budget_limit_minor` — the middle the key → member → organization hierarchy
//! missed. A member belongs to at most one team (`memberships.team_id`,
//! nullable; `None` means no team cap). The layer sits between the member
//! budget and the organization credit line inside `hold_for_key`'s locked
//! window: when the minting member's team carries a cap, the team's committed
//! spend — settled charges plus outstanding holds summed over every key the
//! team's members minted — is read from the ledger's own postings by
//! `team_committed_in`, never a side counter, so the figure cannot drift from
//! the books.
//!
//! Committed spend follows the roster, not the team's history: moving a member
//! between teams changes which cap their *next* hold is checked against, and
//! the money their keys already spent moves with them — the same attribution
//! `member_committed_in` gives per member, aggregated. Dissolving a team nulls
//! the memberships' `team_id` (ON DELETE SET NULL): the members stay, simply
//! uncapped at the team layer again.
//!
//! Management is a person's action under the membership rules — an owner or an
//! admin, never an API key — so the endpoints borrow [`MembershipActor`] rather
//! than growing a second role vocabulary.

use serde::Serialize;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::orgs::MembershipActor;

/// One team as the organization surface answers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub team_id: Uuid,
    /// The member-count the subgroup currently holds — the roster the
    /// committed sum walks.
    pub name: String,
    /// The team's committed-spend cap; `None` carries no cap.
    pub budget_limit_minor: Option<i64>,
    /// How many memberships the team groups today.
    pub members: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Db {
    /// The teams of an organization, oldest first: what the organization
    /// surface lists.
    ///
    /// Reading the organization's own structure is any member's right — the
    /// same rule `members` answers under; the writes on the same paths are the
    /// owner/admin-gated ones.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn teams(&self, organization_id: Uuid) -> Result<Vec<Team>, WalletError> {
        let rows = sqlx::query(
            "SELECT t.team_id, t.name, t.budget_limit_minor, t.created_at, \
                    (SELECT count(*) FROM oxsum.memberships m WHERE m.team_id = t.team_id) AS members \
             FROM oxsum.teams t WHERE t.organization_id = $1 ORDER BY t.created_at",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(team_from_row).collect()
    }

    /// Creates a team under the organization. `budget_limit_minor` is the cap
    /// the team's committed spend is checked against inside `hold_for_key`;
    /// `None` creates an uncapped subgroup.
    ///
    /// The unique `(organization, name)` is the write's anchor: a retry with
    /// the same name answers the standing row rather than minting a second
    /// team — the same read-back convention every row write keeps.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for a member; [`WalletError::InvalidInput`]
    /// for an empty name or a negative cap.
    pub async fn create_team(
        &self,
        organization_id: Uuid,
        acting: MembershipActor,
        name: &str,
        budget_limit_minor: Option<i64>,
    ) -> Result<Team, WalletError> {
        acting.authorized()?;
        let name = validate_name(name)?;
        validate_budget(budget_limit_minor)?;
        let mut tx = self.pool().begin().await?;
        // The name unique is the idempotency anchor: a replayed create reads
        // the standing row back, never minting a second team of the name.
        let row = sqlx::query(
            "INSERT INTO oxsum.teams (organization_id, name, budget_limit_minor) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (organization_id, name) DO UPDATE SET name = EXCLUDED.name \
             RETURNING team_id, name, budget_limit_minor, created_at, 0::bigint AS members",
        )
        .bind(organization_id)
        .bind(&name)
        .bind(budget_limit_minor)
        .fetch_one(&mut *tx)
        .await?;
        let team = team_from_row(&row)?;
        tx.commit().await?;
        Ok(team)
    }

    /// Changes a team's name and/or budget cap in one write — the double
    /// options the PATCH carries: `None` halves stay, `Some(None)` budget
    /// clears the cap, `Some(Some(_))` sets it.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for a member; [`WalletError::InvalidInput`]
    /// when neither field is present, for an empty name or a negative cap;
    /// [`WalletError::NotFound`] for a team outside the organization.
    pub async fn update_team(
        &self,
        organization_id: Uuid,
        acting: MembershipActor,
        team_id: Uuid,
        name: Option<&str>,
        budget_limit_minor: Option<Option<i64>>,
    ) -> Result<Team, WalletError> {
        acting.authorized()?;
        if name.is_none() && budget_limit_minor.is_none() {
            return Err(WalletError::InvalidInput(
                "at least one of name and budgetLimitMinor must be present".into(),
            ));
        }
        let name = name.map(validate_name).transpose()?;
        if let Some(budget) = budget_limit_minor {
            validate_budget(budget)?;
        }
        let mut tx = self.pool().begin().await?;
        let mut target = sqlx::query(
            "SELECT team_id, name, budget_limit_minor, created_at, 0::bigint AS members \
             FROM oxsum.teams WHERE organization_id = $1 AND team_id = $2 FOR UPDATE",
        )
        .bind(organization_id)
        .bind(team_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| team_from_row(&row))
        .transpose()?
        .ok_or_else(|| WalletError::NotFound("no team with that id".into()))?;
        if let Some(name) = name
            && target.name != name
        {
            sqlx::query(
                "UPDATE oxsum.teams SET name = $3, updated_at = now() \
                 WHERE organization_id = $1 AND team_id = $2",
            )
            .bind(organization_id)
            .bind(team_id)
            .bind(&name)
            .execute(&mut *tx)
            .await?;
            target.name = name;
        }
        if let Some(budget) = budget_limit_minor
            && target.budget_limit_minor != budget
        {
            sqlx::query(
                "UPDATE oxsum.teams SET budget_limit_minor = $3, updated_at = now() \
                 WHERE organization_id = $1 AND team_id = $2",
            )
            .bind(organization_id)
            .bind(team_id)
            .bind(budget)
            .execute(&mut *tx)
            .await?;
            target.budget_limit_minor = budget;
        }
        tx.commit().await?;
        Ok(target)
    }

    /// Dissolves a team: the row goes, every membership it grouped keeps its
    /// seat with `team_id` nulled — the member layer of the hierarchy stays
    /// and the team cap simply stops applying.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for a member; [`WalletError::NotFound`] for a
    /// team outside the organization.
    pub async fn delete_team(
        &self,
        organization_id: Uuid,
        acting: MembershipActor,
        team_id: Uuid,
    ) -> Result<Team, WalletError> {
        acting.authorized()?;
        let mut tx = self.pool().begin().await?;
        let target = sqlx::query(
            "DELETE FROM oxsum.teams WHERE organization_id = $1 AND team_id = $2 \
             RETURNING team_id, name, budget_limit_minor, created_at, 0::bigint AS members",
        )
        .bind(organization_id)
        .bind(team_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| team_from_row(&row))
        .transpose()?
        .ok_or_else(|| WalletError::NotFound("no team with that id".into()))?;
        tx.commit().await?;
        Ok(target)
    }
}

/// Maps a row of the teams projection to [`Team`].
fn team_from_row(row: &sqlx::postgres::PgRow) -> Result<Team, WalletError> {
    Ok(Team {
        team_id: row.try_get("team_id")?,
        name: row.try_get("name")?,
        budget_limit_minor: row.try_get("budget_limit_minor")?,
        members: row.try_get("members")?,
        created_at: row.try_get("created_at")?,
    })
}

fn validate_name(name: &str) -> Result<String, WalletError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(WalletError::InvalidInput(
            "a team name may not be empty".into(),
        ));
    }
    Ok(name.to_owned())
}

fn validate_budget(budget_limit_minor: Option<i64>) -> Result<(), WalletError> {
    if budget_limit_minor.is_some_and(|limit| limit < 0) {
        return Err(WalletError::InvalidInput(
            "budgetLimitMinor may not be negative".into(),
        ));
    }
    Ok(())
}
