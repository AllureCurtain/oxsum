//! A channel's credential pool: several upstream keys behind one channel, and
//! the pick that chooses among them (issue #444).
//!
//! A provider's rate limits and quota are per-account, not per-endpoint. With a
//! single key per channel, one account's 429 could only fail over onto a
//! *different* channel — another provider, another price. A pool lets the same
//! channel carry several keys, so a single account's refusal rotates to the
//! next credential of the same channel — same model, same price, same protocol —
//! before the turn has to leave the channel at all.
//!
//! Two picks happen on every request, at different layers:
//!
//! - `route_order` (channels.rs) picks the *channel* order — weighted, then
//!   cost-aware inside a tier — over one [`Serving`] per channel. The pool is
//!   invisible to it: three credentials on one channel are still one route.
//! - [`Pool::order`] orders the channel's credentials for the attempts inside
//!   that route. A conversation's sticky hit leads when it is still healthy
//!   (it keeps the upstream's prompt cache warm); the rest follow
//!   least-recently-used, then `position`. Credentials cooling down or disabled
//!   are filtered out before any of that.
//!
//! Health is shared mutable state on the credential row — `cooldown_until` is
//! the pick's memory of a credential that kept refusing, `last_used_at` the
//! `usage` ranking's input. Neither is ledger data: the settlement names
//! `(channel, version)`, so a turn billed under one channel settles identically
//! whichever of its keys answered.
//!
//! Session affinity is process-local and deliberately small: a `Db` holds an
//! in-memory map of `conversation → credential` with a bounded TTL. It is a
//! cache-warmer hint, not shared state — a restart or a second process simply
//! picks fresh, which is correct and cheap, because the only cost of a missed
//! sticky is a cold upstream prompt cache.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// How long a conversation's last answerer is remembered: the pick sticks a
/// conversation to the credential that answered it while the upstream's prompt
/// cache could still be warm. Provider caches live minutes, not hours — a
/// longer memory mostly re-picks credentials that lost their cache anyway.
const STICK_TTL: Duration = Duration::from_secs(10 * 60);

/// The most stick entries the map holds: affinity is a hint, and an unbounded
/// map is how a hint becomes a leak. Past the cap the oldest expire first.
const STICK_CAP: usize = 8 * 1024;

/// How long a credential cools after a refusal the pool can blame on it: a 429
/// or 5xx names the credential's own wall, so the pick skips it for this window
/// and lets it back without an operator touching it. Bounded — a credential
/// that stays bad keeps getting re-cooled by its own failures, not banned.
pub const CREDENTIAL_COOLDOWN: Duration = Duration::from_secs(60);

/// One credential in a channel's pool, as the pick orders it: the opened key
/// for the upstream `Authorization` header, its id for the health writes a
/// refusal or a served attempt records, and the pick inputs.
#[derive(Debug, Clone)]
pub struct PoolCredential {
    /// The row's id: what `note_used`/`cool_down` write health against.
    pub credential_id: Uuid,
    /// The upstream credential, opened. Held for this request only.
    pub api_key: String,
    /// The operator's pick order inside the pool.
    pub position: i64,
    /// When this credential last led an upstream call — `None` is never used.
    pub last_used_at: Option<OffsetDateTime>,
}

/// A channel as the admin API presents its pool: one entry per credential,
/// never more than the last four of any key.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelCredential {
    pub credential_id: Uuid,
    /// The last four characters of the upstream key, and nothing more of it.
    pub api_key_last4: String,
    /// The pick order inside the pool.
    pub position: i64,
    /// `active` picks normally; `disabled` is kept on record, never picked.
    pub status: String,
    /// The health cool-down the pick honours until; absent means healthy.
    #[serde(with = "time::serde::rfc3339::option")]
    pub cooldown_until: Option<OffsetDateTime>,
    /// What cooled it, when one did — observability for the admin list.
    pub last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The pick over one channel's credentials: conversation stickiness first, then
/// least-recently-used, `position` as the stable tiebreak. Cooling-down and
/// disabled credentials never reach it — `Db::servings` filtered them out of
/// the pool the `Serving` carries.
///
/// `sticky` is the credential this conversation last answered on, when the
/// caller knows one (`Option` so a request with no conversation key picks
/// fresh). A sticky id that is no longer healthy — cooled since it answered,
/// or disabled — is simply absent from `credentials`, so it wins nothing.
#[must_use]
pub fn order(credentials: &[PoolCredential], sticky: Option<Uuid>) -> Vec<PoolCredential> {
    let mut ordered: Vec<PoolCredential> = credentials.to_vec();
    ordered.sort_by(|a, b| {
        let stuck =
            |credential: &&PoolCredential| sticky.is_some_and(|id| id == credential.credential_id);
        stuck(&b).cmp(&stuck(&a)).then_with(|| {
            // Least-recently-used: never-used first, then the oldest use.
            // `position` breaks the tie the operator set.
            a.last_used_at
                .cmp(&b.last_used_at)
                .then_with(|| a.position.cmp(&b.position))
                .then_with(|| a.credential_id.cmp(&b.credential_id))
        })
    });
    ordered
}

/// The conversation→credential memory, kept on [`Db`] so every test and the one
/// process share it. `Instant` deadlines are process-local by nature, which is
/// exactly right for a cache warmer: nothing here may outlive the process.
#[derive(Debug, Default)]
pub struct Affinity {
    inner: Mutex<AffinityMap>,
}

#[derive(Debug, Default)]
struct AffinityMap {
    /// `(channel_id, conversation) → (credential_id, expires)` — who answered
    /// this conversation last, and when the hint stops being worth taking.
    sticks: HashMap<(Uuid, String), (Uuid, Instant)>,
}

impl Affinity {
    /// The credential this conversation last answered on, if the hint is still
    /// warm. `None` for a conversation nobody answered yet, one the TTL lapsed,
    /// or a call with no conversation key — the pick simply starts fresh.
    pub fn sticky(&self, channel_id: Uuid, conversation: Option<&str>) -> Option<Uuid> {
        let conversation = conversation?;
        let mut map = self.inner.lock().ok()?;
        let now = Instant::now();
        match map.sticks.get(&(channel_id, conversation.to_owned())) {
            Some(&(credential, expires)) if expires > now => Some(credential),
            Some(_) => {
                map.sticks.remove(&(channel_id, conversation.to_owned()));
                None
            }
            None => None,
        }
    }

    /// Records who answered a conversation: the next pick of the same
    /// conversation leads with it while the TTL holds. A call with no
    /// conversation key records nothing — there is nothing to stick to.
    pub fn note(&self, channel_id: Uuid, conversation: Option<&str>, credential: Uuid) {
        let Some(conversation) = conversation else {
            return;
        };
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        if map.sticks.len() >= STICK_CAP {
            // Drop the expired entries first; if the cap still binds, drop the
            // soonest-to-expire — a hint the pool is about to lose anyway.
            let now = Instant::now();
            map.sticks.retain(|_, (_, expires)| *expires > now);
            while map.sticks.len() >= STICK_CAP {
                if let Some(oldest) = map
                    .sticks
                    .iter()
                    .min_by_key(|(_, (_, expires))| *expires)
                    .map(|(key, _)| key.clone())
                {
                    map.sticks.remove(&oldest);
                } else {
                    break;
                }
            }
        }
        map.sticks.insert(
            (channel_id, conversation.to_owned()),
            (credential, Instant::now() + STICK_TTL),
        );
    }
}

impl Db {
    /// Every credential of one channel, ordered by `position`: the pool as the
    /// admin API lists it — sealed keys, last four only, health state included
    /// so the list shows *why* a credential sits out.
    ///
    /// `None`: no such channel.
    pub async fn channel_credentials(
        &self,
        channel: &str,
    ) -> Result<Option<Vec<ChannelCredential>>, WalletError> {
        let rows = sqlx::query(
            "SELECT cr.credential_id, cr.api_key_last4, cr.position, cr.status, \
                    cr.cooldown_until, cr.last_error, cr.last_used_at, cr.created_at \
             FROM oxsum.channel_credentials cr \
             JOIN oxsum.channels c USING (channel_id) \
             WHERE c.name = $1 \
             ORDER BY cr.position, cr.created_at",
        )
        .bind(channel)
        .fetch_all(self.pool())
        .await?;
        let channel_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oxsum.channels WHERE name = $1)")
                .bind(channel)
                .fetch_one(self.pool())
                .await?;
        if !channel_exists {
            return Ok(None);
        }
        rows.iter()
            .map(|row| {
                Ok(ChannelCredential {
                    credential_id: row.try_get("credential_id")?,
                    api_key_last4: row.try_get("api_key_last4")?,
                    position: i64::from(row.try_get::<i32, _>("position")?),
                    status: row.try_get("status")?,
                    cooldown_until: row.try_get("cooldown_until")?,
                    last_error: row.try_get("last_error")?,
                    last_used_at: row.try_get("last_used_at")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, WalletError>>()
            .map(Some)
    }

    /// Adds a credential to a channel's pool at the end of the order.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for an unknown channel or an empty key;
    /// storage failures surface as [`WalletError`].
    pub async fn add_credential(
        &self,
        channel: &str,
        api_key: &str,
        key: &crate::channels::SecretKey,
    ) -> Result<ChannelCredential, WalletError> {
        let api_key = crate::channels::credential_text(api_key)?;
        let sealed = key.seal(api_key)?;
        let last4 = crate::channels::last_chars(api_key, crate::channels::LAST4);
        let row = sqlx::query(
            "INSERT INTO oxsum.channel_credentials \
                 (credential_id, channel_id, api_key_sealed, api_key_last4, position) \
             SELECT $1, c.channel_id, $2, $3, \
                    COALESCE(MAX(cr.position), 0) + 1 \
             FROM oxsum.channels c \
             LEFT JOIN oxsum.channel_credentials cr ON cr.channel_id = c.channel_id \
             WHERE c.name = $4 \
             GROUP BY c.channel_id \
             RETURNING credential_id, position, created_at",
        )
        .bind(Uuid::new_v4())
        .bind(sealed)
        .bind(last4)
        .bind(channel)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Err(WalletError::InvalidInput(format!(
                "no channel named {channel:?}"
            )));
        };
        Ok(ChannelCredential {
            credential_id: row.try_get("credential_id")?,
            api_key_last4: crate::channels::last_chars(api_key, crate::channels::LAST4),
            position: i64::from(row.try_get::<i32, _>("position")?),
            status: "active".to_owned(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: row.try_get("created_at")?,
        })
    }

    /// Switches one credential's `status`: `active` picks it again, `disabled`
    /// keeps it on record and never picks it. `false` when the credential is
    /// not this channel's — unknown to it, or another channel's row.
    ///
    /// # Errors
    ///
    /// Refuses a status outside `active`/`disabled`; storage failures surface.
    pub async fn set_credential_status(
        &self,
        channel: &str,
        credential_id: Uuid,
        status: &str,
    ) -> Result<bool, WalletError> {
        if !matches!(status, "active" | "disabled") {
            return Err(WalletError::InvalidInput(format!(
                "status must be active or disabled, got {status:?}"
            )));
        }
        let touched = sqlx::query(
            "UPDATE oxsum.channel_credentials cr SET status = $3 \
             FROM oxsum.channels c \
             WHERE cr.credential_id = $1 AND cr.channel_id = c.channel_id AND c.name = $2",
        )
        .bind(credential_id)
        .bind(channel)
        .bind(status)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(touched > 0)
    }

    /// Drops one credential from a channel's pool; `false` when it is not this
    /// channel's. The pool's last credential refuses to drop: a channel with no
    /// key cannot serve, and a `channels` row that promises routes it cannot
    /// honour is a lie the schema does not tell — take the channel down with it
    /// or disable the credential instead.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when the credential is the pool's last one.
    pub async fn drop_credential(
        &self,
        channel: &str,
        credential_id: Uuid,
    ) -> Result<bool, WalletError> {
        let mut tx = self.pool().begin().await?;
        // What the drop would leave pickable: rows still `active` besides the
        // one going — a cooling credential counts, because its cool-down ends
        // on its own. `disabled` does not: it stays out until an operator puts
        // it back, and counting it would let a pool drain to disabled-only.
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.channel_credentials cr \
             JOIN oxsum.channels c USING (channel_id) \
             WHERE c.name = $1 AND cr.credential_id <> $2 AND cr.status = 'active'",
        )
        .bind(channel)
        .bind(credential_id)
        .fetch_one(&mut *tx)
        .await?;
        let owns: bool = sqlx::query_scalar(
            "SELECT EXISTS ( \
                 SELECT 1 FROM oxsum.channel_credentials cr \
                 JOIN oxsum.channels c USING (channel_id) \
                 WHERE c.name = $1 AND cr.credential_id = $2)",
        )
        .bind(channel)
        .bind(credential_id)
        .fetch_one(&mut *tx)
        .await?;
        if !owns {
            tx.rollback().await?;
            return Ok(false);
        }
        if remaining == 0 {
            tx.rollback().await?;
            return Err(WalletError::InvalidInput(
                "a channel's last credential cannot be dropped; disable it, \
                 or retire the channel"
                    .into(),
            ));
        }
        sqlx::query("DELETE FROM oxsum.channel_credentials WHERE credential_id = $1")
            .bind(credential_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Marks the credential that led a served upstream call: the `usage` pick's
    /// input — least-recently-used rotates the pool across its keys, and a
    /// credential that answers is the one a conversation should stick to.
    /// Best-effort by contract: the pick survives a missed write.
    pub async fn note_credential_used(&self, credential_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.channel_credentials \
             SET last_used_at = now() \
             WHERE credential_id = $1",
        )
        .bind(credential_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Cools a credential that refused upstream before answering: a 429 or 5xx
    /// names its own wall, so the pool skips it for [`CREDENTIAL_COOLDOWN`] and
    /// `last_error` records what the admin list should say. An unreachable
    /// upstream is the channel's problem, not the credential's — the caller
    /// only cools statuses the credential itself produced. Best-effort by
    /// contract: the write may race, and the worst case is one more attempt on
    /// a credential that deserved a rest.
    pub async fn cool_credential(&self, credential_id: Uuid, why: &str) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.channel_credentials \
             SET cooldown_until = now() + make_interval(secs => $2), \
                 last_error = $3 \
             WHERE credential_id = $1",
        )
        .bind(credential_id)
        .bind(CREDENTIAL_COOLDOWN.as_secs_f64())
        .bind(why)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The conversation→credential memory this process keeps: shared across the
    /// pool reads so a `servings` call and the attempt that follows it see the
    /// same sticks.
    #[must_use]
    pub fn affinity(&self) -> &Affinity {
        &self.affinity
    }

    /// The pool's health filter: every credential of `channel_id` worth picking
    /// — `active`, and either never cooled or cooled past its window. Ordered
    /// by `position`; the [`order`] pick re-ranks inside that.
    ///
    /// Returned sealed: the caller opens each under [`crate::channels::SecretKey`]
    /// only for the request it is about to serve.
    pub(crate) async fn healthy_credentials(
        &self,
        channel_id: Uuid,
    ) -> Result<Vec<(Uuid, String, i64, Option<OffsetDateTime>)>, WalletError> {
        let rows = sqlx::query(
            "SELECT credential_id, api_key_sealed, position, last_used_at \
             FROM oxsum.channel_credentials \
             WHERE channel_id = $1 AND status = 'active' \
               AND (cooldown_until IS NULL OR cooldown_until <= now()) \
             ORDER BY position, created_at",
        )
        .bind(channel_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get::<Uuid, _>("credential_id")?,
                    row.try_get::<String, _>("api_key_sealed")?,
                    i64::from(row.try_get::<i32, _>("position")?),
                    row.try_get::<Option<OffsetDateTime>, _>("last_used_at")?,
                ))
            })
            .collect()
    }
}

/// The shared handle: `Db` holds one `Arc` so its cheap clones share the map.
pub type SharedAffinity = Arc<Affinity>;
