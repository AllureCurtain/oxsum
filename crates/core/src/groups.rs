//! Request groups (issue #508): a key/channel scoping dimension for
//! team-level isolation.
//!
//! A group is a named subset of one organization's channels. A key assigned to
//! a group may only reach that group's channels — "team X's traffic uses the
//! paid OpenAI channel, team Y's uses the cheaper Ollama mirror" — where
//! `model_allowlist` only ever limited models. A key in no group (the common
//! case, and every key minted before this landed) reaches every channel its
//! organization sees. A group with an empty channel set denies all channels.
//!
//! The layer is mutable config, not ledger: a group's channel set edits with
//! admin writes and a key's `group_id` moves, the same shape `teams` and
//! `model_fallbacks` took. The gateway reads the key's group channels at
//! admission and restricts the serving candidates to them, so a group change
//! takes effect on the next request — no version pinned to a hold, because a
//! hold names a channel, never a group.

use std::collections::{BTreeSet, HashSet};

use sqlx::Row;
use uuid::Uuid;

use crate::{Db, WalletError};

/// The longest one group name may be — the same bound a team name takes.
const MAX_NAME: usize = 64;

/// One request group as the admin surface answers it: the name and the channel
/// ids a grouped key may reach.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    /// The channels this group may serve through.
    pub channel_ids: Vec<Uuid>,
    /// Readable channel identities paired with `channel_ids` in order.
    pub channels: Vec<GroupChannel>,
}

/// Public identity for a channel in a group's access set.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupChannel {
    pub id: Uuid,
    pub name: String,
}

impl Db {
    /// The groups of an organization with their channel sets, oldest first.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn groups(&self, organization_id: Uuid) -> Result<Vec<Group>, WalletError> {
        let rows = sqlx::query(
            "SELECT g.id, g.name, \
                    COALESCE(array_agg(gc.channel_id ORDER BY gc.channel_id) FILTER (WHERE gc.channel_id IS NOT NULL), '{}') AS channel_ids, \
                    COALESCE(jsonb_agg(jsonb_build_object('id', c.channel_id, 'name', c.name) ORDER BY gc.channel_id) FILTER (WHERE gc.channel_id IS NOT NULL), '[]'::jsonb) AS channels \
             FROM oxsum.groups g \
             LEFT JOIN oxsum.group_channels gc ON gc.group_id = g.id \
             LEFT JOIN oxsum.channels c ON c.channel_id = gc.channel_id \
             WHERE g.org_id = $1 \
             GROUP BY g.id, g.name, g.created_at \
             ORDER BY g.created_at",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let channels: Vec<GroupChannel> = serde_json::from_value(row.try_get("channels")?)
                    .map_err(|error| {
                        WalletError::Storage(
                            doubleentry::storage::postgres::PostgresError::Malformed(format!(
                                "invalid group channel data: {error}"
                            )),
                        )
                    })?;
                Ok(Group {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    channel_ids: row.try_get("channel_ids")?,
                    channels,
                })
            })
            .collect()
    }

    /// Creates or replaces a group's channel set — `PUT`-shaped, idempotent on
    /// `(org, name)`. Every named channel must exist: a group referencing a
    /// channel id that is not a channel would scope keys to a serving that can
    /// never answer, and the honest refusal is at write time.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for an over-length or empty name, or a
    /// channel id that is not a channel; storage failures surface as
    /// [`WalletError`].
    pub async fn put_group(
        &self,
        organization_id: Uuid,
        name: &str,
        channel_ids: &[Uuid],
    ) -> Result<Group, WalletError> {
        let name = name.trim();
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(WalletError::InvalidInput(format!(
                "a group name is 1..={MAX_NAME} bytes"
            )));
        }
        let channel_ids: Vec<Uuid> = channel_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut tx = self.pool().begin().await?;
        let org_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.organizations WHERE organization_id = $1)",
        )
        .bind(organization_id)
        .fetch_one(&mut *tx)
        .await?;
        if !org_exists {
            return Err(WalletError::NotFound("organization not found".into()));
        }
        let found: Vec<Uuid> = sqlx::query_scalar(
            "SELECT channel_id FROM oxsum.channels WHERE channel_id = ANY($1) FOR KEY SHARE",
        )
        .bind(&channel_ids)
        .fetch_all(&mut *tx)
        .await?;
        if found.len() != channel_ids.len() {
            return Err(WalletError::InvalidInput(
                "a channelId is not a channel".into(),
            ));
        }
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO oxsum.groups (org_id, name) VALUES ($1, $2) \
             ON CONFLICT (org_id, name) DO UPDATE SET name = EXCLUDED.name \
             RETURNING id",
        )
        .bind(organization_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM oxsum.group_channels WHERE group_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for channel_id in &channel_ids {
            sqlx::query(
                "INSERT INTO oxsum.group_channels (group_id, channel_id) VALUES ($1, $2) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(id)
            .bind(channel_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        let channels = sqlx::query(
            "SELECT channel_id AS id, name FROM oxsum.channels WHERE channel_id = ANY($1) ORDER BY channel_id",
        )
        .bind(&channel_ids)
        .fetch_all(self.pool())
        .await?
        .iter()
        .map(|row| Ok(GroupChannel { id: row.try_get("id")?, name: row.try_get("name")? }))
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
        Ok(Group {
            id,
            name: name.to_owned(),
            channel_ids: channel_ids.to_vec(),
            channels,
        })
    }

    /// Drops an unused group. Assigned keys (including revoked keys) keep the
    /// group alive: deletion never implicitly broadens access.
    pub async fn delete_group(
        &self,
        organization_id: Uuid,
        name: &str,
    ) -> Result<bool, WalletError> {
        let result = sqlx::query("DELETE FROM oxsum.groups WHERE org_id = $1 AND name = $2")
            .bind(organization_id)
            .bind(name)
            .execute(self.pool())
            .await;
        match result {
            Ok(deleted) => Ok(deleted.rows_affected() > 0),
            Err(sqlx::Error::Database(error))
                if error.constraint() == Some("api_keys_org_group_fk") =>
            {
                Err(WalletError::Conflict(
                    "group is assigned to keys; reassign or ungroup them first".into(),
                ))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// The channels a key's group lets it reach. `None` means the key is
    /// ungrouped — it may reach every channel (the backward-compatible
    /// default). `Some(set)` means the key is grouped and may only reach that
    /// set.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn key_channel_scope(
        &self,
        key_id: Uuid,
    ) -> Result<Option<HashSet<Uuid>>, WalletError> {
        // One statement observes assignment and membership in the same MVCC
        // snapshot, including an empty group (Some(empty), never unrestricted).
        let row = sqlx::query(
            "SELECT k.group_id, ARRAY(SELECT gc.channel_id FROM oxsum.group_channels gc \
             WHERE gc.group_id = k.group_id ORDER BY gc.channel_id) AS channels \
             FROM oxsum.api_keys k WHERE k.key_id = $1",
        )
        .bind(key_id)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) if row.try_get::<Option<Uuid>, _>("group_id")?.is_some() => Ok(Some(
                row.try_get::<Vec<Uuid>, _>("channels")?
                    .into_iter()
                    .collect(),
            )),
            Some(_) => Ok(None),
            None => Err(WalletError::Unauthenticated),
        }
    }

    /// Assigns a group atomically for trusted domain callers. HTTP callers use
    /// the authorized key-constraint writer instead.
    pub async fn assign_key_group(
        &self,
        organization_id: Uuid,
        key_id: Uuid,
        group_id: Option<Uuid>,
    ) -> Result<bool, WalletError> {
        let mut tx = self.pool().begin().await?;
        if let Some(id) = group_id {
            let found: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM oxsum.groups WHERE org_id = $1 AND id = $2 FOR KEY SHARE",
            )
            .bind(organization_id)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            if found.is_none() {
                return Err(WalletError::InvalidInput(
                    "groupId is not a group of this organization".into(),
                ));
            }
        }
        let changed = sqlx::query(
            "UPDATE oxsum.api_keys SET group_id = $3 WHERE organization_id = $1 AND key_id = $2",
        )
        .bind(organization_id)
        .bind(key_id)
        .bind(group_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(changed > 0)
    }
}
