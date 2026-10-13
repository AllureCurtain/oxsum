//! Bill verification, shared by the server and the browser.
//!
//! Given a proof bundle (the entry source, an inclusion proof, and the tree head
//! as of the proof) plus the content hash the user recorded with the bill, this
//! crate recomputes the hash from the source and links it to the head. Everything
//! here is deterministic computation with no I/O, so it compiles to
//! `wasm32-unknown-unknown` and runs inside the verification page: the same
//! function body the server runs, with no round-trip.
//!
//! The money scale lives here because the entry encoding — and therefore the
//! content hash — depends on it. The writer (`oxsum-core`) and every verifier
//! must agree on it, and one definition leaves no room for drift.

use doubleentry::{Balanced, Draft, Entry, InclusionProof};

mod charge;

pub use charge::{ChargeCheck, verify_charge};
pub use doubleentry::{ConsistencyProof, Hash, TreeHead};

/// Money precision: 6 decimal places; 1 credit = 1_000_000 minor, fine enough
/// for per-token pricing. Re-exported by `oxsum-core`.
pub const SCALE: u8 = 6;

/// The bill proof bundle handed to the user: the entry source, an inclusion proof,
/// and the tree head as of the proof.
///
/// `price` binds the bill's `(channel, model, priceVersion)` to a signed checkpoint
/// of the price row itself (issue #204). It is `Option` because a bundle prices
/// only settlements — holds, top-ups and adjustments carry none — and because a
/// deployment with no `OXSUM_HEAD_SIGNING_KEY` serves the proof without it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProofBundle {
    pub entry: Entry<Balanced, SCALE>,
    pub head: TreeHead,
    pub proof: InclusionProof,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<PriceLink>,
}

/// The signed link between a bill's price and the operator's catalog.
///
/// Everything a verifier needs to confirm the price a settlement names is the
/// price the catalog actually held: the canonical row bytes the checkpoint's root
/// commits to, the checkpoint note itself, and the published key that signed it.
/// Self-contained — the verifier never asks the server for a second opinion.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceLink {
    /// The channel the price belongs to.
    pub channel: String,
    /// The model the price prices.
    pub model: String,
    /// The price version the checkpoint attests.
    pub version: i64,
    /// The canonical JSON bytes the checkpoint root commits to — the exact string
    /// the server hashed, hashed verbatim by the verifier.
    pub row: String,
    /// The C2SP signed-note text attesting `row`'s hash under `version`.
    pub note: String,
    /// The key name the operator signs under (`oxsum/tree-heads`).
    pub key_name: String,
    /// The operator's Ed25519 verifying key, lowercase hex of 32 bytes.
    pub public_key: String,
    /// The 4-byte selector this key's note signatures carry, lowercase hex.
    pub key_hash: String,
}

/// Verifies a bill on the client, without trusting the server.
///
/// 1. Recomputes the content hash from the entry source; it must equal the hash the
///    user recorded with the bill. The hash the server returns this time does not count.
/// 2. Links that hash to the tree head with the inclusion proof.
///
/// Whether the tree head itself is trustworthy is established separately, by the
/// operator's signed-note signature ([`verify_signed_head`]) and, for a head the holder
/// has archived, by a consistency proof that the new head extends it
/// ([`verify_consistency`]).
///
/// This function only uses the pure, I/O-free parts of doubleentry, so it compiles
/// to WASM and runs in the browser.
pub fn verify_bundle(json: &str, expected_hash: &Hash) -> Result<bool, serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct Wire {
        entry: Entry<Draft, SCALE>,
        head: TreeHead,
        proof: InclusionProof,
    }
    let wire: Wire = serde_json::from_str(json)?;
    // adopt_verified recomputes the hash from the source; a single flipped byte fails it.
    let Ok(entry) = wire.entry.adopt_verified(*expected_hash) else {
        return Ok(false);
    };
    Ok(wire.proof.verify(&entry.content_hash(), &wire.head))
}

/// What checking a signed tree head — or a consistency proof anchored by one — can
/// fail with. Each variant is one distinct trust failure, so a page can say what is
/// wrong instead of only that something is.
#[derive(Debug, PartialEq, Eq)]
pub enum HeadError {
    /// The note text, the published key, or a hex field could not be parsed.
    Malformed(String),
    /// The note names a different log than the caller expected: another tenant's head
    /// verifies as a signature but answers a different question.
    WrongOrigin,
    /// The note's attested head differs from the head the server served alongside it.
    HeadMismatch,
    /// The note carries no signature line under the published key.
    Unsigned,
    /// The published key rejects the note's signature.
    BadSignature,
    /// The consistency response's old head is not the archived head the proof was
    /// requested for: the proof would anchor a history the holder never saw.
    OldHeadMismatch,
    /// The consistency proof does not verify: the new head does not extend the old.
    Inconsistent,
}

impl std::fmt::Display for HeadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "the signed head could not be parsed ({what})"),
            Self::WrongOrigin => write!(f, "the signed head names a different ledger"),
            Self::HeadMismatch => {
                write!(
                    f,
                    "the signature attests a different head than the one served"
                )
            }
            Self::Unsigned => write!(f, "the head carries no signature from the published key"),
            Self::BadSignature => write!(f, "the head's signature does not verify"),
            Self::OldHeadMismatch => {
                write!(
                    f,
                    "the consistency proof does not start from the archived head"
                )
            }
            Self::Inconsistent => {
                write!(f, "the served head does not extend the archived head")
            }
        }
    }
}

impl std::error::Error for HeadError {}

/// The operator's published tree-head key, as a signed-head response carries it.
///
/// The wire spells the Ed25519 key and its selector as hex; both are decoded here so the
/// caller passes the response through untouched.
pub struct PublishedKey<'a> {
    /// The name the operator signs under (`oxsum/tree-heads`).
    pub key_name: &'a str,
    /// The Ed25519 public key, hex.
    pub public_key: &'a str,
    /// The note key hash: which signature line on the note is this key's, hex.
    pub key_hash: &'a str,
}

/// Verifies an operator-signed tree head.
///
/// Three checks, each guarding a different substitution: the note parses and names
/// `expected_origin` (not another tenant's log), the head it attests equals the `served`
/// head the caller will reason about (not a head only described alongside), and a
/// signature line under the published key verifies over the note body byte for byte.
///
/// # Errors
///
/// The first check that fails, as a [`HeadError`].
pub fn verify_signed_head(
    note: &str,
    served: &TreeHead,
    expected_origin: &str,
    key: &PublishedKey<'_>,
) -> Result<(), HeadError> {
    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{KeyName, SignedTreeHead};

    let parsed = SignedTreeHead::parse(note).map_err(|e| HeadError::Malformed(e.to_string()))?;
    if parsed.origin.as_str() != expected_origin {
        return Err(HeadError::WrongOrigin);
    }
    if parsed.head != *served {
        return Err(HeadError::HeadMismatch);
    }
    let selector = hex_bytes::<4>(key.key_hash)
        .map_err(|_| HeadError::Malformed("the published key hash is not hex".into()))?;
    let signature = parsed
        .signatures
        .iter()
        .find(|s| s.name.as_str() == key.key_name && s.key_hash == selector)
        .ok_or(HeadError::Unsigned)?;
    let public = hex_bytes::<32>(key.public_key)
        .map_err(|_| HeadError::Malformed("the published key is not hex".into()))?;
    let name = KeyName::new(key.key_name)
        .map_err(|_| HeadError::Malformed("the published key name is invalid".into()))?;
    let verifying = VerifyingKey::from_bytes(name, public)
        .map_err(|_| HeadError::Malformed("the published key is not a valid Ed25519 key".into()))?;
    // The body is what the signature covers; parsing kept it verbatim.
    match verifying.verify(&parsed.body(), signature) {
        true => Ok(()),
        false => Err(HeadError::BadSignature),
    }
}

/// Verifies a consistency response against a held head.
///
/// The order is the order of trust: the response's old head must equal the `held` head
/// the proof was requested for (a proof anchored anywhere else proves nothing about the
/// holder's archive), the proof must show the new head extends it, and the new head —
/// the only part the holder is asked to keep — must carry a valid operator signature.
///
/// # Errors
///
/// The first check that fails, as a [`HeadError`].
pub fn verify_consistency(
    held: &TreeHead,
    old_head: &TreeHead,
    proof: &ConsistencyProof,
    note: &str,
    new_head: &TreeHead,
    expected_origin: &str,
    key: &PublishedKey<'_>,
) -> Result<(), HeadError> {
    if old_head != held {
        return Err(HeadError::OldHeadMismatch);
    }
    if !proof.verify(old_head, new_head) {
        return Err(HeadError::Inconsistent);
    }
    verify_signed_head(note, new_head, expected_origin, key)
}

/// What checking a bill's price binding — the signed catalog checkpoint — can
/// fail with. Mirrors [`HeadError`]: each variant is one distinct trust failure,
/// so a page can say what is wrong instead of only that something is.
#[derive(Debug, PartialEq, Eq)]
pub enum PriceError {
    /// The `price` field, the note, or a hex field could not be parsed.
    Malformed(String),
    /// The note names a different catalog than the channel the bill priced under.
    WrongOrigin,
    /// The `price` field's `channel`, `model` or `version` differs from what the
    /// settlement description names: the checkpoint answers a different price.
    BindingMismatch,
    /// The canonical row's recomputed hash differs from the root the note attests:
    /// the price row was tampered with.
    HashMismatch,
    /// The note carries no signature line under the published key.
    Unsigned,
    /// The published key rejects the note's signature.
    BadSignature,
    /// The bundle carries no `price` field: a non-settlement entry, a bundle made
    /// before price binding shipped, or a deployment that signs no heads.
    Absent,
}

impl std::fmt::Display for PriceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => {
                write!(f, "the price binding could not be parsed ({what})")
            }
            Self::WrongOrigin => write!(f, "the checkpoint names a different catalog"),
            Self::BindingMismatch => {
                write!(f, "the checkpoint does not name the price this bill used")
            }
            Self::HashMismatch => {
                write!(f, "the price row does not match the signed checkpoint")
            }
            Self::Unsigned => {
                write!(
                    f,
                    "the checkpoint carries no signature from the published key"
                )
            }
            Self::BadSignature => write!(f, "the checkpoint's signature does not verify"),
            Self::Absent => write!(f, "the bundle carries no signed price binding"),
        }
    }
}

impl std::error::Error for PriceError {}

/// The domain the canonical price row hashes under — the same constant
/// `oxsum_core` signs with (`PRICE_ROW_DOMAIN`), duplicated on purpose: the
/// verifier pins the byte string so it cannot drift from what was signed.
const PRICE_ROW_DOMAIN: &[u8] = b"oxsum/price-catalog/v1";

/// The `(channel, model, priceVersion)` a settlement description names.
struct NamedPrice {
    channel: String,
    model: String,
    version: i64,
}

/// Reads the price identity out of a settlement description — the same light,
/// wire-only parse [`verify_charge`] makes, never importing `oxsum-core` types.
fn named_price(description: &str) -> Result<NamedPrice, PriceError> {
    let value: serde_json::Value = serde_json::from_str(description)
        .map_err(|e| PriceError::Malformed(format!("the settlement description: {e}")))?;
    let get_str = |field: &str| -> Result<String, PriceError> {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| PriceError::Malformed(format!("the description has no {field}")))
    };
    let version = value
        .get("priceVersion")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| PriceError::Malformed("the description has no priceVersion".into()))?;
    Ok(NamedPrice {
        channel: get_str("channel")?,
        model: get_str("model")?,
        version,
    })
}

/// Verifies a bundle's signed price binding (issue #204).
///
/// Takes the whole bundle JSON so it can read both halves itself — the `price`
/// link and the `entry.description` whose `(channel, model, priceVersion)` it must
/// name. Four checks, in trust order:
///
/// 1. The description parses and names a price — the same wire the content hash
///    already covered.
/// 2. The `price` link names exactly that `(channel, model, version)`, and its note
///    names the catalog `oxsum/price-catalog/<channel>` with `size` equal to the
///    version — not another row's checkpoint.
/// 3. Recompute [`Hash::digest`] of the canonical `row` bytes; it must equal the
///    root the note attests — the row the bill priced under is the row the
///    operator signed.
/// 4. A signature line under the published key verifies over the note body byte
///    for byte — the operator really signed that hash.
///
/// # Errors
///
/// The first check that fails, as a [`PriceError`].
pub fn verify_price_head(json: &str) -> Result<(), PriceError> {
    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{KeyName, SignedTreeHead};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Wire {
        entry: serde_json::Value,
        #[serde(default)]
        price: Option<PriceLink>,
    }
    let wire: Wire =
        serde_json::from_str(json).map_err(|e| PriceError::Malformed(e.to_string()))?;
    let description = wire
        .entry
        .get("description")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PriceError::Malformed("the bundle's entry has no description".into()))?;
    let named = named_price(description)?;
    let link = wire.price.ok_or(PriceError::Absent)?;

    // The link must name the very price the bill did.
    if link.channel != named.channel || link.model != named.model || link.version != named.version {
        return Err(PriceError::BindingMismatch);
    }

    // The note must checkpoint that same catalog and version.
    let parsed =
        SignedTreeHead::parse(&link.note).map_err(|e| PriceError::Malformed(e.to_string()))?;
    let expected_origin = format!("oxsum/price-catalog/{}", link.channel);
    if parsed.origin.as_str() != expected_origin {
        return Err(PriceError::WrongOrigin);
    }
    if parsed.head.size != link.version as u64 {
        return Err(PriceError::BindingMismatch);
    }

    // The canonical row must hash to what the note attests.
    let row_hash = Hash::digest(PRICE_ROW_DOMAIN, link.row.as_bytes());
    if row_hash != parsed.head.root {
        return Err(PriceError::HashMismatch);
    }

    // And the published key must have signed it.
    let selector = hex_bytes::<4>(&link.key_hash)
        .map_err(|_| PriceError::Malformed("the published key hash is not hex".into()))?;
    let signature = parsed
        .signatures
        .iter()
        .find(|s| s.name.as_str() == link.key_name && s.key_hash == selector)
        .ok_or(PriceError::Unsigned)?;
    let public = hex_bytes::<32>(&link.public_key)
        .map_err(|_| PriceError::Malformed("the published key is not hex".into()))?;
    let name = KeyName::new(&link.key_name)
        .map_err(|_| PriceError::Malformed("the published key name is invalid".into()))?;
    let verifying = VerifyingKey::from_bytes(name, public).map_err(|_| {
        PriceError::Malformed("the published key is not a valid Ed25519 key".into())
    })?;
    match verifying.verify(&parsed.body(), signature) {
        true => Ok(()),
        false => Err(PriceError::BadSignature),
    }
}

/// What checking a statement's log anchor — the marker entry committing the
/// statement's window and rollup digests — can fail with. Mirrors [`PriceError`]:
/// each variant is one distinct trust failure, so a page can say what is wrong
/// instead of only that something is.
#[derive(Debug, PartialEq, Eq)]
pub enum AnchorError {
    /// The anchor object, the note, a hex field, or the served window/rollup
    /// inputs could not be parsed.
    Malformed(String),
    /// The note names a different ledger than the tenant the statement belongs
    /// to: another tenant's anchor verifies as a signature but answers a
    /// different question.
    WrongOrigin,
    /// The note does not attest the head the verifier expected: the signed head
    /// at the position right after the anchor, where the marker's inclusion is
    /// already committed.
    HeadMismatch,
    /// The content hashes the window was rebuilt from fold to a different digest
    /// than the anchor claims: the statement's covered window was truncated,
    /// reordered, or had entries swapped.
    WindowMismatch,
    /// The canonical rollup the statement's lines and totals rebuild does not
    /// digest to what the anchor committed: the document's figures were altered.
    RollupMismatch,
    /// The note carries no signature line under the published key.
    Unsigned,
    /// The published key rejects the note's signature.
    BadSignature,
    /// The statement carries no anchor: drafted before anchors shipped, or a
    /// document the finalize path never issued.
    Absent,
}

impl std::fmt::Display for AnchorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "the statement anchor could not be parsed ({what})"),
            Self::WrongOrigin => write!(f, "the signed head names a different ledger"),
            Self::HeadMismatch => {
                write!(
                    f,
                    "the note does not attest the head right after the anchor"
                )
            }
            Self::WindowMismatch => {
                write!(
                    f,
                    "the covered entries' content hashes do not fold to the anchored window digest"
                )
            }
            Self::RollupMismatch => {
                write!(
                    f,
                    "the statement's lines and totals do not digest to the anchored rollup"
                )
            }
            Self::Unsigned => {
                write!(f, "the head carries no signature from the published key")
            }
            Self::BadSignature => write!(f, "the head's signature does not verify"),
            Self::Absent => write!(f, "the statement carries no log anchor"),
        }
    }
}

impl std::error::Error for AnchorError {}

/// The domain the statement-window digest folds under — the same constant
/// `oxsum_core` anchors with (`STATEMENT_WINDOW_DOMAIN`), duplicated on purpose:
/// the verifier pins the byte string so it cannot drift from what was anchored.
const STATEMENT_WINDOW_DOMAIN: &[u8] = b"oxsum/statement-window/v1";

/// The domain the canonical rollup hashes under — the same constant
/// `oxsum_core` anchors with (`STATEMENT_ROLLUP_DOMAIN`).
const STATEMENT_ROLLUP_DOMAIN: &[u8] = b"oxsum/statement-rollup/v1";

/// The canonical rollup, rebuilt by the verifier from the statement's fields
/// and lines — the same struct `oxsum_core` hashes, redeclared wire-only so the
/// verifier never imports the writer's crate.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StatementRollupV1<'a> {
    v: i64,
    period: &'a str,
    log_from_index: Option<i64>,
    log_to_index: Option<i64>,
    entry_count: i64,
    total_minor: i64,
    credit_drawn_minor: i64,
    window_hash: &'a str,
    lines: &'a [StatementRollupLine<'a>],
}

/// One line of the canonical rollup — a `StatementLine`'s fields in the same
/// declaration order `oxsum_core` serializes.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StatementRollupLine<'a> {
    channel: &'a str,
    model: &'a str,
    turns: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    reasoning_tokens: i64,
    amount_minor: i64,
}

/// The `anchor` object as `GET …/statements/{id}` serves it.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnchorWire {
    /// The marker's ledger id — carried on the wire for the caller's
    /// bookkeeping; the checks bind the digests and the head, not the id.
    #[allow(dead_code)]
    entry_id: String,
    log_index: i64,
    window_hash: String,
    rollup_hash: String,
    origin: String,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
}

/// One line of the statement detail's `lines` array.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineWire {
    channel: String,
    model: String,
    turns: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    reasoning_tokens: i64,
    amount_minor: i64,
}

/// The statement fields the anchor check binds.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatementWire {
    period: String,
    log_from_index: Option<i64>,
    log_to_index: Option<i64>,
    entry_count: i64,
    total_minor: i64,
    credit_drawn_minor: i64,
}

/// What [`verify_statement_anchor`] needs beyond the detail response itself:
/// the covered entries' content hashes, the head the anchor's position implies,
/// and where the check is pointed.
pub struct StatementAnchorCheck<'a> {
    /// The covered entries' `contentHash` hex strings in log order —
    /// `contentHash` per `GET /api/v1/entries/{id}/proof` or the dashboard's
    /// bill rows, ordered by `logIndex` across the statement's window. Fewer
    /// than the window's span means the fold cannot match, so a caller that
    /// could not list them all should not call.
    pub window_hashes: &'a [&'a str],
    /// The signed tree head at size `anchorLogIndex + 1` — the head whose log
    /// already contains the anchor. Fetch it from `GET /api/v1/log/head` (the
    /// current head, when the anchor is the latest entry) or recompute
    /// consistency to it; the verifier only checks the note attests *this* head.
    pub expected_head: &'a TreeHead,
    /// The tenant's ledger origin, `oxsum/ledgers/<tenant>` — the same string
    /// `GET /api/v1/log/head` answers.
    pub expected_origin: &'a str,
}

/// Verifies a statement's log anchor against the detail response (issue #221).
///
/// `json` is the detail body's JSON (`{statement, lines, anchor}`), and `check`
/// carries what the response cannot say: the covered entries' content hashes in
/// log order and the signed head right after the anchor. Five checks, in trust
/// order:
///
/// 1. The response parses and carries an `anchor` — an unanchored statement has
///    nothing to verify, which is `Absent`, not failure.
/// 2. The window fold: `window_hashes` recompute under
///    `oxsum/statement-window/v1` and must equal `windowHash` — a dropped,
///    inserted or reordered entry changes the digest the anchor recorded.
/// 3. The rollup: the statement's fields and lines re-serialize to the canonical
///    bytes and must digest under `oxsum/statement-rollup/v1` to `rollupHash` —
///    the figures the document shows are the figures the anchor committed.
/// 4. The signed head: a present `note` parses under `expected_origin` and
///    attests `expected_head`, which must be the head at exactly
///    `logIndex + 1` — the anchor's inclusion is proven by the head that
///    follows it. An unsigned deployment serves `note` as null, and the anchor
///    is verified to the two digests alone.
/// 5. The published key's signature verifies over the note body byte for byte —
///    the operator really signed that head.
///
/// # Errors
///
/// The first check that fails, as an [`AnchorError`].
pub fn verify_statement_anchor(
    json: &str,
    check: &StatementAnchorCheck<'_>,
) -> Result<(), AnchorError> {
    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{KeyName, SignedTreeHead};

    #[derive(serde::Deserialize)]
    struct Wire {
        statement: StatementWire,
        lines: Vec<LineWire>,
        #[serde(default)]
        anchor: Option<AnchorWire>,
    }
    let wire: Wire =
        serde_json::from_str(json).map_err(|e| AnchorError::Malformed(e.to_string()))?;
    let anchor = wire.anchor.ok_or(AnchorError::Absent)?;

    // The window fold: each served content hash decodes to 32 bytes and folds in
    // order; the digest must be the one the anchor recorded.
    let mut payload = Vec::with_capacity(check.window_hashes.len() * Hash::LEN);
    for hash in check.window_hashes {
        let bytes = hex_bytes::<32>(hash)
            .map_err(|_| AnchorError::Malformed("a window content hash is not hex".into()))?;
        payload.extend_from_slice(&bytes);
    }
    let folded = Hash::digest(STATEMENT_WINDOW_DOMAIN, &payload);
    if folded.to_hex() != anchor.window_hash {
        return Err(AnchorError::WindowMismatch);
    }

    // The rollup: the statement's figures and lines re-serialize to the canonical
    // bytes and digest to what the anchor committed.
    let lines: Vec<StatementRollupLine<'_>> = wire
        .lines
        .iter()
        .map(|line| StatementRollupLine {
            channel: &line.channel,
            model: &line.model,
            turns: line.turns,
            input_tokens: line.input_tokens,
            output_tokens: line.output_tokens,
            cached_tokens: line.cached_tokens,
            reasoning_tokens: line.reasoning_tokens,
            amount_minor: line.amount_minor,
        })
        .collect();
    let rollup = StatementRollupV1 {
        v: 1,
        period: &wire.statement.period,
        log_from_index: wire.statement.log_from_index,
        log_to_index: wire.statement.log_to_index,
        entry_count: wire.statement.entry_count,
        total_minor: wire.statement.total_minor,
        credit_drawn_minor: wire.statement.credit_drawn_minor,
        window_hash: &anchor.window_hash,
        lines: &lines,
    };
    let canonical = serde_json::to_string(&rollup)
        .map_err(|e| AnchorError::Malformed(format!("the canonical rollup: {e}")))?;
    if Hash::digest(STATEMENT_ROLLUP_DOMAIN, canonical.as_bytes()).to_hex() != anchor.rollup_hash {
        return Err(AnchorError::RollupMismatch);
    }

    // The signed head: the note must name this tenant's ledger and attest the
    // head right after the anchor — the marker's inclusion is committed there.
    // An unsigned deployment serves `note` as null, and the anchor is checked
    // to the digests alone — the signature layer simply does not apply.
    let Some(note) = anchor.note.as_deref() else {
        return Ok(());
    };
    let parsed = SignedTreeHead::parse(note).map_err(|e| AnchorError::Malformed(e.to_string()))?;
    if parsed.origin.as_str() != check.expected_origin || parsed.origin.as_str() != anchor.origin {
        return Err(AnchorError::WrongOrigin);
    }
    if parsed.head != *check.expected_head || parsed.head.size != anchor.log_index as u64 + 1 {
        return Err(AnchorError::HeadMismatch);
    }
    let key_name = anchor
        .key_name
        .as_deref()
        .ok_or_else(|| AnchorError::Malformed("the anchor carries no key name".into()))?;
    let key_hash = anchor
        .key_hash
        .as_deref()
        .ok_or_else(|| AnchorError::Malformed("the anchor carries no key hash".into()))?;
    let selector = hex_bytes::<4>(key_hash)
        .map_err(|_| AnchorError::Malformed("the published key hash is not hex".into()))?;
    let signature = parsed
        .signatures
        .iter()
        .find(|s| s.name.as_str() == key_name && s.key_hash == selector)
        .ok_or(AnchorError::Unsigned)?;
    let public_key = anchor
        .public_key
        .as_deref()
        .ok_or_else(|| AnchorError::Malformed("the anchor carries no public key".into()))?;
    let public = hex_bytes::<32>(public_key)
        .map_err(|_| AnchorError::Malformed("the published key is not hex".into()))?;
    let name = KeyName::new(key_name)
        .map_err(|_| AnchorError::Malformed("the published key name is invalid".into()))?;
    let verifying = VerifyingKey::from_bytes(name, public).map_err(|_| {
        AnchorError::Malformed("the published key is not a valid Ed25519 key".into())
    })?;
    match verifying.verify(&parsed.body(), signature) {
        true => Ok(()),
        false => Err(AnchorError::BadSignature),
    }
}

/// Decodes exactly `N` bytes of hex, the shape both key fields on the wire take.
fn hex_bytes<const N: usize>(hex: &str) -> Result<[u8; N], ()> {
    let bytes = hex.as_bytes();
    if bytes.len() != N * 2 {
        return Err(());
    }
    let mut out = [0u8; N];
    for (i, &[hi, lo]) in bytes.as_chunks::<2>().0.iter().enumerate() {
        let hi = (hi as char).to_digit(16).ok_or(())?;
        let lo = (lo as char).to_digit(16).ok_or(())?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A real bundle captured from a wallet ledger (one top-up of 10 credits),
    /// with the content hash the ledger recorded for it. Verification is pure
    /// over these bytes, so the fixture is stable.
    const BUNDLE: &str = r#"{"entry":{"id":"73bdf311-6f39-5a37-b742-b786445e854a","idempotency_key":"666978747572652d31","booking_date":[2026,274],"value_date":[2026,274],"description":"","postings":[{"account":1,"direction":"Debit","amount":"10.000000","currency":"USD","layer":"Settled","dimensions":{}},{"account":0,"direction":"Credit","amount":"10.000000","currency":"USD","layer":"Settled","dimensions":{}}],"kind":null,"provenance":{"actor":null,"source":null,"correlation":null},"document":null,"reverses":null,"original_booking_date":null},"head":{"size":1,"root":"43531c9c43c5a9e6ce51d6fa1d170b3d09971bbf97cf516d4abb1c98cb821ff4"},"proof":{"leaf_index":0,"tree_size":1,"path":[]}}"#;
    const CONTENT_HASH: &str = "3176de676b608f8f74d28054c68443fd1b999ba0349d17c7e72181990d788afc";

    fn content_hash() -> Hash {
        Hash::parse_hex(CONTENT_HASH).expect("the fixture hash is 64 hex characters")
    }

    #[test]
    fn genuine_bundle_verifies() {
        assert!(verify_bundle(BUNDLE, &content_hash()).expect("the fixture bundle parses"));
    }

    #[test]
    fn one_changed_amount_digit_fails_verification() {
        // What the entry records changed, so the recomputed content hash no longer
        // matches the recorded one. This is the page's "done when" at the logic level.
        let tampered = BUNDLE.replacen("\"10.000000\"", "\"10.000001\"", 1);
        assert_ne!(tampered, BUNDLE);
        assert!(!verify_bundle(&tampered, &content_hash()).expect("the tampered bundle parses"));
    }

    #[test]
    fn wrong_content_hash_fails_verification() {
        let mut bytes = *content_hash().as_bytes();
        bytes[0] ^= 0x01;
        let wrong = Hash::from_bytes(bytes);
        assert!(!verify_bundle(BUNDLE, &wrong).expect("the fixture bundle parses"));
    }

    #[test]
    fn malformed_bundle_is_an_error_not_a_verdict() {
        assert!(verify_bundle("{ not json", &content_hash()).is_err());
    }

    mod heads {
        use doubleentry::MerkleLog;
        use doubleentry::witness::signing::SigningKey;
        use doubleentry::witness::{KeyName, Origin, SignedTreeHead};

        use super::super::{
            Hash, HeadError, PublishedKey, TreeHead, hex_bytes, verify_consistency,
            verify_signed_head,
        };

        const ORIGIN: &str = "oxsum/ledgers/acme";

        /// The operator's key pair in the shape `GET /api/v1/log/head` publishes the
        /// public half: name, hex public key, hex selector.
        struct Operator {
            signing: SigningKey,
            key_name: String,
            public_key: String,
            key_hash: String,
        }

        impl Operator {
            fn new() -> Self {
                Self::from_seed([9; 32])
            }

            fn from_seed(seed: [u8; 32]) -> Self {
                let signing = SigningKey::from_seed(
                    KeyName::new("oxsum/tree-heads").expect("a valid key name"),
                    seed,
                );
                let verifying = signing.verifying_key();
                Self {
                    signing,
                    key_name: verifying.name().as_str().to_owned(),
                    public_key: verifying
                        .to_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    key_hash: verifying
                        .note_key_hash()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                }
            }

            fn published(&self) -> PublishedKey<'_> {
                PublishedKey {
                    key_name: &self.key_name,
                    public_key: &self.public_key,
                    key_hash: &self.key_hash,
                }
            }

            fn sign(&self, head: TreeHead, origin: &str) -> String {
                let mut note =
                    SignedTreeHead::new(Origin::new(origin).expect("a valid origin"), head);
                let body = note.body();
                note.add_signature(self.signing.sign(&body));
                note.to_note()
            }
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
        fn a_genuine_signed_head_verifies() {
            let operator = Operator::new();
            let log = log_of(7);
            let note = operator.sign(log.head(), ORIGIN);
            assert_eq!(
                verify_signed_head(&note, &log.head(), ORIGIN, &operator.published()),
                Ok(())
            );
        }

        #[test]
        fn a_consistency_proof_anchors_the_archived_head() {
            let operator = Operator::new();
            let log = log_of(11);
            let held = log.head_at(3).expect("the head exists");
            let proof = log.consistency_proof(3).expect("from is within the log");
            let note = operator.sign(log.head(), ORIGIN);
            assert_eq!(
                verify_consistency(
                    &held,
                    &held,
                    &proof,
                    &note,
                    &log.head(),
                    ORIGIN,
                    &operator.published(),
                ),
                Ok(())
            );
        }

        #[test]
        fn a_wrong_origin_is_named_a_different_ledger() {
            let operator = Operator::new();
            let log = log_of(2);
            let note = operator.sign(log.head(), "oxsum/ledgers/other-tenant");
            // Signed, consistent — just not this tenant's log.
            assert_eq!(
                verify_signed_head(&note, &log.head(), ORIGIN, &operator.published()),
                Err(HeadError::WrongOrigin)
            );
        }

        #[test]
        fn a_head_the_signature_does_not_attest_is_a_mismatch() {
            let operator = Operator::new();
            let log = log_of(5);
            let note = operator.sign(log.head(), ORIGIN);
            let mut bytes = *log.head().root.as_bytes();
            bytes[0] ^= 0x01;
            let other = TreeHead {
                size: log.head().size,
                root: Hash::from_bytes(bytes),
            };
            assert_eq!(
                verify_signed_head(&note, &other, ORIGIN, &operator.published()),
                Err(HeadError::HeadMismatch)
            );
        }

        #[test]
        fn a_note_signed_by_another_key_is_unsigned_for_the_published_key() {
            let operator = Operator::new();
            let attacker = Operator::from_seed([3; 32]);
            let log = log_of(4);
            // The note is signed, but under the attacker's key: no line matches the
            // operator's published selector.
            let note = attacker.sign(log.head(), ORIGIN);
            assert_eq!(
                verify_signed_head(&note, &log.head(), ORIGIN, &operator.published()),
                Err(HeadError::Unsigned)
            );
        }

        #[test]
        fn a_consistency_response_anchored_elsewhere_is_refused() {
            let operator = Operator::new();
            let log = log_of(9);
            let held = log.head_at(4).expect("the head exists");
            let mut elsewhere = held;
            elsewhere.root = Hash::from_bytes([0xee; 32]);
            let proof = log.consistency_proof(4).expect("from is within the log");
            let note = operator.sign(log.head(), ORIGIN);
            assert_eq!(
                verify_consistency(
                    &held,
                    &elsewhere,
                    &proof,
                    &note,
                    &log.head(),
                    ORIGIN,
                    &operator.published(),
                ),
                Err(HeadError::OldHeadMismatch)
            );
        }

        #[test]
        fn a_proof_that_does_not_extend_the_log_is_inconsistent() {
            let operator = Operator::new();
            let log = log_of(6);
            let held = log.head_at(4).expect("the head exists");
            let mut proof = log.consistency_proof(4).expect("from is within the log");
            proof.new_size = log.head().size + 1;
            let note = operator.sign(log.head(), ORIGIN);
            assert_eq!(
                verify_consistency(
                    &held,
                    &held,
                    &proof,
                    &note,
                    &log.head(),
                    ORIGIN,
                    &operator.published(),
                ),
                Err(HeadError::Inconsistent)
            );
        }

        #[test]
        fn hex_decoding_rejects_the_wrong_length_and_non_hex() {
            assert!(hex_bytes::<4>("aabbccdd").is_ok());
            assert!(hex_bytes::<4>("aabbcc").is_err());
            assert!(hex_bytes::<4>("zzbbccdd").is_err());
        }
    }

    /// The signed price binding (issue #204): the note reuses the tlog-checkpoint
    /// body — `size` is the price version, `root` the canonical row hash — so a
    /// settlement's `price` link verifies the same way a tree head does, plus the
    /// binding checks that tie it to the very price the bill named.
    mod price_heads {
        use doubleentry::witness::signing::SigningKey;
        use doubleentry::witness::{KeyName, Origin, SignedTreeHead};
        use doubleentry::{Hash, TreeHead};

        use super::super::{PriceError, PriceLink, verify_price_head};

        const CHANNEL: &str = "deepseek";
        const MODEL: &str = "deepseek-chat";
        const VERSION: i64 = 3;

        /// The operator's key in the shape the bundle's `price` link publishes.
        struct Operator {
            signing: SigningKey,
            key_name: String,
            public_key: String,
            key_hash: String,
        }

        impl Operator {
            fn from_seed(seed: [u8; 32]) -> Self {
                let signing = SigningKey::from_seed(
                    KeyName::new("oxsum/tree-heads").expect("a valid key name"),
                    seed,
                );
                let verifying = signing.verifying_key();
                Self {
                    signing,
                    key_name: verifying.name().as_str().to_owned(),
                    public_key: verifying
                        .to_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    key_hash: verifying
                        .note_key_hash()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                }
            }

            /// A `price` link over `row`, signed as the channel's catalog checkpoint.
            fn link(&self, channel: &str, version: i64, row: &str) -> PriceLink {
                let origin = Origin::new(format!("oxsum/price-catalog/{channel}")).expect("valid");
                let row_hash = Hash::digest(super::super::PRICE_ROW_DOMAIN, row.as_bytes());
                let mut note = SignedTreeHead::new(
                    origin,
                    TreeHead {
                        size: version as u64,
                        root: row_hash,
                    },
                );
                let body = note.body();
                note.add_signature(self.signing.sign(&body));
                PriceLink {
                    channel: channel.to_owned(),
                    model: MODEL.to_owned(),
                    version,
                    row: row.to_owned(),
                    note: note.to_note(),
                    key_name: self.key_name.clone(),
                    public_key: self.public_key.clone(),
                    key_hash: self.key_hash.clone(),
                }
            }
        }

        /// A bundle JSON carrying `link` under `price`, over a settlement
        /// description naming `(channel, model, version)`.
        fn bundle(description: &str, link: Option<&PriceLink>) -> String {
            let entry = serde_json::json!({ "description": description });
            match link {
                Some(link) => serde_json::json!({ "entry": entry, "price": link }).to_string(),
                None => serde_json::json!({ "entry": entry }).to_string(),
            }
        }

        fn settlement(channel: &str, model: &str, version: i64) -> String {
            serde_json::json!({
                "v": 5,
                "channel": channel,
                "model": model,
                "priceVersion": version,
            })
            .to_string()
        }

        const ROW: &str = r#"{"v":1,"channel":"deepseek","model":"deepseek-chat","version":3,"weight":100,"price":{"inputPricePerMillion":1000000,"outputPricePerMillion":1000000,"maxOutputTokens":1000}}"#;

        #[test]
        fn a_genuine_price_binding_verifies() {
            let operator = Operator::from_seed([9; 32]);
            let link = operator.link(CHANNEL, VERSION, ROW);
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Ok(()));
        }

        #[test]
        fn a_changed_price_byte_fails_the_row_hash() {
            let operator = Operator::from_seed([9; 32]);
            let link = operator.link(CHANNEL, VERSION, ROW);
            let mut link = link;
            // One digit in the stored rate: the row no longer hashes to the root.
            link.row = link.row.replace("1000000", "1000001");
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Err(PriceError::HashMismatch));
        }

        #[test]
        fn a_link_naming_another_version_is_a_binding_mismatch() {
            let operator = Operator::from_seed([9; 32]);
            // Signed honestly — but for v4, not the v3 the bill priced under.
            let link = operator.link(CHANNEL, 4, ROW);
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Err(PriceError::BindingMismatch));
        }

        #[test]
        fn a_checkpoint_of_another_catalog_is_wrong_origin() {
            let operator = Operator::from_seed([9; 32]);
            // Signed over `other-channel`'s catalog — a valid note, just not the one
            // this bill's channel answers to. Relabel the link's channel to match
            // the bill's so the binding check passes and the *note's* origin line is
            // the only thing off.
            let mut link = operator.link("other-channel", VERSION, ROW);
            link.channel = CHANNEL.to_owned();
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Err(PriceError::WrongOrigin));
        }

        #[test]
        fn a_note_signed_by_another_key_is_unsigned_for_the_published_key() {
            let attacker = Operator::from_seed([3; 32]);
            let operator = Operator::from_seed([9; 32]);
            // The attacker signs the checkpoint; the published key fields claim the
            // operator's — no signature line matches the operator's selector.
            let mut link = attacker.link(CHANNEL, VERSION, ROW);
            link.key_name = operator.key_name.clone();
            link.public_key = operator.public_key.clone();
            link.key_hash = operator.key_hash.clone();
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Err(PriceError::Unsigned));
        }

        #[test]
        fn a_bundle_without_a_price_link_reports_absent() {
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), None);
            assert_eq!(verify_price_head(&json), Err(PriceError::Absent));
        }

        #[test]
        fn a_non_settlement_entry_is_malformed_not_absent() {
            // A top-up carries no channel/model/priceVersion: there is no price to
            // bind, which is a different report than "the link is missing".
            let operator = Operator::from_seed([9; 32]);
            let link = operator.link(CHANNEL, VERSION, ROW);
            let json = bundle(r#"{"v":1,"kind":"topUp"}"#, Some(&link));
            assert!(matches!(
                verify_price_head(&json),
                Err(PriceError::Malformed(_))
            ));
        }
    }

    /// The statement log anchor (issue #221): the marker entry's digests are
    /// rebuilt from the served window and lines, and the signed head right after
    /// the marker proves the log committed it — the same trust order the checks
    /// run in.
    mod statement_anchor {
        use doubleentry::witness::signing::SigningKey;
        use doubleentry::witness::{KeyName, Origin, SignedTreeHead};
        use doubleentry::{Hash, TreeHead};

        use super::super::{
            AnchorError, STATEMENT_ROLLUP_DOMAIN, STATEMENT_WINDOW_DOMAIN, StatementAnchorCheck,
            StatementRollupLine, StatementRollupV1, verify_statement_anchor,
        };

        const ORIGIN: &str = "oxsum/ledgers/acme";

        /// The operator's key in the shape the anchor object publishes.
        struct Operator {
            signing: SigningKey,
            key_name: String,
            public_key: String,
            key_hash: String,
        }

        impl Operator {
            fn from_seed(seed: [u8; 32]) -> Self {
                let signing = SigningKey::from_seed(
                    KeyName::new("oxsum/tree-heads").expect("a valid key name"),
                    seed,
                );
                let verifying = signing.verifying_key();
                Self {
                    signing,
                    key_name: verifying.name().as_str().to_owned(),
                    public_key: verifying
                        .to_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    key_hash: verifying
                        .note_key_hash()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                }
            }

            /// The signed note over `head`, in the `oxsum/ledgers/<tenant>` origin.
            fn note(&self, head: TreeHead) -> String {
                let mut note =
                    SignedTreeHead::new(Origin::new(ORIGIN).expect("a valid origin"), head);
                let body = note.body();
                note.add_signature(self.signing.sign(&body));
                note.to_note()
            }
        }

        /// Three fake covered entries' content hashes — what the caller collected
        /// from `GET /api/v1/entries/{id}/proof`, in log order.
        fn covered() -> [String; 3] {
            [1u8, 2, 3].map(|i| Hash::digest(b"test/covered", &[i]).to_hex())
        }

        fn window_hash(covered: &[String]) -> String {
            let mut payload = Vec::with_capacity(covered.len() * 32);
            for hex in covered {
                let bytes = (0..32)
                    .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex"))
                    .collect::<Vec<u8>>();
                payload.extend_from_slice(&bytes);
            }
            Hash::digest(STATEMENT_WINDOW_DOMAIN, &payload).to_hex()
        }

        /// The rollup the statement's figures and lines canonicalize to — built
        /// through the same struct the verifier re-serializes, so the bytes are
        /// identical by construction. A `serde_json::json!` literal would sort
        /// the keys alphabetically (no `preserve_order` in the tree), which is
        /// *not* the canonical form.
        fn rollup(window_hash: &str) -> String {
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
            serde_json::to_string(&StatementRollupV1 {
                v: 1,
                period: "2026-09",
                log_from_index: Some(3),
                log_to_index: Some(8),
                entry_count: 3,
                total_minor: 2_000_000,
                credit_drawn_minor: 1_500_000,
                window_hash,
                lines: &lines,
            })
            .expect("the rollup serializes")
        }

        /// The detail body `{statement, lines, anchor}` the endpoint answers.
        /// `anchor_log_index` positions the marker; `head` is the head right
        /// after it; `operator` signs the note — `None` serves an unsigned
        /// deployment's anchor.
        fn detail(
            covered: &[String],
            anchor_log_index: i64,
            head: &TreeHead,
            operator: Option<&Operator>,
        ) -> String {
            let window_hash = window_hash(covered);
            let rollup = rollup(&window_hash);
            let rollup_hash = Hash::digest(STATEMENT_ROLLUP_DOMAIN, rollup.as_bytes()).to_hex();
            let anchor = match operator {
                Some(operator) => serde_json::json!({
                    "entryId": "00000000-0000-5000-8000-000000000001",
                    "logIndex": anchor_log_index,
                    "windowHash": window_hash,
                    "rollupHash": rollup_hash,
                    "origin": ORIGIN,
                    "note": operator.note(*head),
                    "keyName": operator.key_name,
                    "publicKey": operator.public_key,
                    "keyHash": operator.key_hash,
                }),
                None => serde_json::json!({
                    "entryId": "00000000-0000-5000-8000-000000000001",
                    "logIndex": anchor_log_index,
                    "windowHash": window_hash,
                    "rollupHash": rollup_hash,
                    "origin": ORIGIN,
                    "note": serde_json::Value::Null,
                    "keyName": serde_json::Value::Null,
                    "publicKey": serde_json::Value::Null,
                    "keyHash": serde_json::Value::Null,
                }),
            };
            serde_json::json!({
                "statement": {
                    "period": "2026-09",
                    "logFromIndex": 3,
                    "logToIndex": 8,
                    "entryCount": 3,
                    "totalMinor": 2000000,
                    "creditDrawnMinor": 1500000
                },
                "lines": [{
                    "channel": "anthropic",
                    "model": "claude",
                    "turns": 3,
                    "inputTokens": 100,
                    "outputTokens": 50,
                    "cachedTokens": 10,
                    "reasoningTokens": 5,
                    "amountMinor": 2000000
                }],
                "anchor": anchor,
            })
            .to_string()
        }

        /// The check inputs: the covered hashes and the head right after the
        /// marker — `log_index + 1` is where the marker's inclusion lands.
        /// Callers keep the `&str` view alive (`refs` below) since the check
        /// only borrows it.
        fn check<'a>(window: &'a [&'a str], head: &'a TreeHead) -> StatementAnchorCheck<'a> {
            StatementAnchorCheck {
                window_hashes: window,
                expected_head: head,
                expected_origin: ORIGIN,
            }
        }

        /// `&str` views over a `covered()` array — what `check` borrows.
        fn refs(covered: &[String]) -> Vec<&str> {
            covered.iter().map(String::as_str).collect()
        }

        #[test]
        fn a_genuine_signed_anchor_verifies() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            // The marker sits at index 8; the head at size 9 follows it.
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Ok(()),
            );
        }

        #[test]
        fn an_unsigned_deployment_verifies_to_the_digests_alone() {
            let covered = covered();
            let window = refs(&covered);
            // The head only matters when a note exists; a null note returns Ok
            // after the two digest checks.
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, None);
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Ok(()),
            );
        }

        #[test]
        fn a_truncated_window_fails_the_fold() {
            let covered = covered();
            let window = refs(&covered[..2]);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            // The caller only found two of the three covered entries.
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::WindowMismatch),
            );
        }

        #[test]
        fn a_reordered_window_fails_the_fold() {
            let covered = covered();
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            let swapped = [covered[1].clone(), covered[0].clone(), covered[2].clone()];
            let window = refs(&swapped);
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::WindowMismatch),
            );
        }

        #[test]
        fn an_altered_line_fails_the_rollup() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])))
                .replace("2000000", "2000001");
            // One amount digit changes: the rollup no longer digests to the
            // anchor's committed figure.
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::RollupMismatch),
            );
        }

        #[test]
        fn a_head_of_another_ledger_is_wrong_origin() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let attacker = Operator::from_seed([9; 32]);
            // Honestly signed — but the note names another tenant's log.
            let mut json = detail(&covered, 8, &head, Some(&attacker));
            json = json.replace(ORIGIN, "oxsum/ledgers/other-tenant");
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::WrongOrigin),
            );
        }

        #[test]
        fn a_later_head_is_not_the_one_after_the_marker() {
            let covered = covered();
            let window = refs(&covered);
            // The marker sits at index 8; the caller fetched the head at size 10.
            let head = TreeHead {
                size: 10,
                root: Hash::digest(b"test/head", b"10"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::HeadMismatch),
            );
        }

        #[test]
        fn a_note_signed_by_another_key_is_unsigned_for_the_published_key() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let attacker = Operator::from_seed([3; 32]);
            let operator = Operator::from_seed([9; 32]);
            // The attacker signs; the anchor claims the operator's key hash —
            // the served selector matches no signature line in the note.
            let json = detail(&covered, 8, &head, Some(&attacker)).replace(
                &format!("\"keyHash\":\"{}\"", attacker.key_hash),
                &format!("\"keyHash\":\"{}\"", operator.key_hash),
            );
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::Unsigned),
            );
        }

        #[test]
        fn a_tampered_note_body_is_a_bad_signature() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            // Rewrite the attested root inside the signed body: the signature no
            // longer covers it, but the head fields still parse and match.
            let mut tampered = serde_json::from_str::<serde_json::Value>(&json).expect("json");
            let note = tampered["anchor"]["note"].as_str().expect("note");
            let mut parsed = SignedTreeHead::parse(note).expect("parses");
            parsed.head.root = Hash::digest(b"test/head", b"tampered");
            tampered["anchor"]["note"] = serde_json::Value::String(parsed.to_note());
            // The signed head must attest the head the caller fetched; replace
            // `expected_head` too so only the signature check fails.
            let tampered_head = TreeHead {
                size: 9,
                root: parsed.head.root,
            };
            assert_eq!(
                verify_statement_anchor(&tampered.to_string(), &check(&window, &tampered_head),),
                Err(AnchorError::BadSignature),
            );
        }

        #[test]
        fn a_statement_without_an_anchor_reports_absent() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = serde_json::json!({
                "statement": {
                    "period": "2026-09",
                    "logFromIndex": 3,
                    "logToIndex": 8,
                    "entryCount": 3,
                    "totalMinor": 2000000,
                    "creditDrawnMinor": 1500000
                },
                "lines": [],
            })
            .to_string();
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::Absent),
            );
        }
    }
}
