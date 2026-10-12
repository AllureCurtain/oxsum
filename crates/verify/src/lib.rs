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
}
