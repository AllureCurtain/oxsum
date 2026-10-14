//! Operator-signed tree heads.
//!
//! Each tenant's ledger is its own append-only Merkle log. The operator signs the current
//! head of that log as a C2SP `signed-note` (a `tlog-checkpoint` body, see doubleentry's
//! `witness` module), so a user holding an old head can verify — the note signature, then a
//! consistency proof — that the head the server serves today was appended onto the one they
//! hold.
//!
//! The cryptography lives in doubleentry's witness module, used here as a dependency; this
//! module only decides *what* is signed and under which name. The server owns the key
//! material (see `OXSUM_HEAD_SIGNING_KEY` in docs/development.md). There is deliberately no
//! server-side [`Witness`](doubleentry::witness::Witness): the state machine is for
//! independent parties, and the operator witnessing its own log would prove nothing. What
//! makes these notes trustworthy is the wire format — any third party can cosign them later,
//! with software that is not this crate's.

use doubleentry::witness::signing::SigningKey;
use doubleentry::witness::{KeyName, NoteError, Origin, SignedTreeHead};
use doubleentry::{ConsistencyProof, Hash, TreeHead};

use crate::channels::ModelPrice;
use crate::error::WalletError;

pub use doubleentry::witness::signing::SigningKey as HeadSigningKey;

/// The key name the operator signs tree heads under.
///
/// Fixed and published (openapi.yaml): the name a verifier matches signature lines against.
/// It is the operator's log-signing identity, not the deployment's — two deployments that
/// share nothing must not share a seed, and the public key is what tells them apart.
pub const KEY_NAME: &str = "oxsum/tree-heads";

/// The origin of a tenant's ledger log: `oxsum/ledgers/<tenant_id>`.
///
/// One origin per Merkle log, because a witness keys its state on the origin and two logs
/// sharing one would read as each other's fork. Tenant ids are lowercase letters, digits
/// and underscores (`Wallet::open` validates), so they are always origin-safe; the length
/// cap is 40, far below the origin's 256.
///
/// # Errors
///
/// Returns [`NoteError`] when the origin is malformed — unreachable for validated tenant
/// ids, but the failure is reported rather than asserted.
pub fn origin_for(tenant_id: &str) -> Result<Origin, NoteError> {
    Origin::new(format!("oxsum/ledgers/{tenant_id}"))
}

/// Builds the operator's signing key from a 32-byte seed: the decoded
/// `OXSUM_HEAD_SIGNING_KEY`.
///
/// # Errors
///
/// Returns [`NoteError`] when the key name is invalid — unreachable for the constant, but
/// the failure is reported rather than asserted.
pub fn signing_key(seed: [u8; 32]) -> Result<HeadSigningKey, NoteError> {
    Ok(SigningKey::from_seed(KeyName::new(KEY_NAME)?, seed))
}

/// Reads the seed from its environment form: 32 bytes, base64.
///
/// Mirrors [`SecretKey`](crate::SecretKey)::parse: the messages name the variable, so a
/// deployment that mistyped it can fix it without reading the source.
///
/// # Errors
///
/// Names `OXSUM_HEAD_SIGNING_KEY` and what is wrong with it.
pub fn seed_from_base64(text: &str) -> Result<[u8; 32], String> {
    use base64::Engine as _;
    const ENGINE: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::STANDARD;
    let bytes = ENGINE
        .decode(text.trim())
        .map_err(|error| format!("OXSUM_HEAD_SIGNING_KEY is not base64: {error}"))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!(
            "OXSUM_HEAD_SIGNING_KEY must decode to 32 bytes, got {}",
            bytes.len()
        )
    })
}

/// The public half of the operator's key, as the API publishes it.
#[derive(Debug, Clone)]
pub struct KeyPublication {
    /// The name the operator signs under ([`KEY_NAME`]).
    pub name: String,
    /// The raw Ed25519 public key.
    pub public_key: [u8; 32],
    /// The selector the operator's note signatures carry on the wire: which signature line
    /// on a note is this key's.
    pub key_hash: [u8; 4],
}

impl KeyPublication {
    /// The public half of `key`, as an API response would carry it.
    #[must_use]
    pub fn of(key: &HeadSigningKey) -> Self {
        let verifying = key.verifying_key();
        Self {
            name: verifying.name().as_str().to_owned(),
            public_key: verifying.to_bytes(),
            key_hash: verifying.note_key_hash(),
        }
    }
}

/// A tenant's current head, signed by the operator: what `GET /api/v1/log/head` serves.
#[derive(Debug, Clone)]
pub struct SignedHead {
    /// The complete C2SP signed-note text: body, blank line, signature lines. This text is
    /// what the signature covers, byte for byte — a verifier parses this, not a re-render.
    pub note: String,
    /// Which log this head belongs to.
    pub origin: String,
    /// The head itself.
    pub head: TreeHead,
    /// The key that signed it, as the API publishes it.
    pub key: KeyPublication,
}

/// Signs `head` for `origin`: builds the note, signs its body, renders the text.
///
/// # Errors
///
/// Returns [`NoteError`] when the key name is invalid — unreachable for the constant.
pub fn sign_head(
    key: &HeadSigningKey,
    origin: &Origin,
    head: TreeHead,
) -> Result<SignedHead, NoteError> {
    let mut note = SignedTreeHead::new(origin.clone(), head);
    // The body is rendered from the note that will be published, never reconstructed: a
    // signature covers those bytes exactly.
    let body = note.body();
    note.add_signature(key.sign(&body));
    Ok(SignedHead {
        note: note.to_note(),
        origin: origin.as_str().to_owned(),
        head,
        key: KeyPublication::of(key),
    })
}

/// A consistency proof between two heads of one log, with the new head signed: what
/// `GET /api/v1/log/consistency` serves.
#[derive(Debug, Clone)]
pub struct Consistency {
    /// The new (current) head, signed by the operator.
    pub signed: SignedHead,
    /// The old head at the requested size, recomputed from the log.
    pub old_head: TreeHead,
    /// The proof that the log at the new head extends the log at the old head.
    pub proof: ConsistencyProof,
}

// ── signed price heads (issue #204) ────────────────────────────────────────────
//
// A bill's settlement description names exactly one `(channel, model,
// priceVersion)` — but the *authenticity* of that price rests only on the
// append-only `channel_prices` table. A forged current price would still verify.
// The operator closes the gap by signing a checkpoint per priced row, under the
// same Ed25519 key that signs tree heads: a verifier that trusts the key can
// confirm the price a bill names is the price the catalog actually held.

/// The domain separator the canonical price row hashes under.
///
/// Mirrors `oxsum/idempotency/v1` (idempotency.rs): a tagged-BLAKE3 domain so the
/// row hash cannot collide with any other digest in the system, and versioned so
/// the canonical form can evolve without reinterpreting old notes.
pub const PRICE_ROW_DOMAIN: &[u8] = b"oxsum/price-catalog/v1";

/// The origin a channel's price catalog signs under: `oxsum/price-catalog/<channel>`.
///
/// One origin per channel — not per `(channel, model)` — because a `channel` is the
/// unit an operator names and a witness keys on origins; two models of one channel
/// are two checkpoints of one catalog, not two catalogs. Channel names are letters,
/// digits, dashes, underscores and dots (`channel_name`), so they are always
/// origin-safe — unlike *model* names, which may carry spaces and so stay inside the
/// hashed row bytes rather than the origin line.
///
/// # Errors
///
/// Returns [`NoteError`] when the origin is malformed — unreachable for a validated
/// channel name, but reported rather than asserted.
pub fn price_origin_for(channel: &str) -> Result<Origin, NoteError> {
    Origin::new(format!("oxsum/price-catalog/{channel}"))
}

/// The canonical JSON bytes a price row hashes into the signed checkpoint.
///
/// The body signs `{v, channel, model, version, weight, price}` as compact JSON
/// through [`PriceRowV1`]: channel/model/version inside the hash so a note for one
/// row cannot be replayed as another's; `weight` inside because it is versioned
/// with the price (issue #168) and part of what the turn routed under. `created_at`
/// and `channel_id` are deliberately out — the row's identity is its content, and a
/// timestamp would only force time serde into the canonical form. The `v` field
/// future-proofs the shape: a `PriceRowV2` would hash under a new domain.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PriceRowV1<'a> {
    v: i64,
    channel: &'a str,
    model: &'a str,
    version: i64,
    weight: i64,
    price: &'a crate::billing::Price,
}

/// Renders the canonical row bytes for `channel`'s `price` record.
///
/// `serde_json` emits struct fields in declaration order, so the bytes are
/// deterministic — the bundle carries this exact string, and the verifier hashes
/// it verbatim rather than re-serializing.
///
/// # Errors
///
/// Serialization of these fields cannot fail; the `Result` keeps the caller honest.
pub fn canonical_price_row(channel: &str, price: &ModelPrice) -> Result<String, WalletError> {
    serde_json::to_string(&PriceRowV1 {
        v: 1,
        channel,
        model: &price.model,
        version: price.version,
        weight: price.weight,
        price: &price.price,
    })
    .map_err(|error| WalletError::InvalidInput(format!("canonical price row: {error}")))
}

/// The 32-byte hash of the canonical row: what the signed checkpoint's root carries.
#[must_use]
pub fn price_row_hash(canonical_row: &str) -> Hash {
    Hash::digest(PRICE_ROW_DOMAIN, canonical_row.as_bytes())
}

/// A channel-model-version price checkpoint, signed by the operator: what
/// `GET /api/v1/log/price-head` serves.
///
/// The note reuses the `tlog-checkpoint` body — origin, size, root — because a
/// versioned price row *is* a checkpoint: `size` carries `priceVersion` (monotone,
/// append-only like a tree size) and `root` carries the row hash. `sign_head`
/// renders it, so nothing here is a new format.
#[derive(Debug, Clone)]
pub struct SignedPriceHead {
    /// The complete C2SP signed-note text: body, blank line, signature lines.
    pub note: String,
    /// Which catalog this checkpoint belongs to (`oxsum/price-catalog/<channel>`).
    pub origin: String,
    /// The channel the price belongs to.
    pub channel: String,
    /// The model the price prices.
    pub model: String,
    /// The price version the checkpoint attests.
    pub version: i64,
    /// The canonical-row hash the note's root carries.
    pub row_hash: Hash,
    /// The key that signed it, as the API publishes it.
    pub key: KeyPublication,
}

/// Signs a channel's price row as a checkpoint note.
///
/// `canonical_row` is the exact bytes the note's root commits to — pass the output
/// of [`canonical_price_row`] so the served hash and the served row agree.
///
/// # Errors
///
/// Returns [`NoteError`] when the key name or origin is invalid — unreachable for
/// the constants, but reported rather than asserted.
pub fn sign_price_head(
    key: &HeadSigningKey,
    channel: &str,
    price: &ModelPrice,
    canonical_row: &str,
) -> Result<SignedPriceHead, NoteError> {
    let origin = price_origin_for(channel)?;
    let row_hash = price_row_hash(canonical_row);
    // A priced row is a checkpoint: `size` is the version, `root` the row hash.
    let signed = sign_head(
        key,
        &origin,
        TreeHead {
            size: price.version as u64,
            root: row_hash,
        },
    )?;
    Ok(SignedPriceHead {
        note: signed.note,
        origin: signed.origin,
        channel: channel.to_owned(),
        model: price.model.clone(),
        version: price.version,
        row_hash,
        key: signed.key,
    })
}

// ── statement anchors (issue #221) ────────────────────────────────────────────
//
// A finalized statement names the log window it covers
// (`log_from_index`/`log_to_index`), and each covered entry proves its own
// inclusion — but nothing committed *what the log contained* between those
// bounds. A forged or truncated window would still verify. The anchor closes
// the gap: finalize appends a marker entry whose document hash is the window
// digest over the covered entries' content hashes in log order, and whose
// description commits the rollup digest over the statement's lines and totals.
// A verifier recomputes both digests and checks the anchor's inclusion proof —
// an entry dropped from or inserted into the window changes the digest the
// anchor carries.

/// The domain separator the statement-window digest folds under.
///
/// One digest over the ordered content hashes of every covered entry —
/// `oxsum/statement-window/v1` so it cannot collide with any other digest in
/// the system, and versioned so the fold can evolve without reinterpreting
/// anchored statements.
pub const STATEMENT_WINDOW_DOMAIN: &[u8] = b"oxsum/statement-window/v1";

/// The domain separator the rollup digest hashes its canonical form under.
pub const STATEMENT_ROLLUP_DOMAIN: &[u8] = b"oxsum/statement-rollup/v1";

/// The document id the anchor entry's [`doubleentry::DocumentRef`] carries: the
/// statement's identity in its own system, bound to the window digest.
pub const STATEMENT_ANCHOR_DOCUMENT: &str = "oxsum/statement-anchor/v1";

/// The window digest: one fold over `hashes` in log order.
///
/// Each covered entry's `content_hash` is appended to the payload verbatim —
/// 32 raw bytes each — and the fold digests the whole under
/// [`STATEMENT_WINDOW_DOMAIN`]. Order is load-bearing: the same set in a
/// different order folds to a different digest, which is what lets the anchor
/// pin the window's contents *and* their arrangement. An empty window folds
/// the empty payload — a statement with no usage anchors that fact too.
#[must_use]
pub fn statement_window_hash(hashes: &[Hash]) -> Hash {
    let mut payload = Vec::with_capacity(hashes.len() * Hash::LEN);
    for hash in hashes {
        payload.extend_from_slice(hash.as_bytes());
    }
    Hash::digest(STATEMENT_WINDOW_DOMAIN, &payload)
}

/// The canonical JSON bytes the rollup digest commits to.
///
/// The anchor's description carries `{v, period, logFromIndex, logToIndex,
/// entryCount, totalMinor, creditDrawnMinor, windowHash, lines: [...]}` as
/// compact JSON through this struct — the document the rollup digest names.
/// `serde_json` emits struct fields in declaration order, so the bytes are
/// deterministic; the verifier hashes them verbatim rather than re-serializing.
/// The `v` field future-proofs the shape: a `StatementRollupV2` would hash
/// under a new domain.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementRollupV1<'a> {
    /// Canonical-form version.
    pub v: i64,
    /// The billed month, `YYYY-MM`.
    pub period: &'a str,
    /// The first covered entry's log index, if the window is non-empty.
    pub log_from_index: Option<i64>,
    /// The last covered entry's log index, if the window is non-empty.
    pub log_to_index: Option<i64>,
    /// Usage rows the statement itemizes.
    pub entry_count: i64,
    /// The period's settled usage charges.
    pub total_minor: i64,
    /// The part of the charges the credit line carried.
    pub credit_drawn_minor: i64,
    /// The window digest the anchor's document hash carries — kept inside the
    /// rollup so the two digests are bound to each other, not just to the entry.
    pub window_hash: &'a str,
    /// The statement's itemized lines, ordered as persisted.
    pub lines: &'a [StatementRollupLine<'a>],
}

/// One line of the canonical rollup: a `StatementLine` borrowed for hashing.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementRollupLine<'a> {
    /// The channel the usage came through.
    pub channel: &'a str,
    /// The model that priced it.
    pub model: &'a str,
    /// Settled requests the line aggregates.
    pub turns: i64,
    /// Input tokens across those turns.
    pub input_tokens: i64,
    /// Output tokens across those turns.
    pub output_tokens: i64,
    /// Cached-input tokens across those turns.
    pub cached_tokens: i64,
    /// Reasoning tokens across those turns.
    pub reasoning_tokens: i64,
    /// What the line's usage was charged.
    pub amount_minor: i64,
}

/// Renders the canonical rollup bytes.
///
/// # Errors
///
/// Serialization of these fields cannot fail; the `Result` keeps the caller honest.
pub fn canonical_statement_rollup(rollup: &StatementRollupV1<'_>) -> Result<String, WalletError> {
    serde_json::to_string(rollup)
        .map_err(|error| WalletError::InvalidInput(format!("canonical statement rollup: {error}")))
}

/// The 32-byte rollup digest: what the anchor entry's description commits.
#[must_use]
pub fn statement_rollup_hash(canonical_rollup: &str) -> Hash {
    Hash::digest(STATEMENT_ROLLUP_DOMAIN, canonical_rollup.as_bytes())
}

// ── external witness anchoring (issue #231) ───────────────────────────────────
//
// The notes above are signed by the operator — they attest *a* history the
// operator claims. External anchoring is what makes the claim testable by a
// third party: a scheduled pass hands each organization's signed head to the
// witnesses `OXSUM_WITNESS_URLS` names (C2SP `tlog-witness` `/add-checkpoint`),
// and the cosigned notes they answer land here. The `Witness` state machine —
// trust, consistency proofs, fork refusal — lives on the witnesses' side; this
// crate only publishes.
//
// `witness_checkpoints` keeps one row per organization: the head the last pass
// published. `witness_cosignatures` is append-only — every cosigned note ever
// returned — so a later head's trail never rewrites an earlier one's, and
// `GET /api/v1/log/checkpoint` serves the anchored head with its vouchers.

/// The checkpoint the last `witness-checkpoints` pass anchored for an
/// organization: our signed head plus every witness's cosigned note for it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WitnessCheckpoint {
    /// The tree size the stored note attests.
    pub head_size: i64,
    /// The operator-signed C2SP note covering the head, byte for byte.
    pub note: String,
    /// The cosigned notes the witnesses returned for this head, oldest first.
    pub cosignatures: Vec<WitnessCosign>,
    /// When the pass last anchored the checkpoint.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

/// One witness's cosigned note for a checkpoint — its signature line is the
/// part a third party verifies.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WitnessCosign {
    /// The witness endpoint that answered, as `OXSUM_WITNESS_URLS` names it.
    pub witness_url: String,
    /// The cosigned C2SP note text.
    pub note: String,
    /// When this witness first vouched for the head.
    #[serde(with = "time::serde::rfc3339")]
    pub first_seen: time::OffsetDateTime,
}

impl crate::Db {
    /// The pass's own read of the checkpoint row — `(head size, note)` —
    /// without the cosignature join the public surface pays for.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn witness_checkpoint_state(
        &self,
        organization: uuid::Uuid,
    ) -> Result<Option<(i64, String)>, WalletError> {
        use sqlx::Row;
        sqlx::query(
            "SELECT head_size, note FROM oxsum.witness_checkpoints \
             WHERE organization_id = $1",
        )
        .bind(organization)
        .fetch_optional(self.pool())
        .await?
        .map(|row| {
            Ok((
                row.try_get::<i64, _>("head_size")?,
                row.try_get::<String, _>("note")?,
            ))
        })
        .transpose()
    }

    /// The anchored checkpoint of one organization — the stored note plus the
    /// cosignatures for *its* head — or `None` before the pass's first anchor.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn witness_checkpoint(
        &self,
        organization: uuid::Uuid,
    ) -> Result<Option<WitnessCheckpoint>, WalletError> {
        use sqlx::Row;
        let Some(row) = sqlx::query(
            "SELECT head_size, note, updated_at FROM oxsum.witness_checkpoints \
             WHERE organization_id = $1",
        )
        .bind(organization)
        .fetch_optional(self.pool())
        .await?
        else {
            return Ok(None);
        };
        let head_size: i64 = row.try_get("head_size")?;
        let cosignatures = sqlx::query(
            "SELECT witness_url, note, first_seen FROM oxsum.witness_cosignatures \
             WHERE organization_id = $1 AND head_size = $2 \
             ORDER BY first_seen, witness_url",
        )
        .bind(organization)
        .bind(head_size)
        .fetch_all(self.pool())
        .await?
        .iter()
        .map(|row| {
            Ok(WitnessCosign {
                witness_url: row.try_get("witness_url")?,
                note: row.try_get("note")?,
                first_seen: row.try_get("first_seen")?,
            })
        })
        .collect::<Result<Vec<_>, WalletError>>()?;
        Ok(Some(WitnessCheckpoint {
            head_size,
            note: row.try_get("note")?,
            cosignatures,
            updated_at: row.try_get("updated_at")?,
        }))
    }

    /// Records the anchored checkpoint and one witness's cosigned note,
    /// transactionally with the caller — both land or neither does. The
    /// checkpoint upsert is the pass's progress marker; the cosignature's
    /// (organization, witness, head size) key makes a re-delivered note
    /// idempotent, and the trail is append-only across heads.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_witness_checkpoint(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization: uuid::Uuid,
        head_size: i64,
        note: &str,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.witness_checkpoints (organization_id, head_size, note) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (organization_id) DO UPDATE \
                 SET head_size = EXCLUDED.head_size, note = EXCLUDED.note, \
                     updated_at = now()",
        )
        .bind(organization)
        .bind(head_size)
        .bind(note)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Appends a witness's cosigned note for a checkpoint head — `ON CONFLICT DO
    /// NOTHING`, so a re-delivered note records once.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_witness_cosignature(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization: uuid::Uuid,
        witness_url: &str,
        head_size: i64,
        note: &str,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.witness_cosignatures \
             (organization_id, witness_url, head_size, note) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(organization)
        .bind(witness_url)
        .bind(head_size)
        .bind(note)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // The tests may expect: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{NoteSignature, SignedTreeHead};
    use doubleentry::{Hash, MerkleLog};

    use super::*;

    fn seed() -> [u8; 32] {
        [7; 32]
    }

    fn log_of(n: u64) -> MerkleLog {
        let mut log = MerkleLog::new();
        for i in 0..n {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&i.to_le_bytes());
            log.append(Hash::from_bytes(bytes));
        }
        log
    }

    #[test]
    fn the_key_name_satisfies_the_note_rules() {
        assert!(KeyName::new(KEY_NAME).is_ok());
    }

    #[test]
    fn an_origin_names_the_tenant_ledger() {
        let origin = origin_for("acme_42").expect("valid tenant id");
        assert_eq!(origin.as_str(), "oxsum/ledgers/acme_42");
    }

    #[test]
    fn a_signed_head_verifies_against_the_published_key() {
        let key = signing_key(seed()).expect("constant key name");
        let origin = origin_for("acme").expect("valid");
        let log = log_of(8);
        let signed = sign_head(&key, &origin, log.head()).expect("signs");

        let parsed = SignedTreeHead::parse(&signed.note).expect("parses");
        assert_eq!(parsed.head, log.head());
        assert_eq!(parsed.origin, origin);
        assert!(parsed.extensions.is_empty());

        // The published key checks the published note: the whole point.
        let public = signed.key.public_key;
        let verifying =
            VerifyingKey::from_bytes(KeyName::new(&signed.key.name).expect("valid"), public)
                .expect("a real public key");
        assert_eq!(verifying.note_key_hash(), signed.key.key_hash);
        let signature: &NoteSignature = parsed
            .signatures
            .iter()
            .find(|s| s.key_hash == signed.key.key_hash)
            .expect("the operator's signature line is on the note");
        assert!(verifying.verify(&parsed.body(), signature));
    }

    #[test]
    fn a_signature_does_not_survive_a_changed_root() {
        let key = signing_key(seed()).expect("constant key name");
        let origin = origin_for("acme").expect("valid");
        let log = log_of(8);
        let signed = sign_head(&key, &origin, log.head()).expect("signs");

        let mut tampered = SignedTreeHead::parse(&signed.note).expect("parses");
        tampered.head.root = Hash::from_bytes([0xaa; 32]);
        let verifying = VerifyingKey::from_bytes(
            KeyName::new(&signed.key.name).expect("valid"),
            signed.key.public_key,
        )
        .expect("a real public key");
        let signature = tampered
            .signatures
            .iter()
            .find(|s| s.key_hash == signed.key.key_hash)
            .expect("the signature line");
        assert!(
            !verifying.verify(&tampered.body(), signature),
            "the signature covers the body byte for byte"
        );
    }

    // ── signed price heads (issue #204) ─────────────────────────────────────

    /// A price record to render: the fields the canonical row commits to.
    fn priced(model: &str, version: i64) -> ModelPrice {
        ModelPrice {
            model: model.to_owned(),
            version,
            weight: 100,
            price: crate::billing::Price {
                input_price_per_million: 1_000_000,
                output_price_per_million: 2_000_000,
                max_output_tokens: 8_000,
                cache_read_price_per_million: Some(100_000),
                cache_write_5m_price_per_million: None,
                cache_write_1h_price_per_million: None,
                reasoning_price_per_million: Some(8_000_000),
                cost_per_request: Some(100),
                upstream: None,
                mode: Default::default(),
                rules: Vec::new(),
            },
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_price_origin_names_the_channel_catalog() {
        let origin = price_origin_for("deepseek.prod-1").expect("valid channel name");
        assert_eq!(origin.as_str(), "oxsum/price-catalog/deepseek.prod-1");
    }

    /// The canonical row is pinned down to the byte: a field renamed, reordered
    /// or spelled differently would mint a different root for the same row and
    /// every bundle already issued would fail `verify_price_head`.
    #[test]
    fn the_canonical_price_row_is_byte_stable() {
        let row = canonical_price_row("deepseek", &priced("ds-chat", 7)).expect("serializes");
        assert_eq!(
            row,
            r#"{"v":1,"channel":"deepseek","model":"ds-chat","version":7,"weight":100,"price":{"inputPricePerMillion":1000000,"outputPricePerMillion":2000000,"maxOutputTokens":8000,"cacheReadPricePerMillion":100000,"reasoningPricePerMillion":8000000,"costPerRequest":100,"mode":"chat"}}"#
        );
    }

    /// A signed price head *is* a signed tree head: the note body names the
    /// catalog origin, the version rides in `size`, the row hash in `root`, and
    /// the operator's key checks out against the exact bytes.
    #[test]
    fn a_signed_price_head_verifies_against_the_published_key() {
        let key = signing_key(seed()).expect("constant key name");
        let price = priced("ds-chat", 7);
        let row = canonical_price_row("deepseek", &price).expect("serializes");
        let signed = sign_price_head(&key, "deepseek", &price, &row).expect("signs");

        let parsed = SignedTreeHead::parse(&signed.note).expect("parses");
        assert_eq!(parsed.origin.as_str(), "oxsum/price-catalog/deepseek");
        assert_eq!(parsed.head.size, 7, "the version rides in size");
        assert_eq!(parsed.head.root, signed.row_hash);
        assert_eq!(parsed.head.root, price_row_hash(&row));

        let verifying = VerifyingKey::from_bytes(
            KeyName::new(&signed.key.name).expect("valid"),
            signed.key.public_key,
        )
        .expect("a real public key");
        let signature = parsed
            .signatures
            .iter()
            .find(|s| s.key_hash == signed.key.key_hash)
            .expect("the operator's signature line is on the note");
        assert!(verifying.verify(&parsed.body(), signature));
    }

    /// Signing is stateless and deterministic: re-signing the same stored row
    /// later — after newer versions exist — renders the identical note, which
    /// is what keeps `?version=1` verifiable forever.
    #[test]
    fn re_signing_a_stored_row_renders_the_same_note() {
        let key = signing_key(seed()).expect("constant key name");
        let price = priced("ds-chat", 7);
        let row = canonical_price_row("deepseek", &price).expect("serializes");
        let first = sign_price_head(&key, "deepseek", &price, &row).expect("signs");
        let second = sign_price_head(&key, "deepseek", &price, &row).expect("signs");
        assert_eq!(first.note, second.note);
        // A different version is a different checkpoint.
        let newer = sign_price_head(&key, "deepseek", &priced("ds-chat", 8), &row).expect("signs");
        assert_ne!(first.note, newer.note);
    }

    // ── statement anchors (issue #221) ──────────────────────────────────────

    /// The window fold is positional: the same two content hashes in a
    /// different order digest differently, so a reordered window cannot pass.
    #[test]
    fn the_window_fold_binds_log_order() {
        let first = Hash::digest(b"test", b"a");
        let second = Hash::digest(b"test", b"b");
        let mut joined = Vec::from(first.as_bytes().as_slice());
        joined.extend_from_slice(second.as_bytes());
        assert_eq!(
            statement_window_hash(&[first, second]),
            Hash::digest(STATEMENT_WINDOW_DOMAIN, &joined),
        );
        assert_ne!(
            statement_window_hash(&[first, second]),
            statement_window_hash(&[second, first]),
        );
        // An empty window still folds — a statement over no entries commits
        // the digest of nothing, which is a real, checkable value.
        assert_eq!(
            statement_window_hash(&[]),
            Hash::digest(STATEMENT_WINDOW_DOMAIN, b""),
        );
    }

    /// The canonical rollup is pinned down to the byte: a field renamed,
    /// reordered or spelled differently would mint a different digest for the
    /// same statement and every anchor already issued would fail
    /// `verify_statement_anchor`.
    #[test]
    fn the_canonical_statement_rollup_is_byte_stable() {
        let lines = [StatementRollupLine {
            channel: "anthropic",
            model: "claude",
            turns: 3,
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: 10,
            reasoning_tokens: 5,
            amount_minor: 2_000_000,
        }];
        let window_hex = "aa".repeat(32);
        let rollup = StatementRollupV1 {
            v: 1,
            period: "2026-09",
            log_from_index: Some(3),
            log_to_index: Some(8),
            entry_count: 3,
            total_minor: 2_000_000,
            credit_drawn_minor: 1_500_000,
            window_hash: &window_hex,
            lines: &lines,
        };
        let canonical = canonical_statement_rollup(&rollup).expect("serializes");
        assert_eq!(
            canonical,
            format!(
                r#"{{"v":1,"period":"2026-09","logFromIndex":3,"logToIndex":8,"entryCount":3,"totalMinor":2000000,"creditDrawnMinor":1500000,"windowHash":"{}","lines":[{{"channel":"anthropic","model":"claude","turns":3,"inputTokens":100,"outputTokens":50,"cachedTokens":10,"reasoningTokens":5,"amountMinor":2000000}}]}}"#,
                "aa".repeat(32),
            ),
        );
    }

    /// A null window is a real shape too: a statement over no settled entries
    /// serializes `logFromIndex`/`logToIndex` as null, not as absent fields —
    /// the verifier reads the same wire.
    #[test]
    fn the_rollup_keeps_an_empty_window_explicit() {
        let window_hex = "bb".repeat(32);
        let rollup = StatementRollupV1 {
            v: 1,
            period: "2026-09",
            log_from_index: None,
            log_to_index: None,
            entry_count: 0,
            total_minor: 0,
            credit_drawn_minor: 0,
            window_hash: &window_hex,
            lines: &[],
        };
        let canonical = canonical_statement_rollup(&rollup).expect("serializes");
        assert!(canonical.contains(r#""logFromIndex":null"#));
        assert!(canonical.contains(r#""logToIndex":null"#));
        assert!(canonical.ends_with(r#""lines":[]}"#));
    }
}
