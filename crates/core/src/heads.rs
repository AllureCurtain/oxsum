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
use doubleentry::{ConsistencyProof, TreeHead};

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
}
