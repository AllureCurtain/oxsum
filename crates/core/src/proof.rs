use doubleentry::{Balanced, Draft, Entry, Hash, InclusionProof, TreeHead};

use crate::wallet::SCALE;

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
/// Whether the tree head itself is trustworthy is established separately, by witness
/// signatures or by an archived old tree head plus a consistency proof. Not built yet;
/// see TODO.md.
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
