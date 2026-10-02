use doubleentry::account::AccountRegistry;
use doubleentry::storage::postgres::PostgresStore;
use doubleentry::{
    AccountId, Amount, BalanceKey, BalanceLimit, BalanceQuery, Balanced, Currency, Cursor,
    Description, Direction, Draft, Entry, EntryBatch, EntryId, Hash, IdempotencyKey, Layer,
    LedgerId, LedgerPolicy, LedgerStore, LogIndex, PeriodCalendar, Posting, Provenance,
    SealContext,
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::Date;
use time::macros::date;
use uuid::Uuid;

use crate::error::{WalletError, invalid};
use crate::heads::{Consistency, HeadSigningKey, SignedHead, origin_for, sign_head};
use crate::keys::ActingKey;
use crate::proof::ProofBundle;

/// Money precision: 6 decimal places; 1 credit = 1_000_000 minor, fine enough for per-token pricing.
///
/// Defined in `oxsum-verify`, the crate the browser page shares: the entry encoding —
/// and therefore the content hash — depends on it, so writer and verifier use the one
/// definition.
pub use oxsum_verify::SCALE;
pub type Credits = Amount<SCALE>;

/// Account opening date. Every posting date must not be earlier than this.
const OPENED: Date = date!(2026 - 01 - 01);

const WALLET: &str = "Liabilities:Wallet";
const CASH: &str = "Assets:Cash";
const REVENUE: &str = "Income:Usage";

/// What the wallet account may do: be drawn on only up to what it holds, and never
/// release a reservation it did not take.
///
/// `NoDebitBalance` alone is not enough, and the difference is the whole point: a
/// settlement *credits* the pending layer, and a pending credit neither consumes
/// nor grants room under that rule, so settling an amount that was never held
/// raised the available balance by exactly that amount. `FundedReservations` adds
/// the missing half — the pending layer may not carry a credit of its own — so a
/// release can only give back room a hold first took. It is enforced inside the
/// append, against the balance the entry would leave behind, which is why it holds
/// under concurrency: two settlements cannot both release a reservation only one
/// of them can cover.
///
/// oxsum change (not upstream) on the engine side too: the variant exists for this
/// account. See docs/decisions.md.
const WALLET_LIMIT: BalanceLimit = BalanceLimit::FundedReservations;

/// One tenant's wallet ledger.
///
/// Three fixed accounts:
/// - wallet: the balance owed to the user, a liability. Carries `FundedReservations`:
///   no overdraft, and no release beyond what was reserved.
/// - cash: money received from top-ups.
/// - revenue: income recognized on settlement.
pub struct Wallet {
    store: PostgresStore<SCALE>,
    registry: AccountRegistry,
    wallet: AccountId,
    cash: AccountId,
    revenue: AccountId,
    calendar: PeriodCalendar,
    policy: LedgerPolicy,
    /// The validated tenant id this ledger belongs to: the identity its signed tree heads
    /// are published under (`oxsum/ledgers/<tenant_id>`).
    tenant_id: String,
}

/// Receipt of one posting. `content_hash` is what the user keeps to verify the bill later.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub entry_id: EntryId,
    pub log_index: Option<u64>,
    pub content_hash: Hash,
    /// false means an idempotent replay: nothing new was written.
    pub is_new: bool,
}

/// One ledger entry as the dashboard's transaction log shows it.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Position in the log.
    pub index: u64,
    pub id: String,
    /// What the writer recorded: holds and settlements describe the request.
    pub description: String,
    /// The content hash the Merkle log commits to.
    pub content_hash: String,
}

impl Wallet {
    /// Opens a tenant's ledger on the shared pool, creating the ledger on first use.
    ///
    /// The pool is the process's one pool, shared by every tenant: a wallet is a facade
    /// over a schema, not an owner of connections. Which schema each statement resolves
    /// is pinned per transaction inside `PostgresStore`, so the same connection can serve
    /// one tenant and then the next without carrying anything over — see
    /// docs/decisions.md, "all tenants share one connection pool".
    ///
    /// The schema name is derived server-side from the validated tenant id and is never taken from external input.
    pub async fn open(pool: PgPool, tenant_id: &str) -> Result<Self, WalletError> {
        validate_tenant_id(tenant_id)?;
        let ledger = LedgerId::new(format!("tenant-{tenant_id}")).map_err(invalid)?;
        let schema = format!("ledger_{tenant_id}");
        let store = PostgresStore::<SCALE>::new(pool, ledger).in_schema(&schema);
        store.migrate().await?;

        // Account handles must be restored from storage on restart. Re-registering by
        // path could mint different handle numbers and mispoint historical entries.
        let stored = store.accounts().await?;
        let creating = stored.is_empty();
        let mut registry = if creating {
            let mut r = AccountRegistry::new();
            r.register_path(WALLET, OPENED).map_err(invalid)?;
            r.register_path(CASH, OPENED).map_err(invalid)?;
            r.register_path(REVENUE, OPENED).map_err(invalid)?;
            r
        } else {
            AccountRegistry::from_records(stored).map_err(invalid)?
        };

        // The wallet's limit is oxsum's rule about its own account, so it is applied on
        // every open rather than only where a ledger is created. `register_account`
        // upserts master data, which is what lets a ledger written under a weaker rule be
        // tightened here instead of keeping that rule for the rest of its life.
        let wallet = find_account(&registry, WALLET)?;
        let weakened = registry
            .records()
            .into_iter()
            .find(|r| r.id == wallet)
            .is_none_or(|r| r.account.limit != WALLET_LIMIT);
        if weakened {
            registry.set_limit(wallet, WALLET_LIMIT).map_err(invalid)?;
            let record = registry
                .records()
                .into_iter()
                .find(|r| r.id == wallet)
                .ok_or_else(|| WalletError::InvalidInput(format!("missing account {WALLET}")))?;
            store.register_account(&record).await?;
        }
        // A ledger that did not exist a moment ago has no rows at all, so the other two
        // accounts are written once, with it.
        if creating {
            for record in registry.records() {
                if record.id != wallet {
                    store.register_account(&record).await?;
                }
            }
        }

        Ok(Self {
            wallet,
            cash: find_account(&registry, CASH)?,
            revenue: find_account(&registry, REVENUE)?,
            store,
            registry,
            calendar: PeriodCalendar::new(),
            policy: LedgerPolicy::default(),
            tenant_id: tenant_id.to_owned(),
        })
    }

    /// The tenant id this ledger belongs to: the identity its signed tree heads carry.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Top-up: cash and wallet balance increase together.
    pub async fn top_up(&self, key: &str, minor: i64, on: Date) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        self.append(
            Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
                .debit(self.cash, amt, currency())
                .credit(self.wallet, amt, currency()),
        )
        .await
    }

    /// Hold: reserves part of the wallet balance in the pending layer; the settled balance is untouched.
    ///
    /// The wallet's limit counts the pending layer, so concurrent holds cannot
    /// together exceed the balance. Refused with [`WalletError::InsufficientFunds`]
    /// when the balance cannot cover it.
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
        let amt = positive(minor)?;
        self.append(
            Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
                .with_description(description_of(description)?)
                .post(Posting::debit(self.wallet, amt, currency()).in_layer(Layer::Pending))
                .post(Posting::credit(self.revenue, amt, currency()).in_layer(Layer::Pending)),
        )
        .await
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
    /// wallet's own balance cannot cover it — the key limit never overrides the balance.
    pub async fn hold_for_key(
        &self,
        key: &ActingKey,
        idem_key: &str,
        description: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        let actor = Provenance::none()
            .with_actor(&key.key_id.as_simple().to_string())
            .map_err(invalid)?;
        let mut tx = self.store.pool().begin().await?;
        // The check and the append serialize on this lock, per key. The engine's own
        // append takes the per-tenant lock inside, so the order is always key lock first,
        // tenant lock second, and the engine never takes the key lock: no deadlock, and no
        // tenant-level contention beyond the append lock that already exists.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(limit_lock_key(&self.tenant_id, &key.key_id))
            .execute(&mut *tx)
            .await?;
        // The limit in force now, not the one the request authenticated with: locked, so a
        // PATCH landing between authentication and this hold cannot be missed.
        let limit: Option<Option<i64>> = sqlx::query_scalar(
            "SELECT spend_limit_minor FROM oxsum.api_keys WHERE key_id = $1 FOR UPDATE",
        )
        .bind(key.key_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(limit) = limit else {
            // Keys are never deleted; a missing row means the credential died mid-request.
            return Err(WalletError::Unauthenticated);
        };
        if let Some(limit) = limit {
            let committed = self.key_committed_in(&mut tx, &key.key_id).await?;
            if committed + minor > limit {
                return Err(WalletError::KeyLimitExceeded {
                    limit_minor: limit,
                    committed_minor: committed,
                });
            }
        }
        let receipt = self
            .append(
                Entry::<Draft, SCALE>::new(entry_id_for(idem_key), idem(idem_key)?, on)
                    .with_description(description_of(description)?)
                    .with_provenance(actor)
                    .post(Posting::debit(self.wallet, amt, currency()).in_layer(Layer::Pending))
                    .post(Posting::credit(self.revenue, amt, currency()).in_layer(Layer::Pending)),
            )
            .await?;
        tx.commit().await?;
        Ok(receipt)
    }

    /// What one API key has committed: settled charges plus outstanding holds attributed
    /// to it, in minor units. Read from the ledger's own postings — the pending release
    /// and the settled charge of a settlement both carry the hold's actor — so it cannot
    /// drift from the books.
    pub async fn key_committed(&self, key_id: &Uuid) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.key_committed_in(&mut conn, key_id).await
    }

    /// The [`key_committed`](Self::key_committed) read, on the caller's connection: the
    /// limit check runs it inside the transaction that holds the per-key advisory lock.
    async fn key_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        key_id: &Uuid,
    ) -> Result<i64, WalletError> {
        // The schema name is assembled from the validated tenant id, like `Wallet::open`
        // does; the actor is the key id in uuid simple form, bound as a parameter.
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = $1 AND e.provenance_actor = $2",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(self.wallet.index() as i32)
            .bind(key_id.as_simple().to_string())
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }

    /// Settle: one entry does two things — releases the hold (a reversal in the pending layer)
    /// and charges the actual usage (recorded in the settled layer).
    ///
    /// The settlement names the hold it releases: `hold_key` is the idempotency key the hold was
    /// taken under. The held amount is read from the hold entry in the ledger — the entry's
    /// pending-layer debit on the wallet account — so there is nothing for the caller to assert
    /// and nothing for a caller to get wrong. The hold's provenance actor is copied onto the
    /// settlement entry, so both the release and the charge attribute to the key that held.
    ///
    /// `actual` may be less than the held amount; the difference returns to the available
    /// balance. `actual` of zero amounts to a full release.
    ///
    /// One hold settles at most once. The settlement entry's idempotency key is
    /// [`settlement_key_for`] of the hold's key, so the ledger's idempotency gate — inside the
    /// append — refuses a second settlement of the same hold with [`WalletError::Conflict`]:
    /// concurrent settlements naming one hold cannot both release it, and retrying the
    /// identical settlement replays it. The wallet's limit still refuses a release the
    /// outstanding reservations cannot cover, as the backstop behind the pairing.
    ///
    /// Naming a hold that is not outstanding — no entry under the key, or an entry that is not
    /// a hold — is [`WalletError::HoldNotFound`], refused even when other holds would cover the
    /// amount.
    ///
    /// `description` is what the entry says it is, in the caller's words — for a gateway turn, the
    /// record built by [`crate::Settlement::description`], which carries the token counts and the
    /// prices so a bill proves the arithmetic and not only the total. It must be derived from the
    /// request alone: two settlements of one hold with different descriptions are different
    /// requests, and the ledger refuses the second.
    pub async fn settle(
        &self,
        hold_key: &str,
        description: &str,
        actual_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        // A call's own argument is bounded before the ledger is consulted.
        idem(hold_key)?;
        if actual_minor < 0 {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let (held_minor, outstanding_actor) = self.outstanding_hold(hold_key).await?;
        if actual_minor > held_minor {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let held = Credits::from_minor(held_minor);
        let settle_key = settlement_key_for(hold_key);
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id_for(&settle_key), idem(&settle_key)?, on)
                .with_description(description_of(description)?);
        // The settlement attributes to the key the hold attributed to: the release side and
        // the settled charge side both count toward that key's committed spend, with no
        // caller input to get wrong. A hold taken before key attribution existed settles
        // unattributed, like it was held.
        if let Some(actor) = outstanding_actor {
            draft = draft.with_provenance(Provenance::none().with_actor(&actor).map_err(invalid)?);
        }
        draft = draft
            .post(Posting::credit(self.wallet, held, currency()).in_layer(Layer::Pending))
            .post(Posting::debit(self.revenue, held, currency()).in_layer(Layer::Pending));
        if actual_minor > 0 {
            let actual = Credits::from_minor(actual_minor);
            draft = draft.debit(self.wallet, actual, currency()).credit(
                self.revenue,
                actual,
                currency(),
            );
        }
        match self.append(draft).await {
            // The settlement's idempotency key is derived from the hold's key, so a key
            // conflict here means exactly one thing: the hold was already settled.
            Err(WalletError::Conflict(_)) => {
                Err(WalletError::Conflict("hold already settled".into()))
            }
            other => other,
        }
    }

    /// Available balance in minor units = settled balance - unsettled holds.
    pub async fn available(&self) -> Result<i64, WalletError> {
        Ok(self.settled().await? - self.reserved().await?)
    }

    /// Credits that have settled into the wallet: what top-ups put there, less what settlements
    /// have charged.
    ///
    /// Separate from [`reserved`](Self::reserved) because a hold and a settlement are answered
    /// from different layers: a hold draws on the two together, a settlement gives back part of
    /// the reserved one.
    pub async fn settled(&self) -> Result<i64, WalletError> {
        self.wallet_net(Layer::Settled).await
    }

    /// Credits currently reserved by holds that have not been settled yet.
    ///
    /// Never negative: the wallet's limit keeps the pending layer on the hold side, which is what
    /// makes this the ceiling on what a settlement may release.
    pub async fn reserved(&self) -> Result<i64, WalletError> {
        Ok(-self.wallet_net(Layer::Pending).await?)
    }

    /// The amount of the hold taken under `hold_key`, read from the hold entry in the ledger,
    /// plus the provenance actor the hold attributed to — the API key whose spend it counts
    /// toward, if the hold named one.
    ///
    /// The hold entry is found by the id it was given when the hold was taken
    /// ([`entry_id_for`] of the key), and the amount is the entry's pending-layer debit on the
    /// wallet account — the ledger's own record of what was reserved, not a caller assertion.
    /// [`WalletError::HoldNotFound`] when no entry is stored under the key, or when the entry
    /// is not a hold.
    async fn outstanding_hold(&self, hold_key: &str) -> Result<(i64, Option<String>), WalletError> {
        let Some(stored) = self.store.get(entry_id_for(hold_key)).await? else {
            return Err(WalletError::HoldNotFound(format!(
                "no hold under key {hold_key:?}"
            )));
        };
        let held = stored
            .entry
            .postings()
            .iter()
            .filter(|p| p.account == self.wallet && p.layer == Layer::Pending)
            .map(|p| match p.direction {
                Direction::Debit => p.amount.to_minor(),
                Direction::Credit => -p.amount.to_minor(),
            })
            .sum::<i64>();
        if held <= 0 {
            return Err(WalletError::HoldNotFound(format!(
                "key {hold_key:?} does not name a hold"
            )));
        }
        let actor = stored
            .entry
            .provenance()
            .actor
            .as_ref()
            .map(|actor| actor.as_str().to_owned());
        Ok((held, actor))
    }

    /// Builds the proof bundle for one entry. Returns None when the entry does not exist.
    pub async fn receipt_proof(
        &self,
        entry_id: EntryId,
    ) -> Result<Option<ProofBundle>, WalletError> {
        let Some(stored) = self.store.get(entry_id).await? else {
            return Ok(None);
        };
        let index = stored.require_index().map_err(invalid)?;
        let head = self.store.head().await?;
        let proof = self.store.prove_inclusion(index).await?;
        Ok(Some(ProofBundle {
            entry: stored.entry,
            head,
            proof,
        }))
    }

    /// Number of entries in the log. With isolated ledgers, each tenant counts from zero.
    pub async fn log_size(&self) -> Result<u64, WalletError> {
        Ok(self.store.head().await?.size)
    }

    /// The current tree head, signed by the operator: what `GET /api/v1/log/head` serves.
    ///
    /// The note attests the head at signing time; the ledger keeps growing underneath, and a
    /// later call signs a later head. Nothing is stored: the signature is recomputed from the
    /// key, so rotating the key rotates every head without a migration.
    pub async fn signed_head(&self, key: &HeadSigningKey) -> Result<SignedHead, WalletError> {
        let origin = origin_for(&self.tenant_id).map_err(invalid)?;
        let head = self.store.head().await?;
        sign_head(key, &origin, head).map_err(invalid)
    }

    /// A consistency proof from an earlier size to the current head, with the new head signed:
    /// what `GET /api/v1/log/consistency` serves.
    ///
    /// `from` must name a real head: at least 1 — every log extends the empty tree, so a
    /// proof from size 0 would verify against any history at all and is refused — and at most
    /// the current size. The old head is recomputed from the log, not taken from the caller,
    /// so the two heads and the proof always agree with each other.
    pub async fn consistency(
        &self,
        from: u64,
        key: &HeadSigningKey,
    ) -> Result<Consistency, WalletError> {
        if from == 0 {
            return Err(WalletError::InvalidInput(
                "consistency proofs start at size 1: a proof from the empty tree would verify against any history".into(),
            ));
        }
        let head = self.store.head().await?;
        if from > head.size {
            return Err(WalletError::InvalidInput(format!(
                "cannot prove consistency from size {from} against a log of size {}",
                head.size
            )));
        }
        // The proof is built against the head just read, and the response signs that same
        // head: a write landing between the two leaves the answer one entry behind the
        // absolute latest, still a true statement about the head it attests.
        let proof = self
            .store
            .prove_consistency_between(from, head.size)
            .await
            .map_err(invalid)?;
        let old_head = self.store.head_at(from).await.map_err(invalid)?;
        let origin = origin_for(&self.tenant_id).map_err(invalid)?;
        Ok(Consistency {
            signed: sign_head(key, &origin, head).map_err(invalid)?,
            old_head,
            proof,
        })
    }

    /// The newest entries of the log, newest first, at most `limit`.
    ///
    /// What the dashboard's transaction log lists: the position, the id, the description
    /// the writer recorded, and the content hash the Merkle log commits to. The proof
    /// page (a later item) is what verifies one; this only lists them.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_entries(&self, limit: usize) -> Result<Vec<LogEntry>, WalletError> {
        let size = self.store.head().await?.size;
        let start = size.saturating_sub(limit.clamp(1, 100) as u64);
        let after = start.checked_sub(1).map(LogIndex::new);
        let page = self
            .store
            .page(Cursor {
                after,
                limit: limit.clamp(1, 100),
            })
            .await?;
        let mut entries: Vec<LogEntry> = page
            .records
            .into_iter()
            .map(|stored| LogEntry {
                index: stored
                    .require_index()
                    .map(LogIndex::get)
                    .unwrap_or(u64::MAX),
                id: stored.entry.id().to_string(),
                description: stored.entry.description().as_str().to_owned(),
                content_hash: stored.content_hash.to_string(),
            })
            .collect();
        entries.reverse();
        Ok(entries)
    }

    async fn wallet_net(&self, layer: Layer) -> Result<i64, WalletError> {
        let b = self
            .store
            .balance(
                BalanceKey {
                    account: self.wallet,
                    currency: currency(),
                    layer,
                },
                BalanceQuery::all(),
            )
            .await?;
        // The wallet is a liability: credit balances are the positive side.
        Ok(b.credits.to_minor() - b.debits.to_minor())
    }

    fn seal(&self, draft: Entry<Draft, SCALE>) -> Result<Entry<Balanced, SCALE>, WalletError> {
        draft
            .seal(&SealContext {
                accounts: &self.registry,
                calendar: &self.calendar,
                policy: &self.policy,
            })
            .map_err(invalid)
    }

    async fn append(&self, draft: Entry<Draft, SCALE>) -> Result<Receipt, WalletError> {
        let entry = self.seal(draft)?;
        let recorded = self.store.append(&EntryBatch::single(entry)).await?;
        let r = recorded
            .into_iter()
            .next()
            .ok_or_else(|| WalletError::InvalidInput("empty append result".into()))?;
        Ok(Receipt {
            entry_id: r.id,
            log_index: r.index.map(|i| i.get()),
            content_hash: r.content_hash,
            is_new: r.is_new,
        })
    }
}

/// The handle of the account registered at `path`, or an error naming it.
///
/// A ledger whose accounts are not the three this module registers is not a wallet;
/// saying which one is missing beats a panic or a silent default.
fn find_account(registry: &AccountRegistry, path: &str) -> Result<AccountId, WalletError> {
    registry
        .records()
        .into_iter()
        .find(|r| r.account.path.to_string() == path)
        .map(|r| r.id)
        .ok_or_else(|| WalletError::InvalidInput(format!("missing account {path}")))
}

/// Tenant ids end up inside schema names, so only lowercase letters, digits and underscores are allowed, 1 to 40 chars.
fn validate_tenant_id(tenant_id: &str) -> Result<(), WalletError> {
    let ok = !tenant_id.is_empty()
        && tenant_id.len() <= 40
        && tenant_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(WalletError::InvalidInput(format!(
            "bad tenant id {tenant_id:?}"
        )))
    }
}

fn currency() -> Currency {
    Currency::USD
}

fn positive(minor: i64) -> Result<Credits, WalletError> {
    if minor <= 0 {
        return Err(WalletError::InvalidInput("amount must be positive".into()));
    }
    Ok(Credits::from_minor(minor))
}

fn idem(key: &str) -> Result<IdempotencyKey, WalletError> {
    IdempotencyKey::new(key.as_bytes().to_vec()).map_err(invalid)
}

/// The entry's description, validated by the ledger: it caps the length and refuses control
/// characters, so a caller cannot smuggle either into a stored entry.
fn description_of(text: &str) -> Result<Description, WalletError> {
    Description::new(text.to_owned()).map_err(invalid)
}

/// Derives the EntryId deterministically from the idempotency key, so a retry always lands on the
/// same entry.
///
/// Public because the derivation is part of the interface: a gateway hold is taken under
/// `req-<id>:hold`, and a caller holding a request id can name that entry without asking the
/// server (docs/api.md).
#[must_use]
pub fn entry_id_for(key: &str) -> EntryId {
    EntryId::from_uuid(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        key.as_bytes(),
    ))
}

/// The idempotency key of the settlement that releases the hold taken under `hold_key`.
///
/// One hold settles at most once, so the hold's key is the settlement's idempotency key in
/// derived form: retrying the same settlement replays it, and settling the same hold twice is
/// refused by the ledger's idempotency gate, inside the append — which is what makes the
/// pairing hold under concurrency. The derivation hashes the hold key because a hold key may
/// already sit at the engine's 128-byte idempotency limit, where a readable suffix would
/// overflow it.
///
/// Public because the derivation is part of the interface: the settlement entry's id is
/// [`entry_id_for`] of this key, so a caller holding a hold key can name the settlement entry
/// without asking the server (docs/api.md).
#[must_use]
pub fn settlement_key_for(hold_key: &str) -> String {
    format!("settle:{}", entry_id_for(hold_key).as_uuid().as_simple())
}

/// The advisory-lock key serializing one key's limit check with its appends.
///
/// A stable hash of the tenant id and the key id: every process holding the same key
/// computes the same lock, and a key of one tenant can never collide with a key of
/// another. The domain prefix keeps it out of the engine's lock namespace.
fn limit_lock_key(tenant_id: &str, key_id: &Uuid) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(b"oxsum/key-spend-limit/v1\0");
    hasher.update(tenant_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(key_id.as_bytes());
    let digest = hasher.finalize();
    i64::from_le_bytes(digest[..8].try_into().expect("SHA-256 is 32 bytes"))
}

#[cfg(test)]
mod tests {
    use super::validate_tenant_id;

    #[test]
    fn tenant_id_rules() {
        assert!(validate_tenant_id("acme_01").is_ok());
        assert!(validate_tenant_id("").is_err());
        assert!(validate_tenant_id("Acme").is_err());
        assert!(validate_tenant_id("a-b").is_err());
        assert!(validate_tenant_id("x\"; DROP SCHEMA").is_err());
        assert!(validate_tenant_id(&"a".repeat(41)).is_err());
    }
}
