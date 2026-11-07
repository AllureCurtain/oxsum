//! End-user usage reports: the signed report bundle, the subset digest, and
//! the anchored-day completeness proof.

use doubleentry::{Balanced, Draft, Entry, Hash, InclusionProof, MerkleAccumulator, TreeHead};

use crate::SCALE;
use crate::head::{HeadError, PublishedKey, verify_signed_head};

// ── end-user usage reports (issue #261) ──────────────────────────────────────

/// What checking a usage report can fail with — one variant per distinct trust
/// failure, so a page names what is wrong instead of only that something is.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
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
pub(crate) fn hex_bytes<const N: usize>(hex: &str) -> Result<[u8; N], ()> {
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
}
