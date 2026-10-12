//! The bills page's tree-head section (issue #92).
//!
//! Every organization ledger is an append-only Merkle log whose head the operator
//! signs on demand. A user who archives today's head can later prove that the log the
//! server serves still contains it — the strongest statement a verifiable wallet can
//! make: the history behind the bills was not rewritten.
//!
//! ## Spike trust-model change
//!
//! The Leptos dashboard kept the archive in the browser's `localStorage` and ran
//! the check in the browser through `oxsum_verify`: the server only served the
//! signed head and the consistency proof, never a verdict. Topcoat has no browser
//! target, so the spike answers the check server-side on demand
//! (`POST /dashboard/bills/consistency`): the page renders the verdict the server
//! computed. The proof math is unchanged, but the user must trust the server's
//! answer — the "don't trust the server" property is gone, exactly like `/verify`.
//!
//! What is here: the view types the pages share, and the pure [`decide`] that used
//! to drive the browser archive, kept with its tests as the documented semantics of
//! the check.

use oxsum_verify::TreeHead;
use serde::{Deserialize, Serialize};

/// A bare tree head, `{size, root}`: what the archive stores and what responses carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeHeadView {
    /// Entries in the log.
    pub size: u64,
    /// The head's Merkle root, hex.
    pub root: String,
}

/// The operator's published tree-head key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadKeyView {
    /// The name the operator signs under (`oxsum/tree-heads`).
    pub key_name: String,
    /// The Ed25519 public key, hex.
    pub public_key: String,
    /// The note key hash — which signature line on a note is this key's — hex.
    pub key_hash: String,
}

/// A signed tree head, as `GET /api/v1/log/head` serves it: the complete signed-note
/// text, the head it attests, and the key that signed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedHeadView {
    /// The C2SP signed-note text; the signature covers its body byte for byte.
    pub note: String,
    /// Which log this head belongs to (`oxsum/ledgers/<tenant>`).
    pub origin: String,
    /// The key that signed the note, as the operator publishes it.
    pub key: HeadKeyView,
    /// Entries in the log.
    pub size: u64,
    /// The head's Merkle root, hex.
    pub root: String,
}

/// A consistency response, as `GET /api/v1/log/consistency` serves it: the archived
/// head's server-recomputed twin, the signed new head, and the proof between them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsistencyView {
    /// The new (current) head, signed.
    pub signed: SignedHeadView,
    /// The old head at the requested size, recomputed from the log.
    pub old_head: TreeHeadView,
    /// The proof that the log at the new head extends the log at the old head.
    pub proof: ConsistencyProofView,
}

/// The consistency proof's three wire fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsistencyProofView {
    /// Size of the earlier tree.
    pub old_size: u64,
    /// Size of the later tree.
    pub new_size: u64,
    /// The hashes that relate the two roots, hex.
    pub path: Vec<String>,
}

mod wire {
    use oxsum_core::{Consistency, SignedHead};

    use super::{ConsistencyProofView, ConsistencyView, HeadKeyView, SignedHeadView, TreeHeadView};

    impl From<&oxsum_core::KeyPublication> for HeadKeyView {
        fn from(key: &oxsum_core::KeyPublication) -> Self {
            Self {
                key_name: key.name.clone(),
                public_key: key.public_key.iter().map(|b| format!("{b:02x}")).collect(),
                key_hash: key.key_hash.iter().map(|b| format!("{b:02x}")).collect(),
            }
        }
    }

    impl From<SignedHead> for SignedHeadView {
        fn from(signed: SignedHead) -> Self {
            Self {
                note: signed.note,
                origin: signed.origin,
                key: HeadKeyView::from(&signed.key),
                size: signed.head.size,
                root: signed.head.root.to_hex(),
            }
        }
    }

    impl From<oxsum_core::TreeHead> for TreeHeadView {
        fn from(head: oxsum_core::TreeHead) -> Self {
            Self {
                size: head.size,
                root: head.root.to_hex(),
            }
        }
    }

    impl From<Consistency> for ConsistencyView {
        fn from(c: Consistency) -> Self {
            Self {
                signed: c.signed.into(),
                old_head: c.old_head.into(),
                proof: ConsistencyProofView {
                    old_size: c.proof.old_size,
                    new_size: c.proof.new_size,
                    path: c.proof.path.iter().map(|h| h.to_hex()).collect(),
                },
            }
        }
    }
}

/// What the archive does with a freshly served head, given what it holds.
///
/// Kept as the documented semantics of the archive check; the spike answers the
/// check server-side instead of running this in the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ArchiveStep {
    /// Nothing is archived for this log yet: verify the signature, then store the head.
    Record,
    /// The served head is the archived head: only the signature needs re-checking.
    Unchanged,
    /// The log grew: fetch the consistency proof from the archived size, verify it and
    /// the signature, then store the new head.
    Grow {
        /// The archived head's size — what `?from=` asks the consistency proof for.
        from: u64,
    },
    /// The served head is smaller than the archived one: history shrank.
    Shrank,
    /// Same size, different root: history was rewritten in place.
    Forked,
}

/// The archive's decision, in one pure step.
#[allow(dead_code)]
pub fn decide(archived: Option<TreeHead>, served: &TreeHead) -> ArchiveStep {
    match archived {
        None => ArchiveStep::Record,
        Some(held) if held == *served => ArchiveStep::Unchanged,
        Some(held) if held.size < served.size => ArchiveStep::Grow { from: held.size },
        Some(held) if held.size > served.size => ArchiveStep::Shrank,
        Some(_) => ArchiveStep::Forked,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use oxsum_verify::Hash;

    fn head(size: u64, byte: u8) -> TreeHead {
        TreeHead {
            size,
            root: Hash::from_bytes([byte; 32]),
        }
    }

    #[test]
    fn an_empty_archive_records() {
        assert_eq!(decide(None, &head(5, 0xaa)), ArchiveStep::Record);
    }

    #[test]
    fn the_same_head_needs_no_proof() {
        let held = head(5, 0xaa);
        assert_eq!(decide(Some(held), &held), ArchiveStep::Unchanged);
    }

    #[test]
    fn a_grown_log_asks_for_a_proof_from_the_archived_size() {
        assert_eq!(
            decide(Some(head(5, 0xaa)), &head(9, 0xbb)),
            ArchiveStep::Grow { from: 5 }
        );
    }

    #[test]
    fn a_smaller_served_head_is_a_shrunk_log() {
        assert_eq!(
            decide(Some(head(9, 0xaa)), &head(5, 0xaa)),
            ArchiveStep::Shrank
        );
    }

    #[test]
    fn a_different_root_at_the_same_size_is_a_fork() {
        assert_eq!(
            decide(Some(head(9, 0xaa)), &head(9, 0xbb)),
            ArchiveStep::Forked
        );
    }
}
