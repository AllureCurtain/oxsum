//! Deposits: the single record of money arriving, one row per rail payment (decision D2,
//! issue #118).
//!
//! Every rail — the manual top-up, a redemption code, later Stripe or a chain watcher —
//! writes one row on `oxsum.deposits` keyed by `(rail, payment_ref)`, and every row that
//! reaches `credited` names the ledger entry that moved the wallet. The purchased pool is
//! what deposits credit: granted credit never arrives through a rail.
//!
//! The redemption rail owns `oxsum.redemption_codes`: the operator mints a batch of
//! `oxr-` codes and the holder redeems one through [`Db::redeem_code`]. Like invitation
//! tokens and API keys the table keeps only the code's SHA-256, so a dump redeems
//! nothing and the mint answer is the only place the plaintext exists.

use std::fmt::Write;

use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::wallet::{Receipt, Wallet};

/// Every redemption code starts with this, so the value is recognizable in logs.
const CODE_MARK: &str = "oxr-";
const CODE_BYTES: usize = 32;
/// The most codes one mint call may produce.
pub const MAX_BATCH: i64 = 1000;

/// The rail `POST /api/v1/topups` writes under.
pub const RAIL_MANUAL: &str = "manual";
/// The rail redemption codes write under.
pub const RAIL_REDEMPTION: &str = "redemption";

/// A freshly minted batch: the only time the codes are visible.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeBatch {
    pub batch_id: Uuid,
    pub count: i64,
    pub amount_minor: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    pub codes: Vec<String>,
}

/// What a redeem answers: the ledger receipt of the credit and the amount it carried.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Redemption {
    #[serde(flatten)]
    pub receipt: Receipt,
    pub amount_minor: i64,
}

impl Db {
    /// Records a manual-rail deposit for a credit the wallet already booked: every
    /// `POST /topups` leaves a row here too, so `deposits` is the one table
    /// reconciliation (P4-1) reads for money in. The caller's idempotency key is the
    /// payment reference — a replayed top-up sees its own row and writes nothing.
    pub async fn record_manual_deposit(
        &self,
        organization_id: Uuid,
        idempotency_key: &str,
        amount_minor: i64,
        entry_id: Uuid,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, \
                  received_minor, status, entry_id) \
             VALUES ($1, $2, $3, $4, $5, $5, 'credited', $6) \
             ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(organization_id)
        .bind(RAIL_MANUAL)
        .bind(idempotency_key)
        .bind(amount_minor)
        .bind(entry_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Mints `count` redemption codes worth `amount_minor` each: `oxr-` plus 32 random
    /// bytes from the operating system's CSPRNG. Only the SHA-256 of each lands in the
    /// table — this answer is the only place the codes are readable.
    ///
    /// `expires_at` may be null: a code without an expiry never becomes invalid on its
    /// own. One batch shares one `batch_id`, so one mint call is traceable as a group
    /// in deposit metadata and a future admin list.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a count outside `1..=MAX_BATCH` or a
    /// non-positive amount; storage failures surface as [`WalletError`].
    pub async fn mint_codes(
        &self,
        count: i64,
        amount_minor: i64,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<CodeBatch, WalletError> {
        if !(1..=MAX_BATCH).contains(&count) {
            return Err(WalletError::InvalidInput(format!(
                "count must be 1..={MAX_BATCH}"
            )));
        }
        if amount_minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a code's amount must be positive".into(),
            ));
        }
        if expires_at.is_some_and(|at| at <= OffsetDateTime::now_utc()) {
            return Err(WalletError::InvalidInput(
                "an expiry must be in the future".into(),
            ));
        }
        let batch_id = Uuid::new_v4();
        let mut codes = Vec::with_capacity(count as usize);
        let mut tx = self.pool().begin().await?;
        for _ in 0..count {
            let code = generate_code();
            sqlx::query(
                "INSERT INTO oxsum.redemption_codes \
                     (code_id, code_hash, batch_id, amount_minor, expires_at) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(Uuid::new_v4())
            .bind(hash_code(&code).as_slice())
            .bind(batch_id)
            .bind(amount_minor)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;
            codes.push(code);
        }
        tx.commit().await?;
        Ok(CodeBatch {
            batch_id,
            count,
            amount_minor,
            expires_at,
            codes,
        })
    }

    /// Redeems a code into `organization`'s wallet: one atomic claim, then the ledger
    /// credit, then the deposit closed as credited.
    ///
    /// The claim is the invitation shape — the code's row is taken `FOR UPDATE`, the
    /// spend and the deposit (`confirmed`) commit in one transaction, so two
    /// redeems of one code cannot both win. The ledger append runs under the fixed
    /// idempotency key `redemption:<code_id>` after the claim commits: a replay
    /// rewrites nothing, and a redeem retried after a crash finds its own
    /// confirmed deposit and resumes the credit instead of opening a second one.
    /// The deposit then flips to `credited` carrying the entry that moved the money.
    ///
    /// A code that is unknown, spent by another organization or expired is
    /// [`WalletError::NotFound`] whichever it is — which of the three is wrong is the
    /// holder's business. A code this same organization already redeemed replays
    /// its own credit, so a caller's retry after a lost answer still gets a receipt.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unusable code; storage and ledger failures
    /// surface as [`WalletError`].
    pub async fn redeem_code(
        &self,
        wallet: &Wallet,
        organization_id: Uuid,
        code: &str,
        on: time::Date,
    ) -> Result<Redemption, WalletError> {
        let invalid = || WalletError::NotFound("the code is not valid".into());
        if code.len() != CODE_MARK.len() + CODE_BYTES * 2 || !code.starts_with(CODE_MARK) {
            return Err(invalid());
        }
        // The claim transaction: lock the row, spend it, and record the deposit —
        // all or nothing, so the ledger credit that follows is always replayable.
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT code_id, batch_id, amount_minor, redeemed_by, deposit_id \
             FROM oxsum.redemption_codes \
             WHERE code_hash = $1 AND (expires_at IS NULL OR expires_at > now()) \
             FOR UPDATE",
        )
        .bind(hash_code(code).as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Err(invalid());
        };
        let code_id: Uuid = row.try_get("code_id")?;
        let amount_minor: i64 = row.try_get("amount_minor")?;
        let redeemed_by: Option<Uuid> = row.try_get("redeemed_by")?;
        let deposit_id: Option<Uuid> = row.try_get("deposit_id")?;
        let deposit_id = match redeemed_by {
            // This organization's own redeem, retried: resume the deposit it opened.
            Some(by) if by == organization_id => deposit_id.ok_or_else(|| {
                WalletError::InvalidInput("spent code without its deposit".into())
            })?,
            // Unspent: claim it for this organization, opening the deposit in the
            // same transaction.
            None => {
                let id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO oxsum.deposits \
                         (deposit_id, organization_id, rail, payment_ref, amount_minor, \
                          received_minor, status, meta) \
                     VALUES ($1, $2, $3, $4, $5, $5, 'confirmed', $6)",
                )
                .bind(id)
                .bind(organization_id)
                .bind(RAIL_REDEMPTION)
                .bind(code_id.to_string())
                .bind(amount_minor)
                .bind(serde_json::json!({ "batchId": row.try_get::<Uuid, _>("batch_id")? }))
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE oxsum.redemption_codes \
                     SET redeemed_at = now(), redeemed_by = $1, deposit_id = $2 \
                     WHERE code_id = $3",
                )
                .bind(organization_id)
                .bind(id)
                .bind(code_id)
                .execute(&mut *tx)
                .await?;
                id
            }
            // Spent by another organization: indistinguishable from a bad code.
            Some(_) => return Err(invalid()),
        };
        tx.commit().await?;

        // The credit replays under its own key: a redeploy of this same redeem — the
        // caller's retry or a second in-flight call — writes nothing twice.
        let receipt = wallet
            .top_up(&redeem_key(code_id), amount_minor, on)
            .await?;
        sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'credited', entry_id = $1, updated_at = now() \
             WHERE deposit_id = $2",
        )
        .bind(*receipt.entry_id.as_uuid())
        .bind(deposit_id)
        .execute(self.pool())
        .await?;
        Ok(Redemption {
            receipt,
            amount_minor,
        })
    }
}

/// The idempotency key a code's credit writes under — fixed, so a credit is replayable
/// without ever looking the code up twice.
fn redeem_key(code_id: Uuid) -> String {
    format!("redemption:{code_id}")
}

/// A new redemption code: the mark plus 32 random bytes, hex-encoded like an API key
/// secret or an invitation token.
fn generate_code() -> String {
    let mut bytes = [0u8; CODE_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut code = String::with_capacity(CODE_MARK.len() + CODE_BYTES * 2);
    code.push_str(CODE_MARK);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(code, "{byte:02x}");
    }
    code
}

/// SHA-256 of the code, the only form the database ever holds.
fn hash_code(code: &str) -> Vec<u8> {
    Sha256::digest(code.as_bytes()).to_vec()
}
