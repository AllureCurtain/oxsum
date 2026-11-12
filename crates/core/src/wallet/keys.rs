//! Per-key committed reads and the entitlement projection.

use crate::entitlement::{EntitlementMandate, EntitlementRead};
use crate::error::WalletError;
use crate::keys::BudgetDuration;

use super::Wallet;
use sqlx::Row;
use std::collections::{BTreeMap, HashMap};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

impl Wallet {
    /// What one API key has committed: settled charges plus outstanding holds attributed
    /// to it, in minor units. Read from the ledger's own postings — the pending release
    /// and the settled charge of a settlement both carry the hold's actor — so it cannot
    /// drift from the books.
    pub async fn key_committed(&self, key_id: &Uuid) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.key_committed_in(&mut conn, key_id, None).await
    }
    /// The [`key_committed`](Self::key_committed) read for a window — settled
    /// charges plus outstanding holds, the settled half scoped to entries
    /// booked on or after `since` (issue #232: the consumption a
    /// `budgetDuration` budget is measured against).
    pub async fn key_committed_window(
        &self,
        key_id: &Uuid,
        since: Option<Date>,
    ) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.key_committed_in(&mut conn, key_id, since).await
    }
    /// The [`key_committed_window`](Self::key_committed_window) read for many
    /// keys in one round-trip: each `(key_id, since)` pair gets its own window,
    /// and a key with no postings answers 0 — the same figure the per-key read
    /// returns. The budget endpoint lists the organization's keys and asks for
    /// all of them at once rather than a query per key.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn key_committed_windows(
        &self,
        keys: &[(Uuid, Option<Date>)],
    ) -> Result<HashMap<Uuid, i64>, WalletError> {
        let mut committed: HashMap<Uuid, i64> = keys.iter().map(|(id, _)| (*id, 0)).collect();
        if keys.is_empty() {
            return Ok(committed);
        }
        // One grouped read over the actor set: `unnest` pairs each key's actor
        // string with its own window start so the per-key `since` rule matches
        // `key_committed_in` — pending postings always count, settled ones only
        // on or after that key's window start.
        let sql = format!(
            "SELECT actors.actor, COALESCE(SUM(CASE WHEN p.direction = 'D' \
                              THEN p.amount_minor ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM unnest($2::text[], $3::date[]) AS actors(actor, since) \
             LEFT JOIN ledger_{schema}.entries e \
               ON e.provenance_actor = actors.actor \
             LEFT JOIN ledger_{schema}.postings p \
               ON p.entry_id = e.entry_id AND p.account_index = ANY($1) \
              AND (p.layer = 'pending' OR actors.since IS NULL OR e.booking_date >= actors.since) \
             GROUP BY actors.actor",
            schema = self.tenant_id,
        );
        let ids: Vec<String> = keys
            .iter()
            .map(|(id, _)| id.as_simple().to_string())
            .collect();
        let sinces: Vec<Option<Date>> = keys.iter().map(|(_, since)| *since).collect();
        let rows = sqlx::query(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(ids)
            .bind(sinces)
            .fetch_all(self.store.pool())
            .await?;
        for row in &rows {
            let actor: String = row.try_get("actor")?;
            let value: i64 = row.try_get("committed")?;
            if let Ok(id) = Uuid::parse_str(&actor) {
                committed.insert(id, value);
            }
        }
        Ok(committed)
    }
    /// The `member_committed_in` read on its own
    /// connection: the member's committed spend across every key they minted
    /// (issue #232).
    pub async fn member_committed(
        &self,
        organization_id: &Uuid,
        member: &Uuid,
    ) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.member_committed_in(&mut conn, organization_id, member)
            .await
    }
    /// The [`key_committed`](Self::key_committed) read, on the caller's connection: the
    /// limit check runs it inside the transaction that holds the per-key advisory lock.
    ///
    /// `since` scopes the settled half to entries booked on or after that date — the
    /// period start a `budget_duration` window gives. Pending postings always count in
    /// full: an outstanding hold reserves money now whatever day it was taken.
    pub(super) async fn key_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        key_id: &Uuid,
        since: Option<Date>,
    ) -> Result<i64, WalletError> {
        // The schema name is assembled from the validated tenant id, like `Wallet::open`
        // does; the actor is the key id in uuid simple form, bound as a parameter.
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_actor = $2 \
               AND (p.layer = 'pending' OR $3::date IS NULL OR e.booking_date >= $3)",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(key_id.as_simple().to_string())
            .bind(since)
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }
    /// What one member's keys have committed in total: the sum of
    /// [`key_committed_in`](Self::key_committed_in) over every key they minted —
    /// the member budget's denominator. The ledger attributes each posting to
    /// the key's actor id, and `api_keys.created_by` maps keys to the member, so
    /// the sum is the ledger's own figure for the member's spend (issue #162).
    ///
    /// Run inside the transaction holding the member's locked membership row, so
    /// the sum and the hold append that follows it agree on what is committed.
    /// Revoked keys still count: the money they spent stayed spent.
    pub(super) async fn member_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        organization_id: &Uuid,
        member: &Uuid,
    ) -> Result<i64, WalletError> {
        // The actor is the key id in uuid simple form, like `key_committed_in`.
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT replace(key_id::text, '-', '') FROM oxsum.api_keys \
             WHERE organization_id = $1 AND created_by = $2",
        )
        .bind(organization_id)
        .bind(member)
        .fetch_all(&mut *conn)
        .await?;
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_actor = ANY($2)",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(keys)
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }
    /// The `team_committed_in` read on its own
    /// connection: the team's committed spend across every key its members
    /// minted (issue #440).
    pub async fn team_committed(
        &self,
        organization_id: &Uuid,
        team_id: &Uuid,
    ) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.team_committed_in(&mut conn, organization_id, team_id)
            .await
    }
    /// What one team's members have committed in total: the sum of
    /// [`key_committed_in`](Self::key_committed_in) over every key minted by
    /// the team's members — the team budget's denominator (issue #440). The
    /// ledger attributes each posting to the key's actor id, and
    /// `memberships.team_id` + `api_keys.created_by` map keys to the team, so
    /// the sum is the ledger's own figure for the subgroup's spend — the same
    /// attribution `member_committed_in` gives
    /// per member, aggregated.
    ///
    /// Run inside the transaction holding the team's locked row, so the sum
    /// and the hold append that follows it agree on what is committed.
    /// Committed spend follows the roster, not the team's history: a member
    /// moved between teams carries their keys' spend into the new team's sum,
    /// and keys minted by a since-removed member still count.
    pub(super) async fn team_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        organization_id: &Uuid,
        team_id: &Uuid,
    ) -> Result<i64, WalletError> {
        // The actor is the key id in uuid simple form, like
        // `key_committed_in`; the roster is the members the team groups.
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT replace(k.key_id::text, '-', '') FROM oxsum.api_keys k \
             JOIN oxsum.memberships m \
               ON m.organization_id = k.organization_id AND m.user_id = k.created_by \
             WHERE k.organization_id = $1 AND m.team_id = $2",
        )
        .bind(organization_id)
        .bind(team_id)
        .fetch_all(&mut *conn)
        .await?;
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_actor = ANY($2)",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(keys)
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }
    /// How many holds the key has outstanding, counted from the ledger's own
    /// entries: an entry that debits a pool account in the pending layer is a
    /// live reservation, and an entry that credits one is the release leg of a
    /// settlement — the release rides the settlement entry, so a hold stays open
    /// until another entry frees it. The difference is the open count.
    ///
    /// Run inside the transaction holding the per-key advisory lock, so the count
    /// and the hold append that follows it agree on what is committed.
    pub(super) async fn key_open_holds_in(
        &self,
        conn: &mut sqlx::PgConnection,
        key_id: &Uuid,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT count(*) FILTER (WHERE pending_debit > 0) \
                  - count(*) FILTER (WHERE pending_credit > 0) \
             FROM ( \
                 SELECT e.entry_id, \
                        SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor ELSE 0 END) \
                            AS pending_debit, \
                        SUM(CASE WHEN p.direction = 'C' THEN p.amount_minor ELSE 0 END) \
                            AS pending_credit \
                 FROM ledger_{schema}.postings p \
                 JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = ANY($1) AND e.provenance_actor = $2 \
                   AND p.layer = 'pending' \
                 GROUP BY e.entry_id \
             ) per_entry",
            schema = self.tenant_id,
        );
        let open: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.promo.index() as i32,
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(key_id.as_simple().to_string())
            .fetch_one(&mut *conn)
            .await?;
        Ok(open)
    }
    /// What one mandate has committed: the settled charges under its
    /// What one mandate has committed in total: the sum of settled charges and
    /// outstanding holds its entries carry, read from the ledger by the mandate
    /// id each writes into `provenance_correlation` — the same figure
    /// [`key_committed_in`](Self::key_committed_in) answers per key, sliced by
    /// the delegation instead of the key (issue #262).
    ///
    /// Run inside the transaction holding the per-key advisory lock, so the sum
    /// and the hold append that follows it agree on what is committed. A hold's
    /// pending debits count toward the cap; the settlement's release leg credits
    /// the same accounts back, so a settled charge nets to what it billed.
    pub(super) async fn mandate_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        mandate_id: &Uuid,
    ) -> Result<i64, WalletError> {
        // The schema name is assembled from the validated tenant id, like
        // `Wallet::open` does; the mandate id is the entry's provenance
        // correlation in uuid simple form, bound as a parameter.
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_correlation = $2",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(mandate_id.as_simple().to_string())
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }
    /// The ledger side of the entitlement read (issue #501): every check the
    /// ledger itself can answer for a request — suspension, the allowlist, the
    /// spend caps layered key → member → team, the open-holds count, and a
    /// presented mandate's window, model subset and committed cap — gathered
    /// on one connection under no lock.
    ///
    /// Every constraint is read even when an earlier one already refuses: the
    /// `checks` breakdown the endpoint exists for needs the whole picture, and
    /// a read that short-circuits could not say what else was wrong. The
    /// request window (the server's in-process limiter) and the settled-token
    /// window (`Db::tpm_window*` on the `oxsum` tables) are not in here — the
    /// limits they are judged against ride the returned [`EntitlementRead`] so
    /// the route can run those two and compose the answer itself.
    ///
    /// `model` is the gateway model the request would call (`None` skips the
    /// allowlist question, like `POST /api/v1/holds`); `estimate` is a
    /// hypothetical charge in minor units — the freeze a real request would
    /// take — judged against the spend caps' headroom (`None` asks whether the
    /// caps have room at all, against one minor unit — the smallest hold the
    /// ledger takes). `mandate` is the already-verified delegation the caller
    /// presented — the route ran [`crate::mandates::parse_presented`]; what is
    /// judged here are its verified fields, and a mandate minted for another
    /// key refuses `mandate_invalid` rather than leak which key it belongs to.
    /// `tags` are the request's `metadata` tag values: the key's
    /// `tokensPerMinuteScopes` buckets resolve to their scope names only where
    /// the request would actually land in them, so a scope the turn does not
    /// tag into is no more a constraint than it is at admission.
    ///
    /// The figures are a snapshot a racing request may already have spent —
    /// the surface is "can I", not "reserve me", and the atomic admission
    /// checks stay authoritative. `scope` is the caller's [`crate::KeyScope`],
    /// applied like the key list's: a member asks only about the keys they
    /// created — any other id answers `None`, indistinguishable from a foreign
    /// or nonexistent one.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    #[allow(clippy::too_many_arguments)]
    pub async fn entitlement_read(
        &self,
        organization_id: &Uuid,
        key_id: &Uuid,
        scope: crate::KeyScope,
        model: Option<&str>,
        estimate: Option<i64>,
        tags: &BTreeMap<String, String>,
        mandate: Option<&EntitlementMandate>,
    ) -> Result<Option<EntitlementRead>, WalletError> {
        use crate::entitlement::{EntitlementRead, MandateVerdict};
        let mut conn = self.store.pool().acquire().await?;
        // The scope clause is the key list's: `Own` narrows to keys the member
        // created; a scoped-out id reads as missing — 404 upstream, never 403.
        let row = {
            let created_by = match scope {
                crate::KeyScope::Own(user_id) => Some(user_id),
                crate::KeyScope::Key(id) => {
                    if id != *key_id {
                        return Ok(None);
                    }
                    None
                }
                crate::KeyScope::Organization | crate::KeyScope::All => None,
            };
            sqlx::query(
                "SELECT k.key_id, k.spend_limit_minor, k.budget_duration, \
                        k.model_allowlist, k.max_concurrent_holds, \
                        k.requests_per_minute, k.tokens_per_minute, \
                        k.tokens_per_minute_scopes, \
                        k.revoked_at IS NOT NULL OR \
                            (k.expires_at IS NOT NULL AND k.expires_at <= now()) AS inactive, \
                        (o.suspended_at IS NOT NULL OR o.dunning_suspended_at IS NOT NULL OR EXISTS (SELECT 1 FROM oxsum.customer_profiles c WHERE c.organization_id=o.organization_id AND c.state='archived')) AS suspended, \
                        m.user_id AS budget_member, m.team_id AS budget_team \
                 FROM oxsum.api_keys k \
                 JOIN oxsum.organizations o ON o.organization_id = k.organization_id \
                 LEFT JOIN oxsum.memberships m \
                        ON m.organization_id = k.organization_id \
                       AND m.user_id = k.created_by \
                 WHERE k.key_id = $1 AND k.organization_id = $2 \
                   AND ($3::uuid IS NULL OR k.created_by = $3)",
            )
            .bind(key_id)
            .bind(organization_id)
            .bind(created_by)
            .fetch_optional(&mut *conn)
            .await?
        };
        let Some(row) = row else {
            return Ok(None);
        };

        let key_active = !row.try_get::<bool, _>("inactive")?;
        let org_suspended = row.try_get::<bool, _>("suspended")?;
        // The allowlist question only runs when a model was asked: `None` skips
        // it the way `POST /api/v1/holds` does, and `model_allowed` stays
        // `None` to say so.
        let allowlist: Option<Vec<String>> = row.try_get("model_allowlist")?;
        let model_allowed = model.map(|model| {
            allowlist
                .as_ref()
                .is_none_or(|list| list.iter().any(|allowed| allowed == model))
        });
        // The spend limit's headroom — committed reads the same figure
        // `hold_for_key` judges, budget-windowed when the key carries one.
        // `estimate` is what the request would reserve; absent, the check asks
        // whether the caps have room at all — one minor unit, the smallest
        // hold the ledger takes, is the bar.
        let bar = estimate.unwrap_or(1);
        let limit: Option<i64> = row.try_get("spend_limit_minor")?;
        let on = OffsetDateTime::now_utc().date();
        let mut remaining_budget: Option<i64> = None;
        let mut budget_ok: Option<bool> = None;
        if let Some(limit) = limit {
            let since = row
                .try_get::<Option<String>, _>("budget_duration")?
                .as_deref()
                .map(BudgetDuration::parse)
                .transpose()?
                .map(|d| d.period_start(on));
            let committed = self.key_committed_in(&mut conn, key_id, since).await?;
            remaining_budget = Some(limit - committed);
            budget_ok = Some(committed + bar <= limit);
        }
        // The member and team caps layer onto the key's own, exactly like
        // `hold_for_key` reads them — folded into the same `budget_ok` verdict
        // and the same `remaining_budget_minor` floor, so the answer names the
        // tightest cap in force rather than the key's alone.
        let member: Option<Uuid> = row.try_get("budget_member")?;
        if let Some(member) = member {
            let member_cap: Option<i64> = sqlx::query_scalar(
                "SELECT budget_limit_minor FROM oxsum.memberships \
                 WHERE organization_id = $1 AND user_id = $2",
            )
            .bind(organization_id)
            .bind(member)
            .fetch_one(&mut *conn)
            .await?;
            if let Some(cap) = member_cap {
                let committed_member = self
                    .member_committed_in(&mut conn, organization_id, &member)
                    .await?;
                if committed_member + bar > cap {
                    budget_ok = Some(false);
                }
                // The tighter of the caps is the headroom worth reporting: the
                // member cap bounds the sum across every key they minted, so
                // `cap - committed_member` is already that scope's own
                // remaining figure.
                remaining_budget = Some(
                    remaining_budget
                        .map_or(cap - committed_member, |r| r.min(cap - committed_member)),
                );
            }
            let team: Option<Uuid> = row.try_get("budget_team")?;
            if let Some(team) = team {
                let team_cap: Option<i64> = sqlx::query_scalar(
                    "SELECT budget_limit_minor FROM oxsum.teams \
                     WHERE organization_id = $1 AND team_id = $2",
                )
                .bind(organization_id)
                .bind(team)
                .fetch_one(&mut *conn)
                .await?;
                if let Some(cap) = team_cap {
                    let committed_team = self
                        .team_committed_in(&mut conn, organization_id, &team)
                        .await?;
                    if committed_team + bar > cap {
                        budget_ok = Some(false);
                    }
                    remaining_budget = Some(
                        remaining_budget
                            .map_or(cap - committed_team, |r| r.min(cap - committed_team)),
                    );
                }
            }
        }
        // The outstanding-holds cap reads the same ledger count the hold path
        // enforces — `concurrentHolds` reports the figure either way, so a
        // surface can draw the occupancy without a second query.
        let open = self.key_open_holds_in(&mut conn, key_id).await?
            + self.zero_wallet_reservations_in(&mut conn, *key_id).await?;
        // The tag-scoped token buckets, resolved the way admission resolves
        // them: the scope's `scope_name` against the request's tags names the
        // bucket the turn would accrue into — a scope the request does not tag
        // into is skipped, exactly like `hold_for_key`'s callers do.
        let scopes_json: Option<serde_json::Value> = row.try_get("tokens_per_minute_scopes")?;
        let mut tpm_scopes: Vec<(String, i64)> = Vec::new();
        if let Some(scopes) = crate::keys::scopes_from_json(scopes_json) {
            for (tag, scope) in scopes.iter() {
                let tag_scope = crate::TagScope {
                    tag: tag.clone(),
                    scope: scope.clone(),
                };
                if let Some(name) = tag_scope.scope_name(tags) {
                    tpm_scopes.push((name, scope.cap()));
                }
            }
        }
        // The mandate's own bounds, on top of the key's — the same three the
        // hold path enforces inside its lock, judged against the same
        // ledger-derived committed figure.
        let mandate_verdict = match mandate {
            None => None,
            Some(mandate) if mandate.key_id != *key_id => Some(MandateVerdict::Invalid),
            Some(mandate) => {
                let now = OffsetDateTime::now_utc().unix_timestamp();
                if now < mandate.not_before_ts {
                    Some(MandateVerdict::NotYetOrExpired(
                        (mandate.not_before_ts - now) as u64,
                    ))
                } else if now > mandate.not_after_ts {
                    Some(MandateVerdict::NotYetOrExpired(0))
                } else if let (Some(model), Some(models)) = (model, &mandate.models)
                    && !models.iter().any(|allowed| allowed == model)
                {
                    Some(MandateVerdict::ModelRefused)
                } else {
                    let committed_mandate = self
                        .mandate_committed_in(&mut conn, &mandate.mandate_id)
                        .await?;
                    if committed_mandate + bar > mandate.cap_minor {
                        Some(MandateVerdict::CapExceeded(
                            mandate.cap_minor - committed_mandate,
                        ))
                    } else {
                        Some(MandateVerdict::Ok)
                    }
                }
            }
        };
        Ok(Some(EntitlementRead {
            key_active,
            org_suspended,
            model_allowed,
            remaining_budget_minor: remaining_budget,
            budget_ok,
            open_holds: open,
            max_concurrent_holds: row.try_get("max_concurrent_holds")?,
            requests_per_minute: row.try_get("requests_per_minute")?,
            tokens_per_minute: row.try_get("tokens_per_minute")?,
            tpm_scopes,
            mandate: mandate_verdict,
        }))
    }
}
