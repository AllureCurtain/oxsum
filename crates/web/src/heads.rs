//! The bills page's tree-head archive (issue #92).
//!
//! Every organization ledger is an append-only Merkle log whose head the operator
//! signs on demand. A user who archives today's head can later prove that the log the
//! server serves still contains it — the strongest statement a verifiable wallet can
//! make: the history behind the bills was not rewritten. The archive lives in the
//! browser's `localStorage`, keyed by the log's origin, and the check itself runs in
//! the browser through `oxsum_verify`: the server only serves the signed head and the
//! consistency proof, never a verdict.
//!
//! What is here: the view types the server functions and the page share, the pure
//! [`decide`] that turns an archived head plus a served head into an action, and —
//! under `hydrate` — the two `localStorage` calls. The signature and proof checking
//! stays in `oxsum_verify`, which is what actually compiles to WASM.

use oxsum_verify::{Hash, PublishedKey, TreeHead};
use serde::{Deserialize, Serialize};

/// The deployment's head-signing seed, handed to the server functions through leptos
/// context (`crates/server/src/web.rs`). `None` means this deployment signs no heads —
/// the archive check then reports itself not configured rather than failing, the same
/// way the `/api/v1/log` endpoints answer 503 there.
#[cfg(feature = "ssr")]
#[derive(Clone)]
pub struct HeadSeed(pub Option<[u8; 32]>);

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

impl TreeHeadView {
    /// The typed head, or `None` when the served root is not 32 bytes of hex.
    pub fn head(&self) -> Option<TreeHead> {
        Some(TreeHead {
            size: self.size,
            root: Hash::parse_hex(&self.root).ok()?,
        })
    }
}

impl SignedHeadView {
    /// The head the note attests, parsed.
    pub fn head(&self) -> Option<TreeHead> {
        Some(TreeHead {
            size: self.size,
            root: Hash::parse_hex(&self.root).ok()?,
        })
    }

    /// The published key in `oxsum_verify`'s shape.
    pub fn published_key(&self) -> PublishedKey<'_> {
        PublishedKey {
            key_name: &self.key.key_name,
            public_key: &self.key.public_key,
            key_hash: &self.key.key_hash,
        }
    }
}

impl ConsistencyView {
    /// The typed proof, or `None` when a path hash is not 32 bytes of hex.
    pub fn consistency_proof(&self) -> Option<oxsum_verify::ConsistencyProof> {
        Some(oxsum_verify::ConsistencyProof {
            old_size: self.proof.old_size,
            new_size: self.proof.new_size,
            path: self
                .proof
                .path
                .iter()
                .map(|h| Hash::parse_hex(h).ok())
                .collect::<Option<Vec<_>>>()?,
        })
    }
}

#[cfg(feature = "ssr")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// The archive's decision, in one pure step so the browser code has nothing to judge.
pub fn decide(archived: Option<TreeHead>, served: &TreeHead) -> ArchiveStep {
    match archived {
        None => ArchiveStep::Record,
        Some(held) if held == *served => ArchiveStep::Unchanged,
        Some(held) if held.size < served.size => ArchiveStep::Grow { from: held.size },
        Some(held) if held.size > served.size => ArchiveStep::Shrank,
        Some(_) => ArchiveStep::Forked,
    }
}

/// The archived head's `localStorage` calls — browser-only: on the server there is no
/// storage and the archive check simply does not run.
#[cfg(feature = "hydrate")]
pub mod archive {
    use oxsum_verify::{Hash, TreeHead};

    /// One log's slot in `localStorage`, keyed by the log's origin so two
    /// organizations on one browser can never read as each other.
    pub fn key(origin: &str) -> String {
        format!("oxsum.tree-head.{origin}")
    }

    /// The stored head as `size:root`, plain text — an archive a user could diff by
    /// hand against the page if they ever wanted to.
    fn render(head: &TreeHead) -> String {
        format!("{}:{}", head.size, head.root.to_hex())
    }

    /// What is archived for `origin`, or `None` — nothing stored, storage denied, or
    /// an unreadable value. An unreadable archive reads as a fresh start: corruption
    /// is a local fault, and the served head still had to verify to be stored.
    pub fn read(origin: &str) -> Option<TreeHead> {
        let storage = web_sys::window()?.local_storage().ok()??;
        let text = storage.get_item(&key(origin)).ok()??;
        let (size, root) = text.split_once(':')?;
        Some(TreeHead {
            size: size.parse().ok()?,
            root: Hash::parse_hex(root).ok()?,
        })
    }

    /// Records `head` as the newest head this browser has verified. A failed write is
    /// swallowed: a full or blocked `localStorage` makes the archive empty next time,
    /// which reads as a first visit — not as tampering.
    pub fn write(origin: &str, head: &TreeHead) {
        if let Some(Ok(Some(storage))) = web_sys::window().map(|w| w.local_storage()) {
            let _ = storage.set_item(&key(origin), &render(head));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

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
