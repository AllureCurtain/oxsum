//! Version-2 evidence-bundle verification (issue #616, task T25).
//!
//! The v1 bill bundle proves one entry. A v2 bundle proves a whole committed
//! set: a marker entry binds a *manifest* — the sorted list of every
//! artifact's digest — and the bundle's job is to show the manifest is the
//! one the ledger committed and that every artifact it names is served,
//! untampered. This module is the verifier half of `oxsum_core::evidence`:
//! pure, I/O-free, wasm32-clean like the rest of the crate.
//!
//! The checks, in trust order:
//!
//! 1. `version` is `2` — the dispatch field `verify_bundle`'s legacy wire
//!    lacks. A v1 bundle belongs to [`crate::verify_bundle`], not here.
//! 2. The manifest re-canonicalizes from the bundle's own `kind`,
//!    `organizationId`, `scope` and `manifest` rows and digests to the
//!    committed `manifestDigest` — a bundle that claims a different manifest
//!    than it carries is caught before the ledger is asked.
//! 3. Every artifact the manifest names is present in `artifacts` and
//!    re-digests to the manifest's figure — a tampered or dropped committed
//!    row fails here, named.
//! 4. `entries[0]` — the marker — adopt-verifies to its recorded hash, its
//!    `DocumentRef` names `oxsum/evidence-bundle/v1` bound to the same
//!    manifest digest, and it includes in `head`: the manifest is the one
//!    the ledger committed, not a served claim.
//! 5. Every other entry adopt-verifies and includes — the money artifacts'
//!    ledger links are facts of the same log.
//! 6. On a signing deployment the head's note verifies under the published
//!    key — the same check `verify_signed_head` runs.

use doubleentry::{Balanced, Draft, Entry, Hash, InclusionProof, TreeHead};

use crate::{HeadError, PublishedKey, verify_signed_head};

/// The document id the bundle marker's `DocumentRef` must carry — the same
/// constant `oxsum_core` commits with, redeclared so the verifier pins the
/// byte string rather than importing the writer's crate.
const EVIDENCE_BUNDLE_DOCUMENT: &str = "oxsum/evidence-bundle/v1";

/// The domain the manifest digest folds under — the same constant
/// `oxsum_core` digests with.
const EVIDENCE_MANIFEST_DOMAIN: &[u8] = b"oxsum/evidence-manifest/v1";

/// The domain prefix one artifact's digest hashes under — the same prefix
/// `oxsum_core` digests with.
const EVIDENCE_ARTIFACT_DOMAIN: &[u8] = b"oxsum/evidence-artifact/v1/";

/// The digest of one artifact's canonical bytes — the same computation the
/// writer ran: `H(oxsum/evidence-artifact/v1/<kind>, canonical)`.
fn artifact_digest(kind: &str, canonical_json: &str) -> Hash {
    let mut domain = Vec::with_capacity(EVIDENCE_ARTIFACT_DOMAIN.len() + kind.len());
    domain.extend_from_slice(EVIDENCE_ARTIFACT_DOMAIN);
    domain.extend_from_slice(kind.as_bytes());
    Hash::digest(&domain, canonical_json.as_bytes())
}

/// What checking an evidence bundle can fail with — one variant per distinct
/// trust failure, so a reader names what is wrong instead of only that
/// something is.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvidenceError {
    /// The bundle, a hex field, or the canonical manifest could not be parsed.
    Malformed(String),
    /// `version` is absent or not `2`: this is a v1 bill bundle — it belongs
    /// to `verify_bundle`, not this verifier.
    WrongVersion,
    /// The `scope.covered` list does not carry every class the bundle claims
    /// to commit — the honest-scope block itself was edited.
    ScopeMismatch(String),
    /// The `manifest` rows re-canonicalized and re-digested do not equal the
    /// committed `manifestDigest` — the manifest was edited after mint.
    ManifestMismatch,
    /// An artifact the manifest names is missing from `artifacts`: the
    /// bundle dropped a committed row.
    MissingArtifact {
        /// The manifest row's `(kind, id)` pair.
        kind: String,
        /// Its id.
        id: String,
    },
    /// An artifact re-canonicalizes but digests differently than the
    /// manifest names — a tampered amount, rate, funding figure or customer
    /// field.
    ArtifactMismatch {
        /// The manifest row's `(kind, id)` pair.
        kind: String,
        /// Its id.
        id: String,
    },
    /// The marker's `DocumentRef` does not bind the manifest digest — the
    /// entry the bundle claims commits it is not the marker.
    MarkerMismatch,
    /// An entry's bytes do not re-hash to its recorded content hash — a
    /// tampered ledger row.
    EntryHash {
        /// Which `entries` position failed.
        index: usize,
    },
    /// An entry's inclusion proof does not verify against `head` — the row
    /// is not in the ledger the head attests.
    Inclusion {
        /// Which `entries` position failed.
        index: usize,
    },
    /// The head carries no `note`: an unsigned deployment's bundle verifies
    /// to the digests alone, which this variant reports rather than fails.
    Unsigned,
    /// The note names a different ledger than `expected_origin` — another
    /// tenant's head verifies as a signature but answers a different
    /// question.
    WrongOrigin,
    /// The note's attested head differs from `head`.
    HeadMismatch,
    /// The published key rejects the note's signature.
    BadSignature,
}

impl std::fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "the evidence bundle could not be parsed ({what})"),
            Self::WrongVersion => write!(
                f,
                "the bundle is not a version-2 evidence bundle — a v1 bill verifies with verify_bundle"
            ),
            Self::ScopeMismatch(what) => {
                write!(f, "the bundle's scope block is inconsistent ({what})")
            }
            Self::ManifestMismatch => write!(
                f,
                "the manifest does not digest to the committed manifestDigest"
            ),
            Self::MissingArtifact { kind, id } => {
                write!(
                    f,
                    "the manifest commits {kind}/{id} but the bundle drops it"
                )
            }
            Self::ArtifactMismatch { kind, id } => write!(
                f,
                "the artifact {kind}/{id} does not digest to what the manifest committed"
            ),
            Self::MarkerMismatch => write!(
                f,
                "the marker's DocumentRef does not bind the manifest digest"
            ),
            Self::EntryHash { index } => write!(
                f,
                "entry {index} does not hash to its recorded content hash"
            ),
            Self::Inclusion { index } => write!(
                f,
                "entry {index}'s inclusion proof does not verify against the head"
            ),
            Self::Unsigned => {
                write!(f, "the head carries no signature from the published key")
            }
            Self::WrongOrigin => write!(f, "the signed head names a different ledger"),
            Self::HeadMismatch => {
                write!(f, "the signed head does not attest the served head")
            }
            Self::BadSignature => write!(f, "the head's signature does not verify"),
        }
    }
}

impl std::error::Error for EvidenceError {}

/// The scope classes a v2 bundle must claim to cover — the verifier pins the
/// list so a bundle cannot quietly drop a class it is supposed to commit.
const EXPECTED_COVERED: &[&str] = &[
    "usageRecords",
    "planStatements",
    "invoiceJournals",
    "paymentOrders",
    "refunds",
    "disputes",
    "creditNotes",
    "daySeals",
    "ledgerEntries",
];

/// One manifest row on the wire.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEntryWire {
    kind: String,
    id: String,
    digest: String,
}

impl serde::Serialize for ManifestEntryWire {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ManifestEntry", 3)?;
        state.serialize_field("kind", &self.kind)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("digest", &self.digest)?;
        state.end()
    }
}

/// The scope block on the wire.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopeWire {
    covered: Vec<String>,
    excluded: Vec<String>,
    #[allow(dead_code)]
    upstream_cost_included: bool,
}

/// One `entries` element — the same `{entry, contentHash, proof}` shape a
/// usage-report row carries, plus the artifact link.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryWire {
    entry: Entry<Draft, { crate::SCALE }>,
    content_hash: String,
    proof: InclusionProof,
    #[allow(dead_code)]
    for_kind: String,
    #[allow(dead_code)]
    for_id: String,
}

impl EntryWire {
    /// Recomputes the entry's content hash and binds it to the recorded one —
    /// a tampered entry fails adoption before its proof is ever asked.
    fn adopt(&self, index: usize) -> Result<Entry<Balanced, { crate::SCALE }>, EvidenceError> {
        let recorded = Hash::parse_hex(&self.content_hash)
            .map_err(|_| EvidenceError::Malformed("a content hash is not hex".into()))?;
        self.entry
            .clone()
            .adopt_verified(recorded)
            .map_err(|_| EvidenceError::EntryHash { index })
    }
}

/// The whole bundle on the wire — every field the check binds, declared
/// wire-only so the verifier never imports `oxsum-core`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceWire {
    version: i64,
    kind: String,
    #[allow(dead_code)]
    bundle_id: String,
    organization_id: String,
    manifest_digest: String,
    #[serde(default)]
    #[allow(dead_code)]
    period_from: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    period_to: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    statement_id: Option<String>,
    #[allow(dead_code)]
    anchor_log_index: i64,
    scope: ScopeWire,
    artifacts: serde_json::Map<String, serde_json::Value>,
    manifest: Vec<ManifestEntryWire>,
    entries: Vec<EntryWire>,
    head: TreeHead,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
}

/// The canonical manifest re-serialized for digesting — the same struct
/// `oxsum_core` hashes, redeclared wire-only.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceManifestV1<'a> {
    v: i64,
    kind: &'a str,
    organization_id: &'a str,
    covered: &'a [String],
    excluded: &'a [String],
    entries: &'a [ManifestEntryWire],
}

/// Verifies a version-2 customer evidence bundle (issue #616).
///
/// `json` is the bundle body `GET /api/v1/org/evidence-bundles/{id}/download`
/// or `GET /api/v1/evidence/{token}` answers — the whole `data` object.
/// `expected_origin` is the tenant's ledger origin (`oxsum/ledgers/<tenant>`)
/// the signed head is expected to name. `expected_organization` is the
/// customer the bundle should be *for* — a bundle minted for another
/// organization verifies as a signature but answers a different question.
/// `key` is the published head key, like `verify_usage_report`'s: `None`
/// checks the digests and inclusion alone, which is what an unsigned
/// deployment leaves verifiable.
///
/// # Errors
///
/// The first check that fails, as an [`EvidenceError`]. A `note`-less head on
/// an unsigned deployment is `Ok`, not `Unsigned` — the signature layer
/// simply does not apply there, the same posture `verify_usage_report` takes.
pub fn verify_evidence_bundle(
    json: &str,
    expected_origin: &str,
    expected_organization: &str,
    key: Option<&PublishedKey<'_>>,
) -> Result<(), EvidenceError> {
    let wire: EvidenceWire =
        serde_json::from_str(json).map_err(|e| EvidenceError::Malformed(e.to_string()))?;

    if wire.version != 2 {
        return Err(EvidenceError::WrongVersion);
    }
    if wire.organization_id != expected_organization {
        return Err(EvidenceError::Malformed(format!(
            "the bundle is for {expected_organization:?} but commits {:?}",
            wire.organization_id
        )));
    }
    // The honest-scope block: the covered list must spell every class the
    // bundle claims, so an edited scope cannot shrink what it admits to.
    for class in EXPECTED_COVERED {
        if !wire.scope.covered.iter().any(|c| c == class) {
            return Err(EvidenceError::ScopeMismatch(format!(
                "the covered list does not name {class}"
            )));
        }
    }

    // The manifest re-canonicalizes from the bundle's own fields and must
    // digest to the committed figure — a manifest edited after mint fails
    // before any artifact is read.
    let manifest = EvidenceManifestV1 {
        v: 1,
        kind: &wire.kind,
        organization_id: &wire.organization_id,
        covered: &wire.scope.covered,
        excluded: &wire.scope.excluded,
        entries: &wire.manifest,
    };
    let canonical = serde_json::to_string(&manifest)
        .map_err(|e| EvidenceError::Malformed(format!("the canonical manifest: {e}")))?;
    if Hash::digest(EVIDENCE_MANIFEST_DOMAIN, canonical.as_bytes()).to_hex() != wire.manifest_digest
    {
        return Err(EvidenceError::ManifestMismatch);
    }

    // Every artifact the manifest names: present, and re-digesting to the
    // committed figure. The artifact map is `{kind: {id: json}}`.
    for entry in &wire.manifest {
        let Some(artifact) = wire
            .artifacts
            .get(&entry.kind)
            .and_then(|bucket| bucket.get(&entry.id))
        else {
            return Err(EvidenceError::MissingArtifact {
                kind: entry.kind.clone(),
                id: entry.id.clone(),
            });
        };
        let canonical = serde_json::to_string(artifact)
            .map_err(|e| EvidenceError::Malformed(format!("the canonical artifact: {e}")))?;
        if artifact_digest(&entry.kind, &canonical).to_hex() != entry.digest {
            return Err(EvidenceError::ArtifactMismatch {
                kind: entry.kind.clone(),
                id: entry.id.clone(),
            });
        }
    }

    // The marker first: adopt-verify it, then its DocumentRef must bind the
    // manifest digest — the bundle's claim that this manifest is the one the
    // ledger committed.
    let Some(marker_wire) = wire.entries.first() else {
        return Err(EvidenceError::Malformed(
            "the bundle carries no marker".into(),
        ));
    };
    let marker = marker_wire.adopt(0)?;
    let Some(document) = marker.document() else {
        return Err(EvidenceError::MarkerMismatch);
    };
    let bound = document
        .content_hash
        .map(|h| h.to_hex())
        .unwrap_or_default();
    if document.id.as_str() != EVIDENCE_BUNDLE_DOCUMENT || bound != wire.manifest_digest {
        return Err(EvidenceError::MarkerMismatch);
    }
    if !marker_wire.proof.verify(&marker.content_hash(), &wire.head) {
        return Err(EvidenceError::Inclusion { index: 0 });
    }

    // Every other entry: adopt-verify and include against the same head.
    for (index, row) in wire.entries.iter().enumerate().skip(1) {
        let entry = row.adopt(index)?;
        if !row.proof.verify(&entry.content_hash(), &wire.head) {
            return Err(EvidenceError::Inclusion { index });
        }
    }

    // V2 plan accounting explicitly names every ledger entry behind its
    // listed effects. Require those entries in the export, even if their proofs
    // were simply removed rather than altered. Older artifacts stay unchanged.
    if let Some(statements) = wire
        .artifacts
        .get("planStatement")
        .and_then(serde_json::Value::as_object)
    {
        for artifact in statements.values() {
            let Some(accounting) = artifact.get("accounting") else {
                continue;
            };
            if accounting
                .get("policyVersion")
                .and_then(serde_json::Value::as_i64)
                != Some(2)
            {
                return Err(EvidenceError::Malformed(
                    "unknown plan accounting policy".into(),
                ));
            }
            let effects = accounting
                .get("effects")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    EvidenceError::Malformed("daily accounting effects missing".into())
                })?;
            for effect in effects {
                if let Some(id) = effect.get("entry_id").and_then(serde_json::Value::as_str)
                    && !wire
                        .entries
                        .iter()
                        .skip(1)
                        .any(|row| row.entry.id().to_string() == id)
                {
                    return Err(EvidenceError::Malformed(format!(
                        "daily accounting ledger entry {id} missing"
                    )));
                }
            }
        }
    }

    // The head's signature, when the deployment signs heads — the same check
    // `verify_signed_head` runs, mapped into this bundle's vocabulary.
    let Some(note) = wire.note.as_deref() else {
        return Ok(());
    };
    let key = key.ok_or(EvidenceError::Unsigned)?;
    for (served, held, what) in [
        (wire.key_name.as_deref(), key.key_name, "key name"),
        (wire.public_key.as_deref(), key.public_key, "public key"),
        (wire.key_hash.as_deref(), key.key_hash, "key hash"),
    ] {
        if let Some(served) = served
            && served != held
        {
            return Err(EvidenceError::Malformed(format!(
                "the bundle's {what} does not match the published key"
            )));
        }
    }
    verify_signed_head(note, &wire.head, expected_origin, key).map_err(|e| match e {
        HeadError::Malformed(what) => EvidenceError::Malformed(what),
        HeadError::WrongOrigin => EvidenceError::WrongOrigin,
        HeadError::HeadMismatch => EvidenceError::HeadMismatch,
        HeadError::Unsigned => EvidenceError::Unsigned,
        HeadError::BadSignature => EvidenceError::BadSignature,
        HeadError::OldHeadMismatch | HeadError::Inconsistent => {
            EvidenceError::Malformed(e.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use doubleentry::witness::signing::SigningKey;
    use doubleentry::witness::{KeyName, Origin, SignedTreeHead};
    use doubleentry::{
        AccountId, AccountRegistry, Amount, Currency, Description, DocumentRef, Draft, Entry,
        EntryId, IdempotencyKey, LedgerPolicy, MerkleLog, PeriodCalendar, SealContext,
    };
    use time::macros::date;
    use uuid::Uuid;

    use super::{EvidenceError, verify_evidence_bundle};
    use crate::{Hash, PublishedKey, TreeHead};

    const SCALE: u8 = crate::SCALE;
    const ORIGIN: &str = "oxsum/ledgers/acme";
    const ORG: &str = "acme";
    /// The account the marker and the money entry both post against — the
    /// self-balancing `Equity:Adjustments` pattern `oxsum-core` uses for
    /// anchors, registered here so the fixture's seal agrees.
    const ADJUSTMENTS: &str = "Equity:Adjustments";

    /// The operator's key pair, published the way `GET /api/v1/log/head`
    /// serves it.
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
                KeyName::new("oxsum/tree-heads").expect("a valid name"),
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
            let mut note = SignedTreeHead::new(Origin::new(origin).expect("a valid origin"), head);
            let body = note.body();
            note.add_signature(self.signing.sign(&body));
            note.to_note()
        }
    }

    /// The seal context every fixture entry shares: one registered account, an
    /// open calendar, the default policy.
    struct Fixture {
        accounts: AccountRegistry,
        calendar: PeriodCalendar,
        policy: LedgerPolicy,
        adjustments: AccountId,
    }

    impl Fixture {
        fn new() -> Self {
            let mut accounts = AccountRegistry::new();
            let adjustments = accounts
                .register_path(ADJUSTMENTS, date!(2026 - 01 - 01))
                .expect("the account registers");
            Self {
                accounts,
                calendar: PeriodCalendar::new(),
                policy: LedgerPolicy::default(),
                adjustments,
            }
        }

        fn ctx(&self) -> SealContext<'_> {
            SealContext {
                accounts: &self.accounts,
                calendar: &self.calendar,
                policy: &self.policy,
            }
        }

        /// A self-balancing 1-minor entry, the marker-entry shape anchors use.
        /// `tag` makes each fixture entry distinct so the log's leaves differ.
        fn entry(
            &self,
            tag: u8,
            document: Option<DocumentRef>,
        ) -> Entry<doubleentry::Balanced, SCALE> {
            let mut draft = Entry::<Draft, SCALE>::new(
                EntryId::from_uuid(Uuid::from_bytes([tag; 16])),
                IdempotencyKey::new(vec![tag]).expect("a valid key"),
                date!(2026 - 03 - 15),
            )
            .with_description(Description::new("evidence fixture").expect("valid"))
            .debit(
                self.adjustments,
                Amount::<SCALE>::from_minor(1),
                Currency::USD,
            )
            .credit(
                self.adjustments,
                Amount::<SCALE>::from_minor(1),
                Currency::USD,
            );
            if let Some(document) = document {
                draft = draft.with_document(document);
            }
            draft
                .seal(&self.ctx())
                .expect("a self-balancing entry seals")
        }
    }

    /// A minted v2 bundle, built the way `oxsum-core` builds one: a manifest of
    /// artifact digests, a marker entry binding the manifest digest, one money
    /// entry, both appended to a log, the head signed.
    struct BuiltBundle {
        json: String,
    }

    fn artifact_digest(kind: &str, canonical_json: &str) -> Hash {
        let mut domain = Vec::new();
        domain.extend_from_slice(b"oxsum/evidence-artifact/v1/");
        domain.extend_from_slice(kind.as_bytes());
        Hash::digest(&domain, canonical_json.as_bytes())
    }

    /// Builds a genuine bundle. `artifacts` is the served map; the manifest is
    /// derived from it, so the genuine case always agrees. `tamper` lets a
    /// case mutate the serialized JSON after the fact.
    fn build(operator: &Operator, tamper: impl FnOnce(&mut serde_json::Value)) -> BuiltBundle {
        let fixture = Fixture::new();

        // Two artifacts: a usage record and a plan statement, each canonical
        // JSON the writer would have hashed.
        let usage = serde_json::json!({
            "v": 1,
            "id": "usage-1",
            "chargedMinor": 42_000_000i64,
            "costBasis": "price-book",
        });
        let statement = serde_json::json!({
            "v": 1,
            "id": "stmt-1",
            "totalMinor": 42_000_000i64,
        });
        let usage_canonical = serde_json::to_string(&usage).unwrap();
        let stmt_canonical = serde_json::to_string(&statement).unwrap();

        let covered: Vec<String> = [
            "usageRecords",
            "planStatements",
            "invoiceJournals",
            "paymentOrders",
            "refunds",
            "disputes",
            "creditNotes",
            "daySeals",
            "ledgerEntries",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let excluded: Vec<String> = [
            "upstreamCostFigures",
            "otherOrganizationsData",
            "historyOutsideScope",
            "inFlightOperations",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

        // The manifest digest: the writer's canonical struct ordering is
        // {v, kind, organizationId, covered, excluded, entries} with each
        // entry {kind, id, digest} in declaration order — which a
        // serde_json::json! Value does NOT reproduce (its objects serialize
        // alphabetically), so the fixture builds the manifest through typed
        // structs exactly like the writer and verifier do.
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ManifestEntryRow {
            kind: String,
            id: String,
            digest: String,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Manifest<'a> {
            v: i64,
            kind: &'a str,
            organization_id: &'a str,
            covered: &'a [String],
            excluded: &'a [String],
            entries: &'a [ManifestEntryRow],
        }
        let manifest_entries = vec![
            ManifestEntryRow {
                kind: "planStatements".to_owned(),
                id: "stmt-1".to_owned(),
                digest: artifact_digest("planStatements", &stmt_canonical).to_hex(),
            },
            ManifestEntryRow {
                kind: "usageRecords".to_owned(),
                id: "usage-1".to_owned(),
                digest: artifact_digest("usageRecords", &usage_canonical).to_hex(),
            },
        ];
        let manifest = Manifest {
            v: 1,
            kind: "period",
            organization_id: ORG,
            covered: &covered,
            excluded: &excluded,
            entries: &manifest_entries,
        };
        let manifest_digest = Hash::digest(
            b"oxsum/evidence-manifest/v1",
            serde_json::to_string(&manifest).unwrap().as_bytes(),
        );
        // The served `manifest` rows deserialize into ManifestEntryWire, whose
        // own Serialize writes kind/id/digest in order — so the served rows can
        // carry that same ordering and still re-digest.
        let manifest_rows = serde_json::to_value(&manifest_entries).unwrap();

        // The marker binds the manifest digest; the money entry is a plain
        // settled entry. Both go into the log; the head covers both.
        let marker = fixture.entry(
            1,
            Some(
                DocumentRef::new("oxsum/evidence-bundle/v1", manifest_digest)
                    .expect("a valid document ref"),
            ),
        );
        let money = fixture.entry(2, None);

        let mut log = MerkleLog::new();
        log.append(marker.content_hash());
        log.append(money.content_hash());
        let head = log.head();
        let marker_proof = log.inclusion_proof_at(0, head.size).unwrap();
        let money_proof = log.inclusion_proof_at(1, head.size).unwrap();

        let note = operator.sign(head, ORIGIN);
        let bundle_id = Uuid::from_bytes([0xab; 16]).to_string();
        let anchor_log_index = 0i64;

        let mut artifacts = serde_json::Map::new();
        artifacts.insert(
            "usageRecords".to_owned(),
            serde_json::json!({"usage-1": usage}),
        );
        artifacts.insert(
            "planStatements".to_owned(),
            serde_json::json!({"stmt-1": statement}),
        );

        let mut bundle = serde_json::json!({
            "version": 2,
            "kind": "period",
            "bundleId": bundle_id,
            "organizationId": ORG,
            "manifestDigest": manifest_digest.to_hex(),
            "periodFrom": "2026-03-01",
            "periodTo": "2026-03-14",
            "statementId": null,
            "anchorLogIndex": anchor_log_index,
            "scope": {
                "covered": covered,
                "excluded": excluded,
                "upstreamCostIncluded": false,
            },
            "artifacts": serde_json::Value::Object(artifacts),
            "manifest": manifest_rows,
            "entries": [
                {
                    "entry": serde_json::to_value(&marker).unwrap(),
                    "contentHash": marker.content_hash().to_hex(),
                    "proof": serde_json::to_value(&marker_proof).unwrap(),
                    "forKind": "bundle",
                    "forId": bundle_id,
                },
                {
                    "entry": serde_json::to_value(&money).unwrap(),
                    "contentHash": money.content_hash().to_hex(),
                    "proof": serde_json::to_value(&money_proof).unwrap(),
                    "forKind": "usageRecords",
                    "forId": "usage-1",
                },
            ],
            "head": serde_json::to_value(head).unwrap(),
            "note": note,
            "keyName": operator.key_name,
            "publicKey": operator.public_key,
            "keyHash": operator.key_hash,
        });
        tamper(&mut bundle);
        BuiltBundle {
            json: serde_json::to_string(&bundle).unwrap(),
        }
    }

    fn genuine(operator: &Operator) -> BuiltBundle {
        build(operator, |_| {})
    }

    #[test]
    fn a_genuine_bundle_verifies() {
        let operator = Operator::new();
        let bundle = genuine(&operator);
        assert_eq!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, Some(&operator.published())),
            Ok(())
        );
    }

    #[test]
    fn a_version_one_bundle_dispatches_away() {
        let operator = Operator::new();
        let bundle = build(&operator, |b| b["version"] = serde_json::json!(1));
        assert_eq!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, Some(&operator.published())),
            Err(EvidenceError::WrongVersion)
        );
    }

    #[test]
    fn a_tampered_amount_fails_the_artifact_digest() {
        let operator = Operator::new();
        let bundle = build(&operator, |b| {
            b["artifacts"]["usageRecords"]["usage-1"]["chargedMinor"] =
                serde_json::json!(43_000_000i64);
        });
        assert_eq!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, Some(&operator.published())),
            Err(EvidenceError::ArtifactMismatch {
                kind: "usageRecords".to_owned(),
                id: "usage-1".to_owned(),
            })
        );
    }

    #[test]
    fn a_missing_committed_row_fails_as_missing() {
        let operator = Operator::new();
        let bundle = build(&operator, |b| {
            b["artifacts"]["planStatements"]
                .as_object_mut()
                .unwrap()
                .remove("stmt-1");
        });
        assert_eq!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, Some(&operator.published())),
            Err(EvidenceError::MissingArtifact {
                kind: "planStatements".to_owned(),
                id: "stmt-1".to_owned(),
            })
        );
    }

    #[test]
    fn an_edited_manifest_fails_the_manifest_digest() {
        let operator = Operator::new();
        let bundle = build(&operator, |b| {
            // Flip a committed digest digit: the manifest no longer digests to
            // manifestDigest.
            b["manifest"][0]["digest"] = serde_json::json!("0".repeat(64));
        });
        assert_eq!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, Some(&operator.published())),
            Err(EvidenceError::ManifestMismatch)
        );
    }

    #[test]
    fn an_edited_scope_fails_the_scope_check() {
        let operator = Operator::new();
        let bundle = build(&operator, |b| {
            b["scope"]["covered"] = serde_json::json!(["usageRecords"]);
        });
        assert!(matches!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, Some(&operator.published())),
            Err(EvidenceError::ScopeMismatch(_))
        ));
    }

    #[test]
    fn a_bundle_for_another_customer_fails_closed() {
        let operator = Operator::new();
        let bundle = genuine(&operator);
        assert!(matches!(
            verify_evidence_bundle(
                &bundle.json,
                ORIGIN,
                "not-acme",
                Some(&operator.published())
            ),
            Err(EvidenceError::Malformed(_))
        ));
    }

    #[test]
    fn a_wrong_origin_is_a_different_ledger() {
        let operator = Operator::new();
        let bundle = genuine(&operator);
        assert_eq!(
            verify_evidence_bundle(
                &bundle.json,
                "oxsum/ledgers/other-tenant",
                ORG,
                Some(&operator.published())
            ),
            Err(EvidenceError::WrongOrigin)
        );
    }

    #[test]
    fn a_wrong_key_does_not_match_the_served_key_fields() {
        let operator = Operator::new();
        let attacker = Operator::from_seed([3; 32]);
        let bundle = genuine(&operator);
        // The bundle serves the operator's key fields; held up to the
        // attacker's key they disagree before the signature is even checked.
        assert!(matches!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, Some(&attacker.published())),
            Err(EvidenceError::Malformed(_))
        ));
    }

    #[test]
    fn an_unsigned_head_verifies_to_the_digests_alone() {
        let operator = Operator::new();
        // Strip the signature fields: the posture an unsigned deployment
        // serves. The digests and inclusion still bind.
        let bundle = build(&operator, |b| {
            b["note"] = serde_json::Value::Null;
            b["keyName"] = serde_json::Value::Null;
            b["publicKey"] = serde_json::Value::Null;
            b["keyHash"] = serde_json::Value::Null;
        });
        assert_eq!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, None),
            Ok(())
        );
    }

    #[test]
    fn a_tampered_entry_fails_its_content_hash() {
        let operator = Operator::new();
        // Corrupt the money entry's bytes so adopt_verified rejects it.
        let bundle = build(&operator, |b| {
            b["entries"][1]["entry"]["description"] = serde_json::json!("forged");
        });
        assert_eq!(
            verify_evidence_bundle(&bundle.json, ORIGIN, ORG, Some(&operator.published())),
            Err(EvidenceError::EntryHash { index: 1 })
        );
    }
}
