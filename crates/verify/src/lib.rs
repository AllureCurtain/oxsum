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

use doubleentry::{Balanced, Draft, Entry, InclusionProof, MerkleAccumulator};

mod charge;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod wasm;

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

/// The whole `/verify` bill verdict, composed: inclusion, the charge recompute
/// and the price binding, each reduced to the sentence a page prints.
///
/// The server-rendered form and the browser's wasm export both build their
/// verdict from this — one composition, two execution sides (issue #284).
/// `verdict` is `"pass"`, `"chargeMismatch"` or `"fail"`; `charge` and `price`
/// are empty on a fail.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillVerdict {
    /// The verdict's machine name: `"pass"`, `"chargeMismatch"` or `"fail"`.
    pub verdict: &'static str,
    /// The charge-recompute sentence, including the mandate line a v7 record
    /// carries.
    pub charge: String,
    /// The price-binding sentence.
    pub price: String,
}

/// Runs the bill checks the `/verify` page composes — [`verify_bundle`] against
/// `expected_hash`, then [`verify_charge`] and [`verify_price_head`] on a pass —
/// and composes the verdict text. `json` is the bundle's inner JSON (the caller
/// unwraps a `data` envelope). A malformed bundle is an error; a bundle that
/// does not verify is a [`BillVerdict`] whose `verdict` is `"fail"` — the
/// error/verdict split the functions below already make.
///
/// # Errors
///
/// [`serde_json::Error`] when `json` does not parse.
pub fn verify_bill(json: &str, expected_hash: &Hash) -> Result<BillVerdict, serde_json::Error> {
    if !verify_bundle(json, expected_hash)? {
        return Ok(BillVerdict {
            verdict: "fail",
            charge: String::new(),
            price: String::new(),
        });
    }
    let description = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|bundle| {
            bundle
                .get("entry")?
                .get("description")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_default();
    match verify_charge(&description) {
        ChargeCheck::Mismatch => Ok(BillVerdict {
            verdict: "chargeMismatch",
            charge: String::new(),
            price: String::new(),
        }),
        check => {
            // The mandate line is part of the charge sentence: a v7 record that
            // names the delegation says so in the verdict.
            let mandate = serde_json::from_str::<serde_json::Value>(&description)
                .ok()
                .and_then(|record| record.get("mandate").cloned())
                .and_then(|mandate| {
                    let id = mandate.get("id")?.as_str()?.to_owned();
                    let hash = mandate.get("hash")?.as_str()?.to_owned();
                    Some(format!(
                        " The turn ran under spend mandate {id} (blob sha256 {}…).",
                        &hash[..16.min(hash.len())]
                    ))
                })
                .unwrap_or_default();
            // The sentences carry their leading space: the verdict text flows
            // on from the page's own clause — "…the inclusion proof links it to
            // the tree head." — so a renderer concatenates without joining.
            let charge = match check {
                ChargeCheck::Recomputed => {
                    " The recorded charge itself recomputes from the usage and rates inside the entry."
                        .to_owned()
                }
                ChargeCheck::NotASettlement => {
                    " The entry is not a usage settlement, so there is no charge to recompute."
                        .to_owned()
                }
                ChargeCheck::OlderSchema => {
                    " The record predates charge verification — the entry is proven, but its arithmetic is too old to recompute."
                        .to_owned()
                }
                ChargeCheck::NewerSchema { .. } => {
                    " The record is newer than this verifier — the entry is proven, but its arithmetic is skipped rather than guessed at."
                        .to_owned()
                }
                ChargeCheck::Mismatch => unreachable!("handled above"),
            } + &mandate;
            let price = match verify_price_head(json) {
                Ok(()) => {
                    " The price it names is bound to a checkpoint the operator signed.".to_owned()
                }
                Err(PriceError::Absent) => {
                    " This bundle carries no signed price binding — the entry is proven, but its price is not independently attested."
                        .to_owned()
                }
                Err(error) => format!(" Its price binding does not verify: {error}."),
            };
            Ok(BillVerdict {
                verdict: "pass",
                charge,
                price,
            })
        }
    }
}

/// What checking a signed tree head — or a consistency proof anchored by one — can
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

/// What checking a witness's cosignature over an anchored checkpoint can fail
/// with — the failures a reader can act differently on, one per variant.
#[derive(Debug, PartialEq, Eq)]
pub enum WitnessVerdict {
    /// The note parses and the pinned key verifies a `cosignature/v1` line over
    /// the checkpoint body. `at` is the timestamp inside the signed message —
    /// the moment the witness claims it saw the head.
    Verified {
        /// Unix seconds inside the signature, tamper-proof.
        at: u64,
    },
    /// The stored note does not parse as a signed head — truncated, wrong
    /// shape, or not a note at all.
    Malformed,
    /// The note names a different origin than the caller expected — it verifies
    /// as a signature but answers for a different log.
    WrongOrigin,
    /// The note attests a different head than the checkpoint claims — either
    /// the stored row is stale or the witness signed a fork.
    HeadMismatch,
    /// The deployment knows no key under this witness's URL: the verifier has
    /// nothing to check the signature against — the cosigned note may be
    /// genuine but is unverifiable as configured.
    UnknownKey,
    /// The pinned key rejects the cosignature — forged bytes, or the witness
    /// signed under a different key than the deployment pinned.
    BadSignature,
}

impl std::fmt::Display for WitnessVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Verified { at } => write!(f, "witnessed at Unix second {at}"),
            Self::Malformed => write!(f, "the cosigned note could not be parsed"),
            Self::WrongOrigin => {
                write!(f, "the cosigned note names a different ledger")
            }
            Self::HeadMismatch => {
                write!(f, "the cosigned note attests a different head")
            }
            Self::UnknownKey => {
                write!(f, "the deployment publishes no key for this witness")
            }
            Self::BadSignature => {
                write!(f, "the cosignature does not verify under the pinned key")
            }
        }
    }
}

impl std::error::Error for WitnessVerdict {}

/// One witness pin — the URL the deployment publishes checkpoints to, the key
/// name the witness signs under, and its Ed25519 public half.
///
/// How a verifier holds this set decides what the check means: a pin the
/// verifier configured itself proves a real third party vouched; a pin the
/// *server* served is evidence only under the same trust as the operator's own
/// published key.
pub struct WitnessPin<'a> {
    /// The witness endpoint URL, as `OXSUM_WITNESS_URLS` names it.
    pub witness_url: &'a str,
    /// The key name the witness's cosignatures sign under.
    pub key_name: &'a str,
    /// The Ed25519 public key, hex.
    pub public_key: &'a str,
    /// The cosignature selector — `SHA-256(name || '\n' || 0x04 || public)`'s
    /// first four bytes, hex.
    pub key_hash: &'a str,
}

/// One stored cosignature, as `GET /api/v1/log/checkpoint` serves it.
pub struct WitnessNote<'a> {
    /// The witness endpoint this note came back from.
    pub witness_url: &'a str,
    /// The cosigned C2SP note text — the checkpoint body plus the witness's
    /// signature line.
    pub note: &'a str,
}

/// Checks one witness cosignature against the checkpoint it claims to vouch.
///
/// Five steps, in trust order: the cosigned note parses, it names the
/// checkpoint's own origin, it attests the checkpoint's own head, the
/// witness's URL maps to a pinned key, and that key verifies the
/// `cosignature/v1` line — returning the timestamp it carries.
///
/// A failure names itself rather than collapsing into `false`: a forged line,
/// a stale row and a missing pin all read differently on the page.
#[must_use]
pub fn verify_witness(
    note: &WitnessNote<'_>,
    pinned: &[WitnessPin<'_>],
    expected_origin: &str,
    expected_head: &TreeHead,
) -> WitnessVerdict {
    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{KeyName, SignedTreeHead};

    let parsed = match SignedTreeHead::parse(note.note) {
        Ok(p) => p,
        Err(_) => return WitnessVerdict::Malformed,
    };
    if parsed.origin.as_str() != expected_origin {
        return WitnessVerdict::WrongOrigin;
    }
    if parsed.head != *expected_head {
        return WitnessVerdict::HeadMismatch;
    }
    let Some(pin) = pinned.iter().find(|p| p.witness_url == note.witness_url) else {
        return WitnessVerdict::UnknownKey;
    };
    let Ok(selector) = hex_bytes::<4>(pin.key_hash) else {
        return WitnessVerdict::Malformed;
    };
    // The signature line the witness's pin selects: a cosignature is an
    // extra `— name blob` line on the same note, so the line claiming this
    // pin's name and cosignature-key-hash is the one to verify.
    let Some(signature) = parsed
        .signatures
        .iter()
        .find(|s| s.name.as_str() == pin.key_name && s.key_hash == selector)
    else {
        return WitnessVerdict::BadSignature;
    };
    let Ok(public) = hex_bytes::<32>(pin.public_key) else {
        return WitnessVerdict::Malformed;
    };
    let Ok(name) = KeyName::new(pin.key_name) else {
        return WitnessVerdict::Malformed;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(name, public) else {
        return WitnessVerdict::Malformed;
    };
    match verifying.verify_cosignature(&parsed.body(), signature) {
        Some(at) => WitnessVerdict::Verified { at },
        None => WitnessVerdict::BadSignature,
    }
}

/// Runs [`verify_witness`] over every cosigned note on a checkpoint.
///
/// The verdicts come back in the notes' own order — one entry per witness the
/// operator published to, so the page can name the witnesses that failed
/// rather than just count the ones that passed.
#[must_use]
pub fn verify_witnesses<'a>(
    notes: &'a [WitnessNote<'a>],
    pinned: &[WitnessPin<'_>],
    expected_origin: &str,
    expected_head: &TreeHead,
) -> Vec<(&'a str, WitnessVerdict)> {
    notes
        .iter()
        .map(|note| {
            (
                note.witness_url,
                verify_witness(note, pinned, expected_origin, expected_head),
            )
        })
        .collect()
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

// ── end-user usage reports (issue #261) ──────────────────────────────────────

/// What checking a usage report can fail with — one variant per distinct trust
/// failure, so a page names what is wrong instead of only that something is.
#[derive(Debug, PartialEq, Eq)]
pub enum UsageReportError {
    /// The bundle, the marker's description, or a hex field could not be parsed.
    Malformed(String),
    /// The marker's `DocumentRef` does not bind the subset digest — the entry
    /// the report claims commits it is not the marker the descriptor describes.
    MarkerMismatch,
    /// The marker's description does not re-serialize to the canonical
    /// commitment — the figures the report shows are not the ones committed.
    CommitmentMismatch,
    /// The marker or a shown entry's inclusion proof does not verify against
    /// the served head — the row is not in the ledger the head attests.
    Inclusion(String),
    /// The shown entries' recomputed content hashes do not fold to the subset
    /// digest the marker committed — a row was swapped, reordered, or the
    /// subset shown is not the subset the marker attests.
    SubsetMismatch,
    /// The descriptor's own `subsetHash` does not equal the fold of the shown
    /// hashes — the report's answer disagrees with its own commitment before
    /// the ledger is even asked.
    DescriptorMismatch,
    /// The head carries no signature from the published key — unsigned
    /// deployments serve no note to verify, which is `Unsigned`, not a pass.
    Unsigned,
    /// The published key rejects the head note's signature.
    BadSignature,
    /// The anchored-day completeness proof (issue #339) failed: a dropped row,
    /// a dropped day, a substituted anchor, or a head the anchor map does not
    /// fix. The message names which.
    Completeness(String),
}

impl std::fmt::Display for UsageReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "the usage report could not be parsed ({what})"),
            Self::MarkerMismatch => write!(
                f,
                "the marker's DocumentRef does not bind the subset digest"
            ),
            Self::CommitmentMismatch => write!(
                f,
                "the marker's description is not the canonical commitment the report shows"
            ),
            Self::Inclusion(what) => write!(f, "an entry's inclusion proof failed: {what}"),
            Self::SubsetMismatch => write!(
                f,
                "the shown entries do not fold to the subset digest the marker committed"
            ),
            Self::DescriptorMismatch => write!(
                f,
                "the report's subsetHash is not the fold of the entries it shows"
            ),
            Self::Unsigned => write!(f, "the head carries no signature from the published key"),
            Self::BadSignature => write!(f, "the head's signature does not verify"),
            Self::Completeness(what) => write!(f, "the completeness proof failed: {what}"),
        }
    }
}

impl std::error::Error for UsageReportError {}

/// The domain the subset digest folds under — the same constant `oxsum_core`
/// commits with (`USAGE_REPORT_DOMAIN`), redeclared so the verifier pins the
/// byte string and cannot drift from what was committed.
const USAGE_REPORT_DOMAIN: &[u8] = b"oxsum/usage-report/v1";

/// The document id the marker's `DocumentRef` must carry.
const USAGE_REPORT_DOCUMENT: &str = "oxsum/usage-report/v1";

/// The day leaf's domain (issue #337), redeclared so the verifier pins the
/// string the writer used rather than importing it.
const USAGE_DAY_DOMAIN: &[u8] = b"oxsum/usage-day/v1";

/// The anchor entry's document id (issue #338), redeclared like the report's.
const USAGE_ANCHOR_DOCUMENT: &str = "oxsum/usage-anchor/v1";

/// The day leaf: `H(domain, day || count || rows_root)` — the same bytes
/// `oxsum_core` stores in `usage_day_log`, rebuilt here so a completeness
/// proof's leaves are checked against their rows rather than trusted.
#[must_use]
pub fn usage_day_leaf(day: &str, count: u64, rows_root: &Hash) -> Hash {
    let mut payload = Vec::with_capacity(day.len() + 8 + Hash::LEN);
    payload.extend_from_slice(day.as_bytes());
    payload.extend_from_slice(&count.to_le_bytes());
    payload.extend_from_slice(rows_root.as_bytes());
    Hash::digest(USAGE_DAY_DOMAIN, &payload)
}

/// One anchor-map leaf: `H(domain, endUser || head_root || day_count)`.
#[must_use]
pub fn usage_anchor_pair_leaf(end_user: &str, head_root: &Hash, size: u64) -> Hash {
    let mut payload = Vec::with_capacity(end_user.len() + Hash::LEN + 8);
    payload.extend_from_slice(end_user.as_bytes());
    payload.extend_from_slice(head_root.as_bytes());
    payload.extend_from_slice(&size.to_le_bytes());
    Hash::digest(USAGE_ANCHOR_MAP_DOMAIN, &payload)
}

/// One entry in the bundle — the entry itself, the content hash the log
/// recorded for it, and its inclusion proof. The hash is served alongside so
/// the verifier can `adopt_verified` the entry (recompute-and-bind) instead of
/// trusting the bytes it was handed.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportEntryWire {
    entry: Entry<Draft, SCALE>,
    content_hash: String,
    proof: InclusionProof,
}

impl ReportEntryWire {
    /// Recomputes the entry's content hash and binds it to the recorded one —
    /// a tampered entry fails adoption before its proof is ever asked. On a
    /// clean bind returns the balanced entry whose `content_hash()` is now
    /// trustworthy.
    fn adopt(&self) -> Result<Entry<Balanced, SCALE>, UsageReportError> {
        let recorded = Hash::parse_hex(&self.content_hash)
            .map_err(|_| UsageReportError::Malformed("a content hash is not hex".into()))?;
        // adopt_verified consumes the draft; clone so the caller still holds it.
        self.entry.clone().adopt_verified(recorded).map_err(|_| {
            UsageReportError::Malformed(
                "an entry's bytes do not hash to its recorded content hash".into(),
            )
        })
    }
}

/// The canonical commitment the marker's description must re-serialize to.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageReportCommitment<'a> {
    v: i64,
    end_user: &'a str,
    from: &'a str,
    to: &'a str,
    entry_count: i64,
    charged_total_minor: i64,
    subset_hash: &'a str,
}

/// The report bundle as `GET …/usage-reports` serves it — every field the
/// check binds, redeclared wire-only so the verifier never imports the writer.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageReportWire {
    /// The marker's ledger id — carried for the caller's bookkeeping; the
    /// checks bind the digests and the head, not the id.
    #[allow(dead_code)]
    report_id: String,
    end_user: String,
    from: String,
    to: String,
    entry_count: i64,
    charged_total_minor: i64,
    subset_hash: String,
    marker: ReportEntryWire,
    entries: Vec<ReportEntryWire>,
    head: TreeHead,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
    /// The completeness proof (issue #339), present when the span is covered by
    /// a daily anchor: the customer's day-log head, its anchor-map inclusion,
    /// and every day leaf the head was folded from.
    #[serde(default)]
    completeness: Option<CompletenessWire>,
}

/// The completeness proof's anchor half: the ledger entry that committed the
/// day's map root, adopted and proven against the bundle head, plus the day
/// and figures its description must carry.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletenessAnchorWire {
    entry: ReportEntryWire,
    /// The anchored UTC day, ISO `YYYY-MM-DD`.
    day: String,
    /// The sorted-map root over every customer's head at that day, hex.
    map_root: String,
    /// How many customers the map covered.
    enduser_count: u64,
}

/// One day leaf in the customer's day log: the leaf, its index in the head,
/// its inclusion proof, and the positions (in the report's `entries`) of the
/// rows that day's leaf covers. Days outside the span carry no positions —
/// their leaves are bound by the head alone.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletenessDayWire {
    /// The UTC day, ISO `YYYY-MM-DD`.
    day: String,
    /// Position of this leaf in the customer's day log, zero-based.
    index: u64,
    /// The claimed day leaf, hex.
    leaf: String,
    /// The leaf's inclusion proof into the head.
    proof: InclusionProof,
    /// Positions in the report's `entries` this day's leaf was rebuilt from.
    #[serde(default)]
    rows: Vec<usize>,
}

/// The completeness proof as the bundle serves it (issue #339): the anchor that
/// fixed the customer's head, the head itself, the map proof that places the
/// head, and every day leaf the head covers.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletenessWire {
    v: i64,
    anchor: CompletenessAnchorWire,
    /// The customer's day-log head at the anchor day.
    head: TreeHead,
    /// The head's inclusion proof in the anchor map (the proof carries the
    /// customer's position).
    map_proof: InclusionProof,
    days: Vec<CompletenessDayWire>,
}

/// Verifies a signed end-user usage report (issue #261).
///
/// `json` is the `data` object `GET …/usage-reports` answers. `expected_origin`
/// is the tenant's ledger origin (`oxsum/ledgers/<tenant>`) the signed head is
/// expected to name. `key` is the published head key the caller fetched
/// through a channel it trusts — or the deployment's own claim, the same
/// choice `verify_signed_head` already asks for. `expected_end_user` lets the
/// verifier name who the report should be *for*: a report minted for somebody
/// else verifies as a signature but answers a different question.
///
/// Six checks, in trust order:
///
/// 1. The bundle parses and the descriptor is self-consistent — `subsetHash`
///    folds the shown entries' recomputed content hashes, or the report
///    disagrees with itself before the ledger is asked (`DescriptorMismatch`).
/// 2. The marker's `DocumentRef` carries `oxsum/usage-report/v1` bound to the
///    same digest — the committed marker is the commitment the descriptor
///    describes (`MarkerMismatch`).
/// 3. The marker's description parses as the canonical commitment and matches
///    the descriptor's figures — `endUser`, `from`, `to`, `entryCount`,
///    `chargedTotalMinor` — so the document the customer reads is the document
///    committed (`CommitmentMismatch`).
/// 4. The marker and every shown entry verify inclusion against the head —
///    each is in the ledger the head attests (`Inclusion`).
/// 5. `entryCount` matches the shown entries' length and `end_user` the
///    expected one — the subset is the one the verifier asked about.
/// 6. On a signing deployment the head note parses under `expected_origin`,
///    attests the served head, and verifies under the published key
///    (`Unsigned`/`BadSignature`/`HeadMismatch`-shaped failures mapped in).
///
/// Completeness is the org's claim, recorded in `docs/decisions.md`: the
/// marker attests the subset and binds it to the digest, and the customer
/// cross-checks `entryCount`/`chargedTotalMinor` against their own records —
/// the check cannot enumerate rows the org chose never to show.
///
/// # Errors
///
/// The first check that fails, as a [`UsageReportError`].
pub fn verify_usage_report(
    json: &str,
    expected_origin: &str,
    expected_end_user: &str,
    key: Option<&PublishedKey<'_>>,
) -> Result<(), UsageReportError> {
    let wire: UsageReportWire =
        serde_json::from_str(json).map_err(|e| UsageReportError::Malformed(e.to_string()))?;

    // Bind every shown entry's bytes to its recorded content hash first — a
    // tampered row fails adoption before its proof is ever asked.
    let marker = wire.marker.adopt()?;
    let mut shown = Vec::with_capacity(wire.entries.len());
    for row in &wire.entries {
        shown.push(row.adopt()?);
    }

    // The subset digest the descriptor commits — recompute it from the shown
    // entries' now-verified hashes before asking the marker anything.
    let mut payload = Vec::with_capacity(shown.len() * Hash::LEN);
    for entry in &shown {
        payload.extend_from_slice(entry.content_hash().as_bytes());
    }
    let folded = Hash::digest(USAGE_REPORT_DOMAIN, &payload);
    if folded.to_hex() != wire.subset_hash {
        return Err(UsageReportError::DescriptorMismatch);
    }

    // The marker binds the digest — the entry it claims commits is the one
    // whose DocumentRef names the subset.
    let Some(document) = marker.document() else {
        return Err(UsageReportError::MarkerMismatch);
    };
    let bound = document
        .content_hash
        .map(|h| h.to_hex())
        .unwrap_or_default();
    if document.id.as_str() != USAGE_REPORT_DOCUMENT || bound != wire.subset_hash {
        return Err(UsageReportError::MarkerMismatch);
    }

    // The marker's description re-serializes to the canonical commitment the
    // descriptor shows — the figures the customer reads are the ones committed.
    let commitment: UsageReportCommitment<'_> = serde_json::from_str(marker.description().as_str())
        .map_err(|e| UsageReportError::Malformed(e.to_string()))?;
    if commitment.v != 1
        || commitment.end_user != wire.end_user
        || commitment.from != wire.from
        || commitment.to != wire.to
        || commitment.entry_count != wire.entry_count
        || commitment.charged_total_minor != wire.charged_total_minor
        || commitment.subset_hash != wire.subset_hash
    {
        return Err(UsageReportError::CommitmentMismatch);
    }

    // Membership: the marker and every shown entry include against the head.
    if !wire.marker.proof.verify(&marker.content_hash(), &wire.head) {
        return Err(UsageReportError::Inclusion("the report marker".into()));
    }
    for (i, (row, entry)) in wire.entries.iter().zip(shown.iter()).enumerate() {
        if !row.proof.verify(&entry.content_hash(), &wire.head) {
            return Err(UsageReportError::Inclusion(format!("entry {i}")));
        }
    }

    // The subset is the one the verifier asked about.
    if wire.entry_count as usize != wire.entries.len() {
        return Err(UsageReportError::SubsetMismatch);
    }
    if wire.end_user != expected_end_user {
        return Err(UsageReportError::Malformed(format!(
            "the report is for {expected_end_user:?} but commits {:?}",
            wire.end_user
        )));
    }

    // Completeness (issue #339): when the bundle carries the anchored-day
    // proof, the shown rows must rebuild the day leaves the customer's
    // anchored head was folded from — a dropped row changes its day's leaf, a
    // dropped day breaks the head, and a substituted anchor fails the map.
    if let Some(completeness) = &wire.completeness {
        verify_completeness(completeness, &wire, &shown)?;
    }

    // The head's signature, when the deployment signs heads — the same check
    // `verify_signed_head` runs, mapped into this report's vocabulary.
    let Some(note) = wire.note.as_deref() else {
        // An unsigned deployment serves no note; the ledger membership is
        // still proven, and the head is simply unattested — the report is
        // verified to the two digests alone.
        return Ok(());
    };
    let key = key.ok_or(UsageReportError::Unsigned)?;
    // The bundle names the key it claims signed the head; when the verifier
    // holds the published key already, the two must agree — a bundle that names
    // a different key than the one being trusted is not self-consistent.
    for (served, held, what) in [
        (wire.key_name.as_deref(), key.key_name, "key name"),
        (wire.public_key.as_deref(), key.public_key, "public key"),
        (wire.key_hash.as_deref(), key.key_hash, "key hash"),
    ] {
        if let Some(served) = served
            && served != held
        {
            return Err(UsageReportError::Malformed(format!(
                "the bundle's {what} does not match the published key"
            )));
        }
    }
    verify_signed_head(note, &wire.head, expected_origin, key).map_err(|e| match e {
        HeadError::Malformed(what) => UsageReportError::Malformed(what),
        HeadError::WrongOrigin => {
            UsageReportError::Malformed("the head names a different ledger".into())
        }
        HeadError::HeadMismatch => {
            UsageReportError::Malformed("the signed head does not attest the served head".into())
        }
        HeadError::Unsigned => UsageReportError::Unsigned,
        HeadError::BadSignature => UsageReportError::BadSignature,
        // verify_signed_head's consistency-only variants cannot arise here.
        HeadError::OldHeadMismatch | HeadError::Inconsistent => {
            UsageReportError::Malformed(e.to_string())
        }
    })?;
    Ok(())
}

/// Verifies the anchored-day completeness proof (issue #339).
///
/// 1. The anchor entry's bytes hash to its recorded content hash, its document
///    is the anchor document bound to the map root, and its description names
///    the same day, map root and customer count as the proof.
/// 2. The anchor entry is included in the same ledger head the marker and rows
///    are — the anchor is a fact of this ledger, not an excerpt.
/// 3. The anchor day is not before the report's end, so the head it fixed
///    covers the span's last days.
/// 4. Every day leaf is included at its claimed index and the indices are
///    exactly `0..head.size` with no gap — no leaf can be missing.
/// 5. The customer's head is the pair the anchor map fixes: the pair leaf
///    `H(endUser, head, dayCount)` is included in the map root.
/// 6. Every shown row belongs to exactly one day leaf and carries that day's
///    booking date; a day with rows must rebuild to its leaf, so a dropped
///    row, a swapped day or an extracted row fails here.
fn verify_completeness(
    completeness: &CompletenessWire,
    wire: &UsageReportWire,
    shown: &[Entry<Balanced, SCALE>],
) -> Result<(), UsageReportError> {
    if completeness.v != 1 {
        return Err(UsageReportError::Completeness(
            "unsupported proof version".into(),
        ));
    }
    // 1. The anchor entry is the anchor its description claims.
    let anchor = completeness.anchor.entry.adopt()?;
    let Some(document) = anchor.document() else {
        return Err(UsageReportError::Completeness(
            "the anchor entry names no document".into(),
        ));
    };
    let bound = document
        .content_hash
        .map(|h| h.to_hex())
        .unwrap_or_default();
    if document.id.as_str() != USAGE_ANCHOR_DOCUMENT || bound != completeness.anchor.map_root {
        return Err(UsageReportError::Completeness(
            "the anchor entry does not bind the map root".into(),
        ));
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AnchorCommitment {
        v: i64,
        day: String,
        map_root: String,
        enduser_count: u64,
    }
    let commitment: AnchorCommitment = serde_json::from_str(anchor.description().as_str())
        .map_err(|e| UsageReportError::Malformed(e.to_string()))?;
    if commitment.v != 1
        || commitment.day != completeness.anchor.day
        || commitment.map_root != completeness.anchor.map_root
        || commitment.enduser_count != completeness.anchor.enduser_count
    {
        return Err(UsageReportError::Completeness(
            "the anchor's description disagrees with the proof".into(),
        ));
    }
    // 2. The anchor is in the ledger the bundle's head attests.
    if !completeness
        .anchor
        .entry
        .proof
        .verify(&anchor.content_hash(), &wire.head)
    {
        return Err(UsageReportError::Completeness(
            "the anchor entry is not in the ledger head".into(),
        ));
    }
    // 3. The anchor covers the span's end.
    if completeness.anchor.day.as_str() < wire.to.as_str() {
        return Err(UsageReportError::Completeness(
            "the anchor predates the report's span".into(),
        ));
    }
    // 4. Every leaf is included; the indices have no gap.
    let size = completeness.head.size;
    if size == 0 || completeness.days.len() as u64 != size {
        return Err(UsageReportError::Completeness(
            "the leaf count does not match the head".into(),
        ));
    }
    let mut index_seen = vec![false; size as usize];
    let mut row_seen = vec![false; shown.len()];
    for day in &completeness.days {
        let index = day.index as usize;
        if index >= index_seen.len() || index_seen[index] {
            return Err(UsageReportError::Completeness(
                "a day leaf index is out of range or repeated".into(),
            ));
        }
        index_seen[index] = true;
        let leaf = Hash::parse_hex(&day.leaf)
            .map_err(|_| UsageReportError::Malformed("a day leaf is not hex".into()))?;
        if !day.proof.verify(&leaf, &completeness.head) {
            return Err(UsageReportError::Completeness(format!(
                "the day leaf for {} is not in the customer's head",
                day.day
            )));
        }
        // 6. The day's rows rebuild its leaf.
        if !day.rows.is_empty() {
            let mut accumulator = MerkleAccumulator::new();
            for &position in &day.rows {
                let Some(entry) = shown.get(position) else {
                    return Err(UsageReportError::Completeness(
                        "a row position is out of range".into(),
                    ));
                };
                if row_seen[position] {
                    return Err(UsageReportError::Completeness(
                        "a row is claimed by two day leaves".into(),
                    ));
                }
                row_seen[position] = true;
                if entry.booking_date().to_string() != day.day {
                    return Err(UsageReportError::Completeness(
                        "a row's booking date is not the day leaf's day".into(),
                    ));
                }
                accumulator.push(entry.content_hash());
            }
            let rebuilt = usage_day_leaf(&day.day, day.rows.len() as u64, &accumulator.root());
            if rebuilt != leaf {
                return Err(UsageReportError::Completeness(format!(
                    "the day leaf for {} does not match its rows",
                    day.day
                )));
            }
        }
    }
    if index_seen.iter().any(|seen| !seen) {
        return Err(UsageReportError::Completeness(
            "the customer's day log has a gap".into(),
        ));
    }
    if row_seen.iter().any(|seen| !seen) {
        return Err(UsageReportError::Completeness(
            "a shown row belongs to no day leaf".into(),
        ));
    }
    // 5. The head is the pair the anchor map fixes.
    let pair = usage_anchor_pair_leaf(&wire.end_user, &completeness.head.root, size);
    let map_root = Hash::parse_hex(&completeness.anchor.map_root)
        .map_err(|_| UsageReportError::Malformed("the map root is not hex".into()))?;
    let map_head = TreeHead {
        size: completeness.anchor.enduser_count,
        root: map_root,
    };
    if !completeness.map_proof.verify(&pair, &map_head) {
        return Err(UsageReportError::Completeness(
            "the customer's head is not in the anchor map".into(),
        ));
    }
    Ok(())
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

/// The domain of one anchor-map leaf: `H(domain, endUser || head_root || size)`.
///
/// The daily anchor commits a sorted map from `endUser` to that customer's
/// day-log head (root and day count), so a report can prove its customer's head
/// is the one the anchor fixed without the verifier holding every other
/// customer's head (issue #338, docs/decisions.md 2026-10-22).
pub const USAGE_ANCHOR_MAP_DOMAIN: &[u8] = b"oxsum/usage-anchor-map/v1";

/// Folds the anchor map's pairs — sorted by `endUser`, unique per customer —
/// into the root the anchor entry's `DocumentRef` binds.
///
/// The verifier never rebuilds this from the whole map: a completeness proof
/// carries one pair's sibling path (stage 3). This function is the writer's and
/// the test's reference for what that path must fold to.
#[must_use]
pub fn usage_anchor_map_root(pairs: &[(&str, Hash, u64)]) -> Hash {
    let mut accumulator = MerkleAccumulator::new();
    for (end_user, head_root, size) in pairs {
        accumulator.push(usage_anchor_pair_leaf(end_user, head_root, *size));
    }
    accumulator.root()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The anchor map binds every pair: dropping one, changing a head, or
    /// changing a day count each yields a different root.
    #[test]
    fn the_anchor_map_root_binds_every_pair() {
        let a = Hash::from_bytes([1; 32]);
        let b = Hash::from_bytes([2; 32]);
        let both = usage_anchor_map_root(&[("a", a, 1), ("b", b, 2)]);
        assert_eq!(both, usage_anchor_map_root(&[("a", a, 1), ("b", b, 2)]));
        assert_ne!(both, usage_anchor_map_root(&[("a", a, 1)]));
        assert_ne!(both, usage_anchor_map_root(&[("a", a, 2), ("b", b, 2)]));
        assert_ne!(both, usage_anchor_map_root(&[("a", b, 1), ("b", b, 2)]));
    }

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

    /// `verify_bill` composes inclusion, charge and price into the verdict the
    /// page prints — the composition the server-rendered form and the wasm
    /// export share (issue #284).
    mod bill_verdict {
        use super::{BUNDLE, CONTENT_HASH, content_hash};
        use crate::{Hash, verify_bill};

        #[test]
        fn a_genuine_top_up_passes_with_no_charge_or_price() {
            let bill = verify_bill(BUNDLE, &content_hash()).expect("the fixture parses");
            assert_eq!(bill.verdict, "pass");
            // A top-up is not a settlement: the charge sentence says so. Its
            // empty description names no price, so the price check reports
            // Malformed — the same sentence the server page has always printed
            // for a non-settlement bundle.
            assert!(bill.charge.contains("no charge to recompute"));
            assert!(bill.price.contains("does not verify"));
        }

        #[test]
        fn a_tampered_bundle_is_a_fail_not_an_error() {
            let tampered = BUNDLE.replacen("\"10.000000\"", "\"10.000001\"", 1);
            let bill = verify_bill(&tampered, &content_hash()).expect("the bundle parses");
            assert_eq!(bill.verdict, "fail");
            assert!(bill.charge.is_empty() && bill.price.is_empty());
        }

        #[test]
        fn a_wrong_content_hash_is_a_fail() {
            let mut bytes = *content_hash().as_bytes();
            bytes[0] ^= 0x01;
            let wrong = Hash::from_bytes(bytes);
            let bill = verify_bill(BUNDLE, &wrong).expect("the bundle parses");
            assert_eq!(bill.verdict, "fail");
        }

        #[test]
        fn malformed_json_is_an_error_not_a_verdict() {
            let hash = Hash::parse_hex(CONTENT_HASH).expect("64 hex");
            assert!(verify_bill("{ not json", &hash).is_err());
        }
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

    /// Third-party cosignatures (issue #259): a `cosignature/v1` line on a stored
    /// note is checked against the witness's pinned key, under the same "each
    /// failure names itself" rule as `verify_signed_head`. `verify_cosignature`
    /// is C2SP's `tlog-cosignature` — the timestamp lives inside the signed
    /// message, so a tampered `at` fails the signature, not just the verdict.
    mod witnesses {
        use doubleentry::witness::signing::SigningKey;
        use doubleentry::witness::{KeyName, Origin, SignedTreeHead};
        use doubleentry::{Hash, TreeHead};

        use super::super::{
            WitnessNote, WitnessPin, WitnessVerdict, verify_witness, verify_witnesses,
        };

        const ORIGIN: &str = "oxsum/ledgers/acme";
        const WITNESS_URL: &str = "https://witness.example.com";

        /// A witness's key pair as the operator's pinset publishes it:
        /// url, cosignature name, hex public key, hex 4-byte selector.
        struct Witness {
            signing: SigningKey,
            url: String,
            key_name: String,
            public_key: String,
            key_hash: String,
        }

        impl Witness {
            fn new(url: &str, name: &str, seed: [u8; 32]) -> Self {
                let signing =
                    SigningKey::from_seed(KeyName::new(name).expect("a valid name"), seed);
                let verifying = signing.verifying_key();
                Self {
                    signing,
                    url: url.to_owned(),
                    key_name: verifying.name().as_str().to_owned(),
                    public_key: verifying
                        .to_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    key_hash: verifying
                        .cosignature_key_hash()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                }
            }

            fn pin(&self) -> WitnessPin<'_> {
                WitnessPin {
                    witness_url: &self.url,
                    key_name: &self.key_name,
                    public_key: &self.public_key,
                    key_hash: &self.key_hash,
                }
            }

            /// The cosigned note the witness's `/add-checkpoint` answers:
            /// the checkpoint's body verbatim plus its cosignature line.
            fn cosign(&self, head: TreeHead, origin: &str, at: u64) -> String {
                let mut parsed =
                    SignedTreeHead::new(Origin::new(origin).expect("a valid origin"), head);
                let body = parsed.body();
                parsed.add_signature(self.signing.cosign_body(&body, at));
                parsed.to_note()
            }
        }

        /// A small head the tests agree on.
        fn head() -> TreeHead {
            TreeHead {
                size: 42,
                root: Hash::digest(b"test/witness", b"42"),
            }
        }

        #[test]
        fn a_genuine_cosignature_verifies() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let note = witness.cosign(head(), ORIGIN, 1_762_000_000);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            let verdict = verify_witness(&cosign, &pinned, ORIGIN, &head());
            assert!(matches!(
                verdict,
                WitnessVerdict::Verified { at: 1_762_000_000 }
            ));
        }

        #[test]
        fn a_tampered_cosignature_fails() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let mut note = witness.cosign(head(), ORIGIN, 1_762_000_000);
            // Rewrite the timestamp inside the signature's first bytes — the
            // signature then no longer covers the message the verifier rebuilds.
            let tampered_at = 1_700_000_000u64;
            let mut parsed = SignedTreeHead::parse(&note).expect("parses");
            // The value's first 8 bytes are the timestamp: re-stamp them.
            let sig = parsed.signatures.first_mut().expect("one signature");
            sig.value[..8].copy_from_slice(&tampered_at.to_be_bytes());
            note = parsed.to_note();
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::BadSignature,
            );
        }

        #[test]
        fn a_cosignature_for_a_different_head_is_a_head_mismatch() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let other = TreeHead {
                size: 43,
                root: Hash::digest(b"test/witness", b"43"),
            };
            let note = witness.cosign(other, ORIGIN, 1_762_000_000);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::HeadMismatch,
            );
        }

        #[test]
        fn a_cosignature_for_a_different_origin_is_wrong_origin() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let note = witness.cosign(head(), "oxsum/ledgers/other-tenant", 1_762_000_000);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::WrongOrigin,
            );
        }

        #[test]
        fn an_unpinned_witness_reports_unknown_key() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let other = Witness::new("https://other.example.com", "w2", [2; 32]);
            let note = witness.cosign(head(), ORIGIN, 1_762_000_000);
            // The pinset knows a different witness only — the note's own URL finds no pin.
            let pinned = [other.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::UnknownKey,
            );
        }

        #[test]
        fn a_malformed_note_is_named_as_such() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: "this is not a signed tree head",
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::Malformed,
            );
        }

        #[test]
        fn verify_witnesses_reports_each_note_by_url() {
            let w1 = Witness::new("https://w1.example.com", "w1", [1; 32]);
            let w2 = Witness::new("https://w2.example.com", "w2", [2; 32]);
            let head = head();
            let good = w1.cosign(head, ORIGIN, 1_762_000_000);
            let bad = w2.cosign(
                TreeHead {
                    size: 43,
                    root: Hash::digest(b"test/witness", b"other"),
                },
                ORIGIN,
                1_762_000_000,
            );
            let pinned = [w1.pin(), w2.pin()];
            let notes = [
                WitnessNote {
                    witness_url: "https://w1.example.com",
                    note: &good,
                },
                WitnessNote {
                    witness_url: "https://w2.example.com",
                    note: &bad,
                },
            ];
            let verdicts = verify_witnesses(&notes, &pinned, ORIGIN, &head);
            assert_eq!(verdicts.len(), 2);
            assert!(matches!(verdicts[0].1, WitnessVerdict::Verified { .. }));
            assert_eq!(verdicts[1].1, WitnessVerdict::HeadMismatch);
        }

        /// The stale-checkpoint case: a witness's note is genuine but older than
        /// the head's own `first_seen` — the check is a signature check, and the
        /// page's caller decides whether the timestamp is recent enough, so the
        /// verdict carries the `at` and lets the caller reason about staleness.
        #[test]
        fn an_old_cosignature_still_verifies_but_marks_its_time() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let note = witness.cosign(head(), ORIGIN, 1_000_000_000);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            let verdict = verify_witness(&cosign, &pinned, ORIGIN, &head());
            assert!(matches!(
                verdict,
                WitnessVerdict::Verified { at: 1_000_000_000 }
            ));
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
                "v": 7,
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
