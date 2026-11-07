//! The bill proof bundle and its inclusion check: the entry source, the
//! inclusion proof, and the tree head as of the proof.

use doubleentry::{Balanced, Draft, Entry, Hash, InclusionProof, TreeHead};

use crate::SCALE;
use crate::price::PriceLink;
use crate::sale::SaleLink;

/// The bill proof bundle handed to the user: the entry source, an inclusion proof,
/// and the tree head as of the proof.
///
/// `price` binds the bill's `(channel, model, priceVersion)` to a signed checkpoint
/// of the price row itself (issue #204). It is `Option` because a bundle prices
/// only settlements — holds, top-ups and adjustments carry none — and because a
/// deployment with no `OXSUM_HEAD_SIGNING_KEY` serves the proof without it.
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct ProofBundle {
    /// The entry source as stored, in the shape the content hash covers.
    pub entry: Entry<Balanced, SCALE>,
    /// The signed tree head the proof was taken against.
    pub head: TreeHead,
    /// The inclusion proof from the entry to `head`.
    pub proof: InclusionProof,
    /// The signed price binding, when the entry settled against a priced channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<PriceLink>,
    /// The signed sale binding, when the settlement priced under a sale book.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sale: Option<SaleLink>,
}

impl ProofBundle {
    /// Assembles a bundle from its parts.
    #[must_use]
    pub fn new(
        entry: Entry<Balanced, SCALE>,
        head: TreeHead,
        proof: InclusionProof,
        price: Option<PriceLink>,
        sale: Option<SaleLink>,
    ) -> Self {
        Self {
            entry,
            head,
            proof,
            price,
            sale,
        }
    }
}

/// Verifies a bill on the client, without trusting the server.
///
/// 1. Recomputes the content hash from the entry source; it must equal the hash the
///    user recorded with the bill. The hash the server returns this time does not count.
/// 2. Links that hash to the tree head with the inclusion proof.
///
/// Whether the tree head itself is trustworthy is established separately, by the
/// operator's signed-note signature ([`crate::verify_signed_head`]) and, for a head the holder
/// has archived, by a consistency proof that the new head extends it
/// ([`crate::verify_consistency`]).
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

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A real bundle captured from a wallet ledger (one top-up of 10 credits),
    /// with the content hash the ledger recorded for it. Verification is pure
    /// over these bytes, so the fixture is stable.
    pub(crate) const BUNDLE: &str = r#"{"entry":{"id":"73bdf311-6f39-5a37-b742-b786445e854a","idempotency_key":"666978747572652d31","booking_date":[2026,274],"value_date":[2026,274],"description":"","postings":[{"account":1,"direction":"Debit","amount":"10.000000","currency":"USD","layer":"Settled","dimensions":{}},{"account":0,"direction":"Credit","amount":"10.000000","currency":"USD","layer":"Settled","dimensions":{}}],"kind":null,"provenance":{"actor":null,"source":null,"correlation":null},"document":null,"reverses":null,"original_booking_date":null},"head":{"size":1,"root":"43531c9c43c5a9e6ce51d6fa1d170b3d09971bbf97cf516d4abb1c98cb821ff4"},"proof":{"leaf_index":0,"tree_size":1,"path":[]}}"#;
    pub(crate) const CONTENT_HASH: &str =
        "3176de676b608f8f74d28054c68443fd1b999ba0349d17c7e72181990d788afc";

    pub(crate) fn content_hash() -> Hash {
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
}
