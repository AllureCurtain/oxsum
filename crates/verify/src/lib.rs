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
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProofBundle {
    pub entry: Entry<Balanced, SCALE>,
    pub head: TreeHead,
    pub proof: InclusionProof,
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
}
