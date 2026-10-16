//! Signed spend mandates (issue #262): a key holder delegates a strictly
//! narrower slice of a key's authority to a bearer who does not hold the key.
//!
//! A mandate is a self-contained, offline-verifiable authorization. The
//! organization mints it with `POST /api/v1/org/keys/{keyId}/mandates`; the
//! delegate presents it in `X-Oxsum-Mandate` on `/v1/*` and `POST
//! /api/v1/holds`. Inside `hold_for_key`, after the key's own constraints run,
//! the mandate's bounds apply on top — cap, model allowlist and validity window
//! — and the committed spend it is checked against is read from the ledger,
//! never from process state.
//!
//! The signature is Ed25519 over a canonical JSON body: the same pinned-wire
//! rule `verify_charge` applies to settlement descriptions, so the bytes the
//! holder signed are the bytes the verifier recomputes. Signing is asymmetric
//! on purpose — a mandate's signature must check against a *public* key a third
//! party can fetch, not a server-only secret — so each key carries a
//! `mandate_signing_seed` sealed under `OXSUM_SECRET_KEY`, and only the public
//! half ever leaves the database.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::sessions::KeyScope;

/// The canonical body's field ordering, enforced by hand: serde's map
/// serializer keeps insertion order, so the writer and the reader agree on the
/// byte sequence the signature covers. Sorted keys, compact separators, no
/// whitespace — the rule `verify_charge` uses.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MandateBody<'a> {
    cap_minor: i64,
    key_id: &'a str,
    mandate_id: &'a str,
    models: &'a Option<Vec<String>>,
    nonce: &'a str,
    not_after: i64,
    not_before: i64,
    org_id: &'a str,
}

/// The wire form the delegate carries: the canonical body plus its signature.
/// `sig` signs the canonical serialization of the fields *without* `sig`, so
/// the bytes the signature covers are the same bytes a verifier rebuilds.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MandateWire {
    cap_minor: i64,
    key_id: String,
    mandate_id: String,
    models: Option<Vec<String>>,
    nonce: String,
    not_after: i64,
    not_before: i64,
    org_id: String,
    sig: String,
}

/// A signed spend mandate as the API lists it: the fields a mandate commits
/// to, never the seed that signed it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mandate {
    /// The stable id a mandate's committed spend keys on.
    pub mandate_id: Uuid,
    /// The key this delegates.
    pub key_id: Uuid,
    /// The organization the key spends for — defense in depth.
    pub organization_id: Uuid,
    /// Total committed spend this mandate may carry, in minor units.
    pub cap_minor: i64,
    /// The model allowlist — a subset of the key's own; `None` means all the
    /// key serves.
    pub models: Option<Vec<String>>,
    /// When the mandate starts being honored.
    #[serde(with = "time::serde::rfc3339")]
    pub not_before: OffsetDateTime,
    /// When it expires — there is no revocation, so mandates are short-lived.
    #[serde(with = "time::serde::rfc3339")]
    pub not_after: OffsetDateTime,
    /// Uniqueness, so two identical mints are two different mandates.
    pub nonce: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A mandate plus its signed blob, returned exactly once: at mint.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedMandate {
    #[serde(flatten)]
    pub mandate: Mandate,
    /// base64url of the canonical body plus its Ed25519 signature — what the
    /// delegate presents in `X-Oxsum-Mandate`.
    pub mandate_blob: String,
}

/// A mandate verified against a key: the fields `hold_for_key` enforces.
#[derive(Debug, Clone)]
pub struct VerifiedMandate {
    /// The mandate's own id — the `mandate_id` `usage_records`/`open_holds`
    /// record under.
    pub mandate_id: Uuid,
    /// The key the mandate delegates — checked against the acting key.
    pub key_id: Uuid,
    /// The organization it spends for — defense in depth.
    pub organization_id: Uuid,
    /// Total committed spend this mandate may carry.
    pub cap_minor: i64,
    /// The model allowlist, narrower than the key's or `None`.
    pub models: Option<Vec<String>>,
    /// The window the mandate is honored inside, unix seconds.
    pub not_before: i64,
    pub not_after: i64,
    /// The mandate's own digest, for the settlement description's v7 `mandate`
    /// field.
    pub hash: String,
}

/// The mandate the delegate presented, parsed and signature-verified against
/// the key's public half. What `hold_for_key` checks before money moves.
#[derive(Debug, Clone)]
pub struct PresentedMandate {
    /// The verified body fields.
    pub verified: VerifiedMandate,
    /// The raw blob as presented, so the settled description can name the
    /// mandate's own digest.
    pub blob: String,
}

impl PresentedMandate {
    /// `notBefore` as unix seconds — the mandate is honored at or after it.
    pub fn not_before_ts(&self) -> i64 {
        self.verified.not_before
    }

    /// `notAfter` as unix seconds — the mandate stops being honored past it.
    pub fn not_after_ts(&self) -> i64 {
        self.verified.not_after
    }
}

/// Serializes the unsigned fields into the canonical byte string the
/// signature covers: compact JSON, keys in declaration order (which is the
/// sorted order the fields are declared in), no whitespace.
fn canonical_body(mandate: &Mandate) -> String {
    // `serde_json` keeps struct field order, which is the canonical order the
    // fields are declared in — sorted keys emerge from the declaration, not a
    // map. The signature covers these exact bytes.
    serde_json::to_string(&MandateBody {
        cap_minor: mandate.cap_minor,
        key_id: &mandate.key_id.as_simple().to_string(),
        mandate_id: &mandate.mandate_id.as_simple().to_string(),
        models: &mandate.models,
        nonce: &mandate.nonce,
        not_after: mandate.not_after.unix_timestamp(),
        not_before: mandate.not_before.unix_timestamp(),
        org_id: &mandate.organization_id.as_simple().to_string(),
    })
    .unwrap_or_default()
}

/// The mandate's digest: sha256 of the signed blob — the identifier the
/// settlement description's v7 `mandate.hash` carries, so a settled bill proves
/// which delegation authorized it.
fn mandate_hash(blob: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(blob.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Signs a mandate's canonical body under the key's seed. The blob is the
/// canonical JSON with `sig` appended — still canonical — then base64url'd.
fn sign_mandate(mandate: &Mandate, seed: &[u8; 32]) -> String {
    let signing = SigningKey::from_bytes(seed);
    let body = canonical_body(mandate);
    let sig = signing.sign(body.as_bytes());
    // The signed wire form: the canonical body with `sig` appended, base64url.
    let wire = MandateWire {
        cap_minor: mandate.cap_minor,
        key_id: mandate.key_id.as_simple().to_string(),
        mandate_id: mandate.mandate_id.as_simple().to_string(),
        models: mandate.models.clone(),
        nonce: mandate.nonce.clone(),
        not_after: mandate.not_after.unix_timestamp(),
        not_before: mandate.not_before.unix_timestamp(),
        org_id: mandate.organization_id.as_simple().to_string(),
        sig: BASE64URL.encode(sig.to_bytes()),
    };
    BASE64URL.encode(serde_json::to_string(&wire).unwrap_or_default())
}

/// Parses and verifies a presented `X-Oxsum-Mandate` blob against the key's
/// public half. `None` answers any way the blob can fail — bad base64, bad
/// JSON, a bad signature — indistinguishable to the caller, because which check
/// caught it is the verifier's business.
pub fn parse_presented(blob: &str, verifying: &VerifyingKey) -> Option<PresentedMandate> {
    let json = BASE64URL.decode(blob.trim()).ok()?;
    let wire: MandateWire = serde_json::from_slice(&json).ok()?;
    let sig_bytes: [u8; 64] = BASE64URL.decode(&wire.sig).ok()?.try_into().ok()?;
    let signature = Signature::from_bytes(&sig_bytes);
    // Rebuild the canonical body from the signed fields and verify the
    // signature over exactly those bytes — the pinned-wire rule.
    let body = serde_json::to_string(&MandateBody {
        cap_minor: wire.cap_minor,
        key_id: &wire.key_id,
        mandate_id: &wire.mandate_id,
        models: &wire.models,
        nonce: &wire.nonce,
        not_after: wire.not_after,
        not_before: wire.not_before,
        org_id: &wire.org_id,
    })
    .ok()?;
    verifying.verify_strict(body.as_bytes(), &signature).ok()?;
    Some(PresentedMandate {
        verified: VerifiedMandate {
            mandate_id: Uuid::parse_str(&wire.mandate_id).ok()?,
            key_id: Uuid::parse_str(&wire.key_id).ok()?,
            organization_id: Uuid::parse_str(&wire.org_id).ok()?,
            cap_minor: wire.cap_minor,
            models: wire.models,
            not_before: wire.not_before,
            not_after: wire.not_after,
            hash: mandate_hash(blob),
        },
        blob: blob.to_owned(),
    })
}

impl Db {
    /// Mints a mandate bound to a key of the organization, signed under the
    /// key's `mandate_signing_seed`.
    ///
    /// `cap` and `models` may only *narrow* the key's own constraints — a cap
    /// above the key's `spend_limit_minor`, or a model outside its allowlist,
    /// is refused. `not_after` bounds how long the delegate may spend: mandates
    /// are `notAfter`-bounded and never revoked (a key that must stop
    /// delegating revokes the key or re-times its constraints). `None` returned
    /// means the organization has no such key — or, for a member, not one they
    /// created — so a mandate id cannot be probed.
    ///
    /// # Errors
    ///
    /// An out-of-bounds cap, a model outside the key's allowlist or an inverted
    /// window is [`WalletError::InvalidInput`]; a malformed stored seed is
    /// [`WalletError::Misconfigured`]; storage failures surface as
    /// [`WalletError`].
    #[allow(clippy::too_many_arguments)]
    pub async fn create_mandate(
        &self,
        organization_id: Uuid,
        key_id: Uuid,
        scope: KeyScope,
        cap_minor: i64,
        models: Option<Vec<String>>,
        not_before: Option<OffsetDateTime>,
        not_after: OffsetDateTime,
        secret: &crate::channels::SecretKey,
    ) -> Result<Option<CreatedMandate>, WalletError> {
        // One transaction for the whole mint: the seed is lazily written under
        // the key row's lock, so two racing mints cannot both see NULL and
        // produce mandates signed under different seeds — the second waits for
        // the first's commit and reads its seed.
        let mut tx = self.pool().begin().await?;
        // The key row, scoped like the constraint write: a member delegates only
        // the keys they created, so a key id outside the scope is not found.
        let key = match scope {
            KeyScope::Own(user_id) => {
                sqlx::query(
                    "SELECT spend_limit_minor, model_allowlist, mandate_signing_seed \
                     FROM oxsum.api_keys \
                     WHERE organization_id = $1 AND key_id = $2 AND created_by = $3 \
                     FOR UPDATE",
                )
                .bind(organization_id)
                .bind(key_id)
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await?
            }
            KeyScope::Organization | KeyScope::All => {
                sqlx::query(
                    "SELECT spend_limit_minor, model_allowlist, mandate_signing_seed \
                     FROM oxsum.api_keys WHERE organization_id = $1 AND key_id = $2 \
                     FOR UPDATE",
                )
                .bind(organization_id)
                .bind(key_id)
                .fetch_optional(&mut *tx)
                .await?
            }
        };
        let Some(key) = key else {
            tx.rollback().await?;
            return Ok(None);
        };

        // A mandate can only narrow: the cap sits under the key's own limit,
        // and the allowlist is a subset of the key's — wider is refused at
        // mint, so the delegate never holds bounds the key itself does not.
        let spend_limit: Option<i64> = key.try_get("spend_limit_minor")?;
        if let Some(limit) = spend_limit
            && cap_minor > limit
        {
            return Err(WalletError::InvalidInput(format!(
                "capMinor must sit inside the key's spendLimitMinor of {limit}"
            )));
        }
        let key_models: Option<Vec<String>> = key.try_get("model_allowlist")?;
        if let (Some(models), Some(allowed)) = (&models, &key_models)
            && let Some(model) = models.iter().find(|m| !allowed.contains(m))
        {
            return Err(WalletError::InvalidInput(format!(
                "mandate model {model:?} is outside the key's modelAllowlist"
            )));
        }
        let not_before = not_before.unwrap_or_else(OffsetDateTime::now_utc);
        if not_after <= not_before {
            return Err(WalletError::InvalidInput(
                "notAfter must be after notBefore".into(),
            ));
        }

        // The signing seed is minted lazily, on the first mandate the key
        // delegates: keys that predate the column carry none, and a key that
        // never delegates never needs one. The row is locked so two racing
        // mints cannot both write a seed — the second sees the first's.
        let seed_text = {
            let seed_sealed: Option<Vec<u8>> = key.try_get("mandate_signing_seed")?;
            match seed_sealed {
                Some(sealed) => secret.open(&String::from_utf8_lossy(&sealed))?,
                None => {
                    let mut seed = [0u8; 32];
                    rand::rng().fill_bytes(&mut seed);
                    let text = BASE64URL.encode(seed);
                    let sealed = secret.seal(&text)?;
                    sqlx::query(
                        "UPDATE oxsum.api_keys SET mandate_signing_seed = $2 \
                         WHERE key_id = $1 AND mandate_signing_seed IS NULL",
                    )
                    .bind(key_id)
                    .bind(sealed.as_bytes())
                    .execute(&mut *tx)
                    .await?;
                    text
                }
            }
        };
        let seed_bytes: [u8; 32] = BASE64URL
            .decode(seed_text.trim())
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| {
                WalletError::Misconfigured("the mandate signing seed is not 32 bytes".into())
            })?;

        let mut nonce_bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = nonce_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mandate = Mandate {
            mandate_id: Uuid::new_v4(),
            key_id,
            organization_id,
            cap_minor,
            models,
            not_before,
            not_after,
            nonce,
            created_at: OffsetDateTime::now_utc(),
        };
        let blob = sign_mandate(&mandate, &seed_bytes);

        sqlx::query(
            "INSERT INTO oxsum.mandates \
             (mandate_id, key_id, organization_id, cap_minor, models, not_before, not_after, nonce) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(mandate.mandate_id)
        .bind(mandate.key_id)
        .bind(mandate.organization_id)
        .bind(mandate.cap_minor)
        .bind(
            mandate
                .models
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .unwrap_or_default(),
        )
        .bind(mandate.not_before)
        .bind(mandate.not_after)
        .bind(&mandate.nonce)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(Some(CreatedMandate {
            mandate,
            mandate_blob: blob,
        }))
    }

    /// Every mandate minted for a key the caller may see, newest first.
    /// Metadata only — the signed blob is not stored, because the delegate
    /// already carries it; the row keeps the fields it authorized.
    pub async fn list_mandates(
        &self,
        organization_id: Uuid,
        key_id: Uuid,
        scope: KeyScope,
    ) -> Result<Vec<Mandate>, WalletError> {
        let rows = match scope {
            KeyScope::Own(user_id) => {
                sqlx::query(
                    "SELECT m.mandate_id, m.key_id, m.organization_id, m.cap_minor, \
                            m.models, m.not_before, m.not_after, m.nonce, m.created_at \
                     FROM oxsum.mandates m \
                     JOIN oxsum.api_keys k ON k.key_id = m.key_id \
                     WHERE m.organization_id = $1 AND m.key_id = $2 AND k.created_by = $3 \
                     ORDER BY m.created_at DESC, m.mandate_id",
                )
                .bind(organization_id)
                .bind(key_id)
                .bind(user_id)
                .fetch_all(self.pool())
                .await?
            }
            KeyScope::Organization | KeyScope::All => {
                sqlx::query(
                    "SELECT mandate_id, key_id, organization_id, cap_minor, models, \
                            not_before, not_after, nonce, created_at \
                     FROM oxsum.mandates \
                     WHERE organization_id = $1 AND key_id = $2 \
                     ORDER BY created_at DESC, mandate_id",
                )
                .bind(organization_id)
                .bind(key_id)
                .fetch_all(self.pool())
                .await?
            }
        };
        rows.iter().map(mandate_from_row).collect()
    }

    /// The verifying half a mandate's signature checks against: the key's
    /// Ed25519 public key, derived from the stored seed. `None` when the key
    /// has never delegated — a mandate presented for such a key cannot verify.
    ///
    /// The seed never changes once minted, so the read needs no lock: a key's
    /// mandates are signed under one seed for the key's whole life.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]; a seed that does not open
    /// under `OXSUM_SECRET_KEY` is [`WalletError::Misconfigured`].
    pub async fn mandate_verifying_key(
        &self,
        key_id: &Uuid,
        secret: &crate::channels::SecretKey,
    ) -> Result<Option<VerifyingKey>, WalletError> {
        let seed_sealed: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT mandate_signing_seed FROM oxsum.api_keys WHERE key_id = $1")
                .bind(key_id)
                .fetch_optional(self.pool())
                .await?
                .flatten();
        let Some(seed_sealed) = seed_sealed else {
            return Ok(None);
        };
        let seed_text = secret.open(&String::from_utf8_lossy(&seed_sealed))?;
        let seed_bytes: [u8; 32] = BASE64URL
            .decode(seed_text.trim())
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| {
                WalletError::Misconfigured("the mandate signing seed is not 32 bytes".into())
            })?;
        Ok(Some(SigningKey::from_bytes(&seed_bytes).verifying_key()))
    }
}

/// Maps one `oxsum.mandates` row to [`Mandate`].
fn mandate_from_row(row: &sqlx::postgres::PgRow) -> Result<Mandate, WalletError> {
    let models: Option<serde_json::Value> = row.try_get("models")?;
    Ok(Mandate {
        mandate_id: row.try_get("mandate_id")?,
        key_id: row.try_get("key_id")?,
        organization_id: row.try_get("organization_id")?,
        cap_minor: row.try_get("cap_minor")?,
        models: models.and_then(|v| serde_json::from_value(v).ok()),
        not_before: row.try_get("not_before")?,
        not_after: row.try_get("not_after")?,
        nonce: row.try_get("nonce")?,
        created_at: row.try_get("created_at")?,
    })
}
