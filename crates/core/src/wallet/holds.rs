//! Holds: the raw reservation the wallet and the service surface take.

use super::{
    Credits, ORG_ADMISSION_LOCK, POOL_SPLIT_ATTEMPTS, Receipt, SCALE, Wallet, currency,
    description_of, entry_id_for, idem, is_deadlock, limit_lock_key, positive, stored_receipt,
};
use crate::error::{WalletError, invalid};
use crate::keys::ActingKey;
use crate::mandates::PresentedMandate;
use doubleentry::{
    Description, Direction, Draft, Entry, Layer, LedgerStore, Posting, Provenance, StoredEntry,
};
use std::sync::Arc;
use std::time::Duration;
use time::Date;
use uuid::Uuid;

/// How many of one key's holds may be inside the locked transaction at once —
/// the width of the in-process queue [`Wallet::key_queue`] admits by (issue
/// #486). One is the honest width: the per-key advisory lock serializes the
/// holds anyway, so a second admitted hold can only wait inside the transaction
/// — holding its pooled connection — for exactly the connection-pinning the
/// queue exists to prevent. The width lives in the semaphore's permit count,
/// never as a second copy.
const KEY_QUEUE_WIDTH: usize = 1;

/// How long a hold may wait in the in-process queue before refusing fast rather
/// than queueing longer than the pool's own acquire timeout would have let it.
/// The queued hold costs nothing — no connection — so the budget is the
/// caller's patience, not the pool's; it matches `ACQUIRE_TIMEOUT` so a refused
/// queue-wait surfaces on the same timescale a refused checkout did.
const KEY_QUEUE_WAIT: Duration = Duration::from_secs(5);

/// How many times a `hold` may lose the ledger's append lock to a racing writer
/// before refusing `PoolBusy`: each miss drops the transaction's connection and
/// sleeps `LOCK_WAIT_BACKOFF` off the pool, so the count times the patience
/// window, not the contention depth. Sized so a queued burst drains — a busy
/// ledger appends in milliseconds — rather than on the pool's acquire timeout.
const LOCK_WAIT_ATTEMPTS: usize = 250;

/// How long a hold that lost the append-lock claim sleeps before re-checking —
/// async time, so the wait costs no connection.
const LOCK_WAIT_BACKOFF: Duration = Duration::from_millis(20);

/// How one amount divides across the funded pools — promo first (it is the
/// money that can expire), then the bonus pool, the purchased wallet, and the
/// credit line last. Any side may be zero; the parts always sum to the amount
/// the split was computed for.
#[derive(Clone, Copy)]
pub(super) struct PoolSplit {
    pub(super) promo: i64,
    bonus: i64,
    wallet: i64,
    pub(super) credit: i64,
}

impl PoolSplit {
    fn total(&self) -> i64 {
        self.promo + self.bonus + self.wallet + self.credit
    }
}

impl Wallet {
    /// Hold: reserves part of the balance in the pending layer; the settled balance is untouched.
    ///
    /// The reservation splits across the pools — the bonus pool funds what it
    /// can, the wallet carries what it can, and the credit line picks up the
    /// rest — so the pending debit each pool takes stays inside what that pool's
    /// own balance covers, which is what `FundedReservations` enforces at
    /// append. A racing hold can take bonus room between the split's read and
    /// its write; the retry re-reads and splits again rather than refusing a
    /// request the combined balance could have served.
    /// Refused with [`WalletError::InsufficientFunds`] when the balance cannot
    /// cover it, and with [`WalletError::Forbidden`] when the organization is
    /// suspended (issue #162) — suspension gates every hold, attributed or not.
    ///
    /// `description` is what the entry says it is, in the caller's words — for a gateway request, the
    /// record built by [`crate::hold_description`]. An empty description records none; either way it
    /// is part of the entry and covered by its content hash.
    pub async fn hold(
        &self,
        key: &str,
        description: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        self.hold_inner(key, description, minor, None, on).await
    }
    /// A hold attributed to a service credential: the metering API's hold
    /// (issue #172). The credential is the entry's provenance actor —
    /// `svc-<credential id>` — so the hold and its settlement name the
    /// reporting service the way a key hold names the key. Everything else is
    /// [`hold`](Self::hold): the same pool split, the same suspension gate.
    ///
    /// # Errors
    ///
    /// As [`hold`](Self::hold).
    pub async fn hold_for_service(
        &self,
        service: &Uuid,
        key: &str,
        description: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let actor = Provenance::none()
            .with_actor(&format!("svc-{}", service.as_simple()))
            .map_err(invalid)?;
        self.hold_inner(key, description, minor, Some(actor), on)
            .await
    }
    /// The body [`hold`](Self::hold) and [`hold_for_service`](Self::hold_for_service)
    /// share: `actor` is the provenance the hold entry carries, `None` for an
    /// unattributed one.
    async fn hold_inner(
        &self,
        key: &str,
        description: &str,
        minor: i64,
        actor: Option<Provenance>,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        positive(minor)?;
        let description = description_of(description)?;
        let entry_id = entry_id_for(key);
        // A retry recomputes its pool split against balances that have moved
        // since the original hold landed, so the draft it builds need not be
        // the committed entry's shape: the committed one is the answer, like
        // `top_up`'s repaying shape. A stored entry that is not this hold —
        // another request under the same key — is the engine's key-reuse
        // conflict.
        if let Some(stored) = self.store.get(entry_id).await? {
            return self.hold_replay(&stored, &description, actor.as_ref(), minor, key);
        }
        // The suspension check is a short transaction of its own (issue #162): the
        // shared org-admission lock makes it wait for a suspension mid-commit, so a
        // check that runs observes the settled flag — a hold decided before the
        // suspension lands is an in-flight hold, which settles like every other.
        // Unlike `hold_for_key` the lock is not held across the append: an
        // unattributed hold has no per-key serialization to preserve anyway, and
        // holding a connection through it would starve the pool the append draws on.
        if let Ok(organization) = Uuid::parse_str(&self.tenant_id) {
            let mut tx = self.store.pool().begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended($1 || $2, 0))")
                .bind(ORG_ADMISSION_LOCK)
                .bind(organization.to_string())
                .execute(&mut *tx)
                .await?;
            let mut suspended: bool = sqlx::query_scalar(
                "SELECT (suspended_at IS NOT NULL OR dunning_suspended_at IS NOT NULL OR EXISTS (SELECT 1 FROM oxsum.customer_profiles c WHERE c.organization_id=oxsum.organizations.organization_id AND c.state='archived')) FROM oxsum.organizations \
                 WHERE organization_id = $1",
            )
            .bind(organization)
            .fetch_one(&mut *tx)
            .await?;
            if !suspended {
                // A past-due plan statement past its grace also suspends
                // admission (issue #612, T21): the binding's
                // `dunning_suspended_at` is the gate the overdue sweep stamps
                // and the payment reconcile lifts — read under the same
                // org-admission lock so a settled suspension is observed. The
                // table-presence check runs first so a pre-`sold_plans`
                // schema (a legacy baseline fixture) parses no dangling
                // reference: no `customer_plans` means no plan suspension.
                let has_plans: bool =
                    sqlx::query_scalar("SELECT to_regclass('oxsum.customer_plans') IS NOT NULL")
                        .fetch_one(&mut *tx)
                        .await?;
                if has_plans {
                    suspended =
                        crate::plan_debt::admission_suspended_in(&mut tx, organization).await?;
                }
            }
            drop(tx);
            if suspended {
                return Err(WalletError::Forbidden(
                    "the organization is suspended".into(),
                ));
            }
        }
        // The lock-wait itself is bounded off the pool: `pg_try_advisory_xact_lock`
        // answers immediately, so a hold that loses the race drops its
        // connection and sleeps on async time rather than pinning it through a
        // lock wait — enough queued holds on one tenant would otherwise exhaust
        // the shared pool behind the append lock (issue #486's doctrine, moved
        // one level down to the ledger lock the tx ends on). Waiters past the
        // patience window get the same `PoolBusy` a saturated checkout answers.
        let mut lock_waits = LOCK_WAIT_ATTEMPTS;
        let mut split_attempts = POOL_SPLIT_ATTEMPTS;
        loop {
            // The split, the pinned promo draws and the hold append share one
            // transaction: the `promo_draws` rows name the lots whose
            // `remaining_minor` this hold decremented, so settlement debits the
            // rows admission picked rather than re-scanning, and the pending
            // debit the entry posts commits beside them or not at all (issue
            // #609). Holding the transaction's connection through the append
            // costs nothing the split's borrowed read did not already — the
            // append is the same connection the draw-pinning rides.
            let mut tx = self.store.pool().begin().await?;
            // Claim the ledger's append lock up front, without waiting on the
            // connection: the engine takes it re-entrantly inside `append_in`
            // (`pg_advisory_xact_lock` counts per session), so a held lock
            // here means the append below never waits — and a lost claim means
            // this hold frees the connection it would have waited on. A
            // lock-wait miss is not a split attempt — the balance read has not
            // happened yet, so there is nothing a racing append invalidated.
            let claimed: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
                .bind(self.store.append_lock())
                .fetch_one(&mut *tx)
                .await?;
            if !claimed {
                tx.rollback().await?;
                lock_waits -= 1;
                if lock_waits == 0 {
                    return Err(WalletError::PoolBusy);
                }
                tokio::time::sleep(LOCK_WAIT_BACKOFF).await;
                continue;
            }
            let split = self.pool_split(&mut tx, minor).await?;
            if split.promo > 0 {
                self.pin_promo_draws_in(&mut tx, key, split.promo).await?;
            }
            let receipt = self
                .append_in(
                    &mut tx,
                    self.hold_entry(key, &description, split, actor.clone(), on)?,
                )
                .await;
            match receipt {
                Err(error @ WalletError::InsufficientFunds) => {
                    tx.rollback().await?;
                    split_attempts -= 1;
                    if split_attempts == 0 {
                        return Err(error);
                    }
                }
                // A concurrent `Wallet::open` on this same tenant can still be
                // walking `apply_schema`'s DDL when this hold writes: AccessExclusive
                // on the ledger tables against the append's RowExclusive is a
                // textbook cycle — a `40P01`, not corruption. The append is
                // deterministic against committed state, so the retry re-reads
                // and lands.
                Err(error) if is_deadlock(&error) => {
                    tx.rollback().await?;
                    lock_waits -= 1;
                    if lock_waits == 0 {
                        return Err(error);
                    }
                    tokio::time::sleep(LOCK_WAIT_BACKOFF).await;
                }
                // The key was taken between our read and the append — by a
                // racing copy of this same request or by a different one.
                Err(WalletError::Conflict(_)) => {
                    tx.rollback().await?;
                    return match self.store.get(entry_id).await? {
                        Some(stored) => {
                            self.hold_replay(&stored, &description, actor.as_ref(), minor, key)
                        }
                        None => Err(WalletError::Conflict(format!(
                            "idempotency key {key:?} is already taken"
                        ))),
                    };
                }
                Ok(receipt) => {
                    tx.commit().await?;
                    return Ok(receipt);
                }
                Err(error) => {
                    tx.rollback().await?;
                    return Err(error);
                }
            }
        }
    }
    /// Answers for a key that already has a committed entry: the stored
    /// entry's receipt when it is the hold being asked for — same amount,
    /// same description, same actor — whatever its pool split (balances move
    /// after a hold lands, so the committed shape is the answer, never the
    /// split a retry would compute now), and the key-reuse conflict when the
    /// entry is anything else.
    fn hold_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        actor: Option<&Provenance>,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let held: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| {
                self.is_pool(p.account)
                    && p.layer == Layer::Pending
                    && p.direction == Direction::Debit
            })
            .map(|p| p.amount.to_minor())
            .sum();
        let stored_actor = stored
            .entry
            .provenance()
            .actor
            .as_ref()
            .map(|label| label.as_str());
        let asked_actor = actor.and_then(|p| p.actor.as_ref().map(|label| label.as_str()));
        if held == minor
            && stored.entry.description().as_str() == description.as_str()
            && stored_actor == asked_actor
        {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }
    /// The split of `minor` across the pools, most-restricted money first: the
    /// promo pool funds what it can — it is the balance that can expire — then
    /// the bonus pool, the purchased wallet carries what it can, and the
    /// credit line picks up the rest. `split.total() == minor` always; any side
    /// may be zero, and `credit` above what the line still carries is what the
    /// append's `FundedReservations` check refuses.
    pub(super) async fn pool_split(
        &self,
        conn: &mut sqlx::PgConnection,
        minor: i64,
    ) -> Result<PoolSplit, WalletError> {
        let promo = minor.min(self.pool_available_in(conn, self.promo).await?.max(0));
        let bonus = (minor - promo).min(self.pool_available_in(conn, self.bonus).await?.max(0));
        let wallet =
            (minor - promo - bonus).min(self.pool_available_in(conn, self.wallet).await?.max(0));
        Ok(PoolSplit {
            promo,
            bonus,
            wallet,
            credit: minor - promo - bonus - wallet,
        })
    }
    /// Builds the pending-layer hold entry for a computed split: a pending debit
    /// on each pool it draws and the pending credit on revenue for the whole
    /// amount, with the optional provenance actor the hold attributes to.
    pub(super) fn hold_entry(
        &self,
        key: &str,
        description: &Description,
        split: PoolSplit,
        actor: Option<Provenance>,
        on: Date,
    ) -> Result<Entry<Draft, SCALE>, WalletError> {
        let minor = split.total();
        let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
            .with_description(description.clone());
        if let Some(actor) = actor {
            draft = draft.with_provenance(actor);
        }
        for (account, part) in [
            (self.promo, split.promo),
            (self.bonus, split.bonus),
            (self.wallet, split.wallet),
            (self.credit_line, split.credit),
        ] {
            if part > 0 {
                draft = draft.post(
                    Posting::debit(account, Credits::from_minor(part), currency())
                        .in_layer(Layer::Pending),
                );
            }
        }
        Ok(draft.post(
            Posting::credit(self.revenue, Credits::from_minor(minor), currency())
                .in_layer(Layer::Pending),
        ))
    }
    /// Hold attributed to an API key, enforcing the key's spend limit when it has one.
    ///
    /// The hold entry carries the key id as its provenance actor, so the key's committed
    /// spend — settled charges plus outstanding holds attributed to it — is readable from
    /// the ledger afterwards, even for keys that have no limit today.
    ///
    /// The limit check is serialized per key and atomic with the append: the transaction
    /// takes a per-key advisory lock, re-reads the limit from the key row (locked, so a
    /// concurrent limit change cannot slip between the read and the append), reads the
    /// key's committed spend from the ledger, and only then appends through the engine's
    /// normal path. The lock is held until commit — after the engine's append committed —
    /// so a racing second hold for the same key can only read usage that already includes
    /// the first hold. Two racing holds cannot both pass the check.
    ///
    /// Refused with [`WalletError::KeyLimitExceeded`] when the hold would push the key's
    /// committed spend past its limit, and with [`WalletError::InsufficientFunds`] when the
    /// balance cannot cover it — the key limit never overrides the balance.
    ///
    /// `model` is the gateway model the request wants, when the caller names one — the
    /// `/holds` endpoint passes `None` and is not governed by a model allowlist. A hold
    /// for a model the key may not use is [`WalletError::Forbidden`].
    ///
    /// The reservation splits across the pools like [`hold`](Self::hold)'s does; the
    /// split is computed on the transaction's connection so the lock, the limit read
    /// and the balance read agree on what is committed, and the whole flow retries
    /// when a racing append invalidates the split it chose.
    /// `mandate` is a verified `X-Oxsum-Mandate` the caller parsed and checked
    /// against the key's public half ([`crate::mandates::parse_presented`]).
    /// `hold_for_key` applies its bounds on top of the key's own — window, cap
    /// and model — under the same per-key lock, so a delegate's spend is
    /// checked against the ledger-derived committed figure atomically with the
    /// append, exactly like the key's own limit.
    #[allow(clippy::too_many_arguments)]
    pub async fn hold_for_key(
        &self,
        key: &ActingKey,
        model: Option<&str>,
        idem_key: &str,
        description: &str,
        minor: i64,
        on: Date,
        mandate: Option<&PresentedMandate>,
    ) -> Result<Receipt, WalletError> {
        positive(minor)?;
        let description = description_of(description)?;
        let mut actor = Provenance::none()
            .with_actor(&key.key_id.as_simple().to_string())
            .map_err(invalid)?;
        if let Some(mandate) = mandate {
            // The mandate id rides the hold entry's provenance correlation, so
            // the mandate's committed spend is readable from the ledger itself —
            // settled charges and outstanding holds alike — the same way the
            // key's own limit reads it, and a settle copies it onto the charge
            // the way `outstanding_actor` is copied (issue #262).
            actor = actor
                .with_correlation(&mandate.verified.mandate_id.as_simple().to_string())
                .map_err(invalid)?;
        }
        let entry_id = entry_id_for(idem_key);
        // A committed entry under the key is this request's answer before any
        // constraint is consulted: a replayed hold gets its own receipt even
        // when a budget or an allowlist has since moved — its split need not
        // match either, because the balances it was computed against moved too.
        if let Some(stored) = self.store.get(entry_id).await? {
            return self.hold_replay(&stored, &description, Some(&actor), minor, idem_key);
        }
        // Queue the key's own holds before a connection is checked out (issue
        // #486): the advisory lock below serializes them, and a hold that waits
        // for it inside a transaction pins a pooled connection for the whole
        // wait — enough queued holds on one key exhaust the shared pool and
        // every tenant's reads fail behind them. Waiting here costs no
        // connection: the semaphore admits only `KEY_QUEUE_WIDTH` into the
        // transaction, and the rest of the queue sits on async permits until a
        // slot frees or `KEY_QUEUE_WAIT` refuses the wait fast.
        let _queue = self.key_queue_wait(&key.key_id).await?;
        let mut last_err = None;
        for _ in 0..POOL_SPLIT_ATTEMPTS {
            let mut tx = self.store.pool().begin().await?;
            self.lock_key_admission_in(&mut tx, key.key_id).await?;
            self.lock_append_in(&mut tx).await?;
            self.authorize_key_in(&mut tx, key, model, minor, on, mandate)
                .await?;
            let split = self.pool_split(&mut tx, minor).await?;
            let draft = self.hold_entry(idem_key, &description, split, Some(actor.clone()), on)?;
            // The promo part of the split pins its lots here, inside the hold's
            // transaction, just before the entry appends into the same one:
            // `promo_draws` names which lots fund `split.promo`, so the
            // settlement debits the rows admission picked and a release returns
            // to exactly them — never a re-scan. The draw decrement and the
            // pending debit commit or roll back together (issue #609).
            if split.promo > 0 {
                // The pin writes `promo_lots`, so the append lock precedes it
                // — the global order is append advisory lock first, promo row
                // locks second (issue #723). No promo share means no promo
                // row, so the engine's append takes the lock on its own.
                self.lock_append_in(&mut tx).await?;
                self.pin_promo_draws_in(&mut tx, idem_key, split.promo)
                    .await?;
            }
            match self.append_in(&mut tx, draft).await {
                Err(error @ WalletError::InsufficientFunds) => {
                    tx.rollback().await?;
                    last_err = Some(error);
                }
                // The key was taken between our read and the append — by a
                // racing copy of this same request or by a different one.
                Err(WalletError::Conflict(_)) => {
                    tx.rollback().await?;
                    return match self.store.get(entry_id).await? {
                        Some(stored) => {
                            self.hold_replay(&stored, &description, Some(&actor), minor, idem_key)
                        }
                        None => Err(WalletError::Conflict(format!(
                            "idempotency key {idem_key:?} is already taken"
                        ))),
                    };
                }
                receipt => {
                    let receipt = receipt?;
                    tx.commit().await?;
                    return Ok(receipt);
                }
            }
        }
        Err(last_err.unwrap_or(WalletError::InsufficientFunds))
    }
    /// The in-process queue [`hold_for_key`](Self::hold_for_key) waits in before
    /// checking out a connection (issue #486).
    ///
    /// The advisory lock inside the transaction serializes one key's holds
    /// against each other; what it cannot do is keep a waiting hold from pinning
    /// the pooled connection it already checked out. Under enough concurrency on
    /// one key every pooled connection is checked-out-and-waiting on the lock,
    /// and the shared pool times out behind them — a key with no constraints
    /// could starve every tenant. The semaphore admits `KEY_QUEUE_WIDTH` into
    /// the locked transaction and parks the rest of the queue on async permits:
    /// a queued hold holds no connection, so the wait is free until it is
    /// admitted or `KEY_QUEUE_WAIT` refuses it as [`WalletError::PoolBusy`], the
    /// same fast-fail a saturated checkout answers.
    ///
    /// The guard lives until the caller's hold resolves, so the permit covers
    /// every retry the pool-split loop runs — the lock's own serialization stays
    /// exactly one waiter's depth inside the transaction, and a semaphore keyed
    /// on the same [`limit_lock_key`] hash can never widen it. Multi-instance
    /// deployments still serialize on the database lock; the queue only removes
    /// the in-process part of the pile-up.
    ///
    /// # Errors
    ///
    /// [`WalletError::PoolBusy`] when the queue stays full past `KEY_QUEUE_WAIT`.
    pub(super) async fn key_queue_wait(
        &self,
        key_id: &Uuid,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, WalletError> {
        let lock_key = limit_lock_key(&self.tenant_id, key_id);
        let semaphore = {
            // A poisoned lock means another thread panicked mid-update; handing
            // out permits from half-updated queue state would be worse.
            #[expect(clippy::expect_used, reason = "a poisoned lock must panic")]
            let mut queues = self.key_queue.lock().expect("key queue mutex poisoned");
            queues
                .entry(lock_key)
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(KEY_QUEUE_WIDTH)))
                .clone()
        };
        let permit = tokio::time::timeout(KEY_QUEUE_WAIT, semaphore.acquire_owned())
            .await
            .map_err(|_| {
                // The queue drained nothing in the caller's patience window —
                // refuse like a saturated checkout rather than queue deeper.
                WalletError::PoolBusy
            })?
            .map_err(|_| WalletError::PoolBusy)?;
        // A semaphore nobody queues behind is dead weight in the map; it stays
        // (an idle entry costs a few bytes) because removing it races a waiter
        // that already took the `Arc` — the map bounds itself by the keys that
        // were ever busy, which `max_concurrent_holds`-less churn does not grow.
        Ok(permit)
    }
}
