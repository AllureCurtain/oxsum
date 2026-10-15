use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use oxsum_core::{Consistency, HeadSigningKey, KeyPublication, Principal, SignedHead, signing_key};

use crate::AppState;
use crate::error::{ApiError, ApiQuery};
use crate::routes::{ApiResult, ok, wallet};

pub(crate) async fn proof(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(entry_id): Path<Uuid>,
) -> ApiResult<oxsum_core::ProofBundle> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    // The price binding rides the bundle only when the deployment signs heads;
    // `head_signing_key` would 503 the whole proof where `None` just omits the
    // field, so the optional key is derived here rather than through that helper.
    let key = state
        .config
        .head_signing_seed()
        .map(signing_key)
        .transpose()
        .map_err(|_| ApiError::Internal)?;
    match w
        .receipt_proof(oxsum_core::EntryId::from_uuid(entry_id), key.as_ref())
        .await?
    {
        Some(bundle) => ok(bundle),
        None => Err(ApiError::not_found()),
    }
}

/// A tree head as the API presents it: size plus the lowercase-hex root.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TreeHeadRes {
    size: u64,
    root: String,
}

impl From<oxsum_core::TreeHead> for TreeHeadRes {
    fn from(head: oxsum_core::TreeHead) -> Self {
        Self {
            size: head.size,
            root: head.root.to_hex(),
        }
    }
}

/// The operator's verifying key, as the API publishes it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerifyingKeyRes {
    key_name: String,
    public_key: String,
    key_hash: String,
}

impl VerifyingKeyRes {
    fn of(key: &KeyPublication) -> Self {
        use base64::Engine as _;
        Self {
            key_name: key.name.clone(),
            public_key: base64::engine::general_purpose::STANDARD.encode(key.public_key),
            key_hash: key.key_hash.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }
}

/// A signed tree head: the note text, the head it attests, and the key that signed it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SignedTreeHeadRes {
    note: String,
    origin: String,
    #[serde(flatten)]
    key: VerifyingKeyRes,
    size: u64,
    root: String,
}

impl From<SignedHead> for SignedTreeHeadRes {
    fn from(signed: SignedHead) -> Self {
        Self {
            note: signed.note,
            origin: signed.origin,
            key: VerifyingKeyRes::of(&signed.key),
            size: signed.head.size,
            root: signed.head.root.to_hex(),
        }
    }
}

/// A consistency proof between a held head and the current one, with the new head signed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsistencyRes {
    note: String,
    #[serde(flatten)]
    key: VerifyingKeyRes,
    old_head: TreeHeadRes,
    head: TreeHeadRes,
    proof: ConsistencyProofRes,
}

/// The latest externally anchored checkpoint: the operator-signed head the
/// `witness-checkpoints` pass last published plus every witness cosignature
/// returned for it (issue #231). `data` is `null` while nothing is anchored —
/// either because no pass ran yet or because `OXSUM_WITNESS_URLS` is empty.
pub(crate) async fn log_checkpoint(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Option<WitnessCheckpointRes>> {
    let checkpoint = state
        .db
        .witness_checkpoint(principal.organization().id)
        .await?;
    ok(checkpoint.map(WitnessCheckpointRes::from))
}

/// The anchored checkpoint as the API presents it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WitnessCheckpointRes {
    head_size: i64,
    /// The operator-signed C2SP note, byte for byte.
    note: String,
    /// One entry per witness that cosigned this head.
    cosignatures: Vec<WitnessCosignRes>,
    updated_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WitnessCosignRes {
    /// The witness endpoint as configured in `OXSUM_WITNESS_URLS`.
    witness_url: String,
    /// The cosigned C2SP note text returned by the witness.
    note: String,
    first_seen: String,
}

impl From<oxsum_core::WitnessCheckpoint> for WitnessCheckpointRes {
    fn from(c: oxsum_core::WitnessCheckpoint) -> Self {
        Self {
            head_size: c.head_size,
            note: c.note,
            cosignatures: c
                .cosignatures
                .into_iter()
                .map(|co| WitnessCosignRes {
                    witness_url: co.witness_url,
                    note: co.note,
                    first_seen: co.first_seen.to_string(),
                })
                .collect(),
            updated_at: c.updated_at.to_string(),
        }
    }
}

/// A consistency proof in doubleentry's order: the hashes that recompute the new root
/// from the old one.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsistencyProofRes {
    old_size: u64,
    new_size: u64,
    path: Vec<String>,
}

impl From<Consistency> for ConsistencyRes {
    fn from(c: Consistency) -> Self {
        Self {
            note: c.signed.note.clone(),
            key: VerifyingKeyRes::of(&c.signed.key),
            old_head: c.old_head.into(),
            head: c.signed.head.into(),
            proof: ConsistencyProofRes {
                old_size: c.proof.old_size,
                new_size: c.proof.new_size,
                path: c.proof.path.iter().map(|h| h.to_hex()).collect(),
            },
        }
    }
}

/// The operator's head-signing key for this request, or 503 when the deployment did not
/// configure one.
///
/// Built per request from the seed: an Ed25519 keypair derive is cheap, and this keeps the
/// key material in [`Config`]'s one place rather than threading a `SigningKey` — which is
/// deliberately not `Clone` — through `AppState`.
fn head_signing_key(state: &AppState) -> Result<HeadSigningKey, ApiError> {
    let seed = state.config.head_signing_seed().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "tree head signing is not configured for this deployment".into(),
        )
    })?;
    // The key name is a constant that satisfies the note rules; a failure here is not a
    // caller error.
    signing_key(seed).map_err(|_| ApiError::Internal)
}

/// The current tree head of the caller's ledger, signed by the operator.
pub(crate) async fn log_head(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<SignedTreeHeadRes> {
    let key = head_signing_key(&state)?;
    let w = wallet(&state.tenants, principal.organization()).await?;
    ok(w.signed_head(&key).await?.into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsistencyQuery {
    from: u64,
}

/// Prove the current head extends the head at `?from=`: the old head recomputed from the
/// log, the new head signed, and the proof between them.
pub(crate) async fn log_consistency(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(q): ApiQuery<ConsistencyQuery>,
) -> ApiResult<ConsistencyRes> {
    let key = head_signing_key(&state)?;
    let w = wallet(&state.tenants, principal.organization()).await?;
    ok(w.consistency(q.from, &key).await?.into())
}

/// A signed price checkpoint, as `GET /api/v1/log/price-head` serves it: the note
/// text, the price row it attests, and the key that signed it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SignedPriceHeadRes {
    note: String,
    origin: String,
    channel: String,
    model: String,
    version: i64,
    row_hash: String,
    #[serde(flatten)]
    key: VerifyingKeyRes,
}

impl From<oxsum_core::SignedPriceHead> for SignedPriceHeadRes {
    fn from(signed: oxsum_core::SignedPriceHead) -> Self {
        Self {
            note: signed.note,
            origin: signed.origin,
            channel: signed.channel,
            model: signed.model,
            version: signed.version,
            row_hash: signed.row_hash.to_hex(),
            key: VerifyingKeyRes::of(&signed.key),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PriceHeadQuery {
    channel: String,
    model: String,
    /// Absent → the latest version of the (channel, model); a bill that names an
    /// older one asks for it explicitly.
    version: Option<i64>,
}

/// The signed checkpoint of one channel-model price row — `version`'s exact row,
/// or the latest — under the operator's key (issue #204).
///
/// Signing is stateless and deterministic over (key, origin, version, row), so a
/// stored row signs identically whenever it is asked for: a bill under an older
/// version stays checkable after the catalog moves on, and no snapshot table
/// exists to drift.
pub(crate) async fn log_price_head(
    State(state): State<AppState>,
    Extension(_principal): Extension<Principal>,
    ApiQuery(q): ApiQuery<PriceHeadQuery>,
) -> ApiResult<SignedPriceHeadRes> {
    let key = head_signing_key(&state)?;
    let Some(row) = oxsum_core::price_row(state.db.pool(), &q.channel, &q.model, q.version).await?
    else {
        return Err(ApiError::not_found());
    };
    let canonical = oxsum_core::canonical_price_row(&q.channel, &row)?;
    ok(
        oxsum_core::sign_price_head(&key, &q.channel, &row, &canonical)
            .map_err(|_| ApiError::Internal)?
            .into(),
    )
}

/// The operator's tree-head verifying key. A public key, so no credential: fetching it from
/// the server is convenience, and a verifier must have chosen it through a channel the
/// operator does not control.
pub(crate) async fn log_key(State(state): State<AppState>) -> ApiResult<VerifyingKeyRes> {
    let key = head_signing_key(&state)?;
    ok(VerifyingKeyRes::of(&KeyPublication::of(&key)))
}

/// The entries query: `before` pages the log backwards; `limit` bounds the
/// page (1–100, default 100) — the same bounds `Wallet::entries_page` clamps.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntriesQuery {
    before: Option<u64>,
    limit: Option<i64>,
}

/// One ledger entry as the transaction log lists it (issue #234).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogEntryRes {
    index: u64,
    id: String,
    description: String,
    content_hash: String,
}

/// `GET /api/v1/entries` answers `{rows, nextCursor}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntriesRes {
    rows: Vec<LogEntryRes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<u64>,
}

/// Every entry the organization's append-only log carries, newest first — the
/// dashboard's transaction-log page's data (issue #234).
pub(crate) async fn entries(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(query): ApiQuery<EntriesQuery>,
) -> ApiResult<EntriesRes> {
    let limit = query.limit.unwrap_or(100);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::Validation("limit must be 1 to 100".into()));
    }
    let w = wallet(&state.tenants, principal.organization()).await?;
    let page = w.entries_page(query.before, limit as usize).await?;
    ok(EntriesRes {
        rows: page
            .rows
            .into_iter()
            .map(|entry| LogEntryRes {
                index: entry.index,
                id: entry.id,
                description: entry.description,
                content_hash: entry.content_hash,
            })
            .collect(),
        next_cursor: page.next_cursor,
    })
}
